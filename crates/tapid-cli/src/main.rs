mod commands;
mod context;
mod online;
mod package_spec;
#[allow(dead_code)]
mod run;

use clap::Parser;
use commands::{Command, LockCommand, ManifestCommand};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tapid_archive::{ArchiveFormat, ArchiveLimits, extract_to};
use tapid_core::{ArtifactDigest, PackageInstanceId};
use tapid_linker::{
    DependencyEdge, InstanceKey, LayoutInput, ManagedRoot, PackageInstance, Platform,
    VerifiedTreeReference, plan_layout,
};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;
use tapid_release_client::{
    Error as ReleaseError, Fetcher, ReleaseState, accept_release, read_release_state,
    write_release_state,
};
use tapid_signatures::KeyRing;
use tapid_store::Store;

#[derive(Debug, Parser)]
#[command(
    name = "tapid",
    version,
    about = "A deterministic JavaScript and TypeScript package manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

fn main() -> ExitCode {
    dispatch(Cli::parse())
}
fn dispatch(cli: Cli) -> ExitCode {
    match cli.command {
        None => {
            println!("Run 'tapid --help' for usage");
            ExitCode::SUCCESS
        }
        Some(Command::Init { path }) => init(path.as_deref()),
        Some(Command::Manifest {
            command: ManifestCommand::Validate { path },
        }) => validate(path.as_deref().unwrap_or(Path::new("package.json"))),
        Some(Command::Lock {
            command: LockCommand::Verify,
        }) => verify_lock(Path::new("tapid.lock")),
        Some(Command::Run {
            script,
            project_dir,
            arguments,
        }) => run_script(&project_dir, &script, arguments),
        Some(Command::Upgrade {
            endpoints,
            keyring,
            destination,
            dry_run,
        }) => upgrade(
            &endpoints,
            keyring.as_deref(),
            destination.as_deref(),
            dry_run,
        ),
        Some(Command::Install {
            package,
            offline,
            frozen,
            allow_unverified_registry_artifacts,
            project_dir,
            store_dir,
            registry_fixture,
        }) => install(
            &project_dir,
            package.as_deref(),
            store_dir.as_deref(),
            offline,
            frozen,
            registry_fixture.as_deref(),
            allow_unverified_registry_artifacts,
        ),
    }
}

const DEFAULT_STABLE_ENDPOINTS: [&str; 2] = [
    "https://tapid.dev/stable.json",
    "https://github.com/LimeTip/tapid/releases/latest/download/stable.json",
];

fn stable_discovery_endpoints(endpoint_args: &[String], env_value: Option<&str>) -> Vec<String> {
    if !endpoint_args.is_empty() {
        return endpoint_args.to_vec();
    }
    if let Some(value) = env_value {
        let endpoints: Vec<String> = value
            .split(',')
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(str::to_owned)
            .collect();
        if !endpoints.is_empty() {
            return endpoints;
        }
    }
    DEFAULT_STABLE_ENDPOINTS
        .iter()
        .map(|endpoint| (*endpoint).to_owned())
        .collect()
}

fn upgrade(
    endpoint_args: &[String],
    keyring_arg: Option<&Path>,
    destination_arg: Option<&Path>,
    dry_run: bool,
) -> ExitCode {
    let keyring = match keyring_arg
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TAPID_RELEASE_KEYRING").map(PathBuf::from))
    {
        Some(keyring_path) => {
            let keyring_bytes = match fs::read(&keyring_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!(
                        "error: cannot read trusted release keyring '{}': {error}",
                        keyring_path.display()
                    );
                    return ExitCode::from(1);
                }
            };
            match KeyRing::from_embedded_json(&keyring_bytes) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("error: invalid trusted release keyring: {error}");
                    return ExitCode::from(1);
                }
            }
        }
        None => match KeyRing::production() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: embedded production release keyring is invalid: {error}");
                return ExitCode::from(1);
            }
        },
    };
    let endpoints = stable_discovery_endpoints(
        endpoint_args,
        std::env::var("TAPID_STABLE_ENDPOINTS").ok().as_deref(),
    );
    let destination = match destination_arg {
        Some(path) => path.to_owned(),
        None => match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: cannot determine current executable: {error}");
                return ExitCode::from(1);
            }
        },
    };
    if let Err(error) = validate_upgrade_destination(&destination) {
        eprintln!("error: {error}");
        return ExitCode::from(1);
    }
    let target = release_target();
    let mut fetcher = CurlFetcher;
    let (manifest_version, artifact_name, bytes) =
        match fetch_verified_release(&mut fetcher, &endpoints, &keyring, target) {
            Ok(value) => {
                let digest = format!("{:x}", Sha256::digest(&value.1));
                let state_path = release_state_path(&destination);
                let state = match read_release_state(&state_path) {
                    Ok(previous) => accept_release(
                        &previous,
                        &value.0.version,
                        previous.release_sequence.saturating_add(1),
                        digest.clone(),
                    ),
                    Err(_) if !state_path.exists() => {
                        ReleaseState::new(&value.0.version, 1, digest.clone())
                    }
                    Err(error) => Err(error),
                };
                let state = match state {
                    Ok(state) => state,
                    Err(error) => {
                        eprintln!("error: cannot accept verified release state: {error}");
                        return ExitCode::from(1);
                    }
                };
                if let Err(error) =
                    write_cached_artifact(&destination, &digest, &value.1).and_then(|_| {
                        write_release_state(&state_path, &state).map_err(|e| e.to_string())
                    })
                {
                    eprintln!("error: cannot persist verified release state: {error}");
                    return ExitCode::from(1);
                }
                let name = value
                    .0
                    .artifact()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                (value.0.version, name, value.1)
            }
            Err(ReleaseError::AllEndpointsFailed { .. }) => {
                match recover_last_known_good(&destination, target) {
                    Ok(value) => (value.0, value.1, value.2),
                    Err(recovery) => {
                        eprintln!(
                            "error: stable discovery unavailable and recovery failed: {recovery}"
                        );
                        return ExitCode::from(1);
                    }
                }
            }
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
    let executable = match materialize_artifact(&artifact_name, &bytes) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    if dry_run {
        println!(
            "Verified stable Tapid {} for {target}; dry-run did not replace {}",
            manifest_version,
            destination.display()
        );
        return ExitCode::SUCCESS;
    }
    match replace_executable(&destination, &executable) {
        Ok(()) => {
            println!("Upgraded Tapid to {}", manifest_version);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: cannot activate verified artifact: {error}");
            ExitCode::from(1)
        }
    }
}

fn release_state_path(destination: &Path) -> PathBuf {
    destination
        .parent()
        .unwrap_or(Path::new("."))
        .join(".tapid-release-state.json")
}

fn cached_artifact_path(destination: &Path, digest: &str) -> PathBuf {
    destination
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(".tapid-release-artifact-{digest}"))
}

fn write_cached_artifact(destination: &Path, digest: &str, bytes: &[u8]) -> Result<(), String> {
    let path = cached_artifact_path(destination, digest);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err("cached artifact must be a regular file".into());
        }
        return Ok(());
    }
    let temp = path.with_file_name(format!(".tapid-release-artifact-tmp-{}", unique_nonce()));
    fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&temp, &path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        e.to_string()
    })
}

fn recover_last_known_good(
    destination: &Path,
    target: &str,
) -> Result<(String, String, Vec<u8>), String> {
    let state = read_release_state(&release_state_path(destination))
        .map_err(|e| format!("invalid or missing last-known-good state: {e}"))?;
    let path = cached_artifact_path(destination, &state.last_known_good.artifact_sha256);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot inspect cached verified artifact: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err("cached artifact must be a regular file".into());
    }
    let bytes =
        fs::read(&path).map_err(|e| format!("cannot read cached verified artifact: {e}"))?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != state.last_known_good.artifact_sha256 {
        return Err("cached artifact digest does not match last-known-good state".into());
    }
    let name = format!("tapid-{}-{target}.tar.gz", state.last_known_good.version);
    Ok((state.last_known_good.version, name, bytes))
}

fn fetch_verified_release<F: Fetcher>(
    fetcher: &mut F,
    endpoints: &[String],
    keyring: &KeyRing,
    target: &str,
) -> Result<(tapid_release_client::ReleaseManifest, Vec<u8>), ReleaseError> {
    let endpoint_refs: Vec<&str> = endpoints.iter().map(String::as_str).collect();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| ReleaseError::State(format!("cannot determine current time: {e}")))?;
    let manifest = tapid_release_client::discover(
        fetcher,
        &endpoint_refs,
        keyring,
        target,
        &now,
        Some(std::time::Duration::from_secs(7 * 24 * 60 * 60)),
    )?;
    let artifact = manifest
        .artifact()
        .ok_or_else(|| ReleaseError::TargetNotFound(target.into()))?;
    let bytes = fetcher.fetch(&artifact.url).map_err(ReleaseError::Fetch)?;
    manifest.verify_artifact(&bytes)?;
    Ok((manifest, bytes))
}

#[cfg(test)]
mod upgrade_tests {
    use super::{DEFAULT_STABLE_ENDPOINTS, materialize_artifact, stable_discovery_endpoints};
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp(label: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("tapid-upgrade-{label}-{n}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn default_stable_endpoints_are_ordered_with_tapid_dev_first() {
        let endpoints = stable_discovery_endpoints(&[], None);
        assert_eq!(endpoints, DEFAULT_STABLE_ENDPOINTS);
    }

    #[test]
    fn stable_endpoint_overrides_preserve_cli_then_environment_precedence() {
        let cli = vec!["https://cli.test/stable.json".to_owned()];
        assert_eq!(
            stable_discovery_endpoints(&cli, Some("https://env.test/stable.json")),
            cli
        );
        assert_eq!(
            stable_discovery_endpoints(&[], Some(" https://one.test/a, ,https://two.test/b ")),
            vec![
                "https://one.test/a".to_owned(),
                "https://two.test/b".to_owned()
            ]
        );
        assert_eq!(
            stable_discovery_endpoints(&[], Some(" , ")),
            DEFAULT_STABLE_ENDPOINTS
        );
    }

    fn tar_gz(root: &std::path::Path, entries: &[&str]) -> Vec<u8> {
        let archive = root.join("artifact.tar.gz");
        let status = Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .args(["-C"])
            .arg(root)
            .args(entries)
            .status()
            .unwrap();
        assert!(status.success());
        fs::read(archive).unwrap()
    }

    #[test]
    fn tar_artifact_rejects_extra_members_before_selecting_tapid() {
        let root = temp("extra");
        fs::write(root.join("tapid"), b"new").unwrap();
        fs::write(root.join("extra"), b"unexpected").unwrap();
        let bytes = tar_gz(&root, &["tapid", "extra"]);
        let result = materialize_artifact("tapid.tar.gz", &bytes);
        assert!(result.is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tar_artifact_rejects_symlink_members() {
        let root = temp("symlink");
        fs::write(root.join("payload"), b"payload").unwrap();
        std::os::unix::fs::symlink("payload", root.join("tapid")).unwrap();
        let bytes = tar_gz(&root, &["tapid", "payload"]);
        assert!(materialize_artifact("tapid.tar.gz", &bytes).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tar_artifact_rejects_duplicate_members() {
        let root = temp("duplicate");
        let plain = root.join("artifact.tar");
        fs::write(root.join("tapid"), b"new").unwrap();
        assert!(
            Command::new("tar")
                .args(["-cf"])
                .arg(&plain)
                .args(["-C"])
                .arg(&root)
                .arg("tapid")
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("tar")
                .args(["-rf"])
                .arg(&plain)
                .args(["-C"])
                .arg(&root)
                .arg("tapid")
                .status()
                .unwrap()
                .success()
        );
        let output = Command::new("gzip")
            .args(["-c"])
            .arg(&plain)
            .output()
            .unwrap();
        let result = materialize_artifact("tapid.tar.gz", &output.stdout);
        assert!(result.is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tar_artifact_rejects_traversal_members() {
        let root = temp("traversal");
        let outside = root.parent().unwrap().join(format!(
            "tapid-outside-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&outside, b"outside").unwrap();
        let archive = root.join("artifact.tar.gz");
        let output = Command::new("python3")
            .args([
                "-c",
                "import tarfile,sys; t=tarfile.open(sys.argv[1],'w:gz'); i=tarfile.TarInfo('../tapid'); i.size=3; t.addfile(i, __import__('io').BytesIO(b'new')); t.close()",
                archive.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(materialize_artifact("tapid.tar.gz", &fs::read(&archive).unwrap()).is_err());
        let _ = fs::remove_file(outside);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_artifact_is_rejected_without_unsafe_extraction() {
        assert!(materialize_artifact("tapid.zip", b"not a zip").is_err());
    }
}

fn unique_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

fn materialize_artifact(name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if name.ends_with(".zip") {
        return Err(
            "verified Windows zip artifacts are unsupported: refusing unsafe extraction".into(),
        );
    }
    if !name.ends_with(".tar.gz") {
        return Err("verified artifact has an unsupported archive format".into());
    }
    let temp = std::env::temp_dir().join(format!(
        "tapid-artifact-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    let result = (|| {
        extract_to(bytes, ArchiveFormat::TarGz, &temp, ArchiveLimits::default())
            .map_err(|e| format!("cannot safely extract verified artifact: {e}"))?;
        let mut entries = fs::read_dir(&temp).map_err(|e| e.to_string())?;
        let first = entries.next().transpose().map_err(|e| e.to_string())?;
        let Some(first) = first else {
            return Err("verified artifact does not contain a tapid executable".into());
        };
        if entries.next().is_some() {
            return Err("verified artifact must contain exactly one member named tapid".into());
        }
        let path = first.path();
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        if name != "tapid" && name != "tapid.exe" {
            return Err("verified artifact must contain exactly one member named tapid".into());
        }
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_file() {
            return Err("verified artifact tapid member must be a regular file".into());
        }
        fs::read(path).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

fn validate_upgrade_destination(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "cannot inspect upgrade destination '{}': {e}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err("upgrade destination must be a regular file".into());
    }
    let marker = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".tapid-managed");
    let marker_metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Err(format!(
                "refusing to replace unmarked non-Tapid-managed destination '{}'; expected {}",
                path.display(),
                marker.display()
            ));
        }
    };
    if !marker_metadata.file_type().is_file() {
        return Err("Tapid ownership marker must be a regular file".into());
    }
    if fs::read(&marker).map_or(true, |bytes| bytes != b"tapid-managed-v1\n") {
        return Err(format!(
            "refusing to replace unmarked non-Tapid-managed destination '{}'; expected {}",
            path.display(),
            marker.display()
        ));
    }
    Ok(())
}

fn replace_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_file_name(format!(
        ".tapid-upgrade-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        if let Err(error) = fs::set_permissions(&temp, fs::Permissions::from_mode(mode)) {
            let _ = fs::remove_file(&temp);
            return Err(error.to_string());
        }
        fs::rename(&temp, path).map_err(|e| {
            let _ = fs::remove_file(&temp);
            e.to_string()
        })
    }
    #[cfg(windows)]
    {
        let backup = path.with_file_name(format!(
            ".tapid-old-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        fs::rename(path, &backup).map_err(|e| {
            let _ = fs::remove_file(&temp);
            e.to_string()
        })?;
        match fs::rename(&temp, path) {
            Ok(()) => {
                let _ = fs::remove_file(&backup);
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(path);
                let restore = fs::rename(&backup, path);
                if let Err(restore_error) = restore {
                    return Err(format!(
                        "{error}; could not restore old executable: {restore_error}"
                    ));
                }
                let _ = fs::remove_file(&temp);
                Err(error.to_string())
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fs::remove_file(&temp);
        Err("executable replacement is unsupported on this operating system".into())
    }
}

struct CurlFetcher;
impl tapid_release_client::Fetcher for CurlFetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String> {
        if !url.starts_with("https://") {
            return Err("URL must use HTTPS".into());
        }
        let output = std::process::Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--tlsv1.2",
                url,
            ])
            .output()
            .map_err(|e| format!("HTTPS transport unavailable: {e}"))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }
}

fn release_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported-target"
    }
}

fn run_script(project_dir: &Path, script_name: &str, arguments: Vec<String>) -> ExitCode {
    let project_dir = match fs::canonicalize(project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            eprintln!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                project_dir.display()
            );
            return ExitCode::from(1);
        }
    };
    let manifest = match read_manifest(&project_dir.join("package.json")) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let Some(script) = manifest.scripts().get(script_name).cloned() else {
        eprintln!("error: root package script is missing: {script_name}");
        return ExitCode::from(1);
    };
    match run::execute(run::RunRequest::new(project_dir, Some(script)).with_arguments(arguments)) {
        Ok(result) => result
            .exit_code()
            .map_or(ExitCode::from(1), |code| ExitCode::from(code as u8)),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

struct ManifestTransaction {
    path: PathBuf,
    original: Vec<u8>,
    committed: bool,
}

impl ManifestTransaction {
    fn begin(path: &Path) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() {
            return Err("package.json must be a regular file".to_owned());
        }
        Ok(Self {
            path: path.to_owned(),
            original: fs::read(path).map_err(|error| error.to_string())?,
            committed: false,
        })
    }

    fn write(&self, contents: &str) -> Result<(), String> {
        fs::write(&self.path, contents).map_err(|error| error.to_string())
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ManifestTransaction {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::write(&self.path, &self.original);
        }
    }
}

fn install(
    project_dir: &Path,
    package: Option<&str>,
    store_root: Option<&Path>,
    offline: bool,
    frozen: bool,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
) -> ExitCode {
    if package.is_some() && (offline || frozen) {
        eprintln!("error: a package argument cannot be used with --offline or --frozen");
        return ExitCode::from(1);
    }
    if allow_unverified_registry_artifacts && (offline || frozen) {
        eprintln!(
            "error: --allow-unverified-registry-artifacts cannot be used with --offline or --frozen"
        );
        return ExitCode::from(1);
    }
    if allow_unverified_registry_artifacts {
        eprintln!(
            "warning: npm artifacts without registry integrity are not authenticated against a registry-declared digest"
        );
    }
    let project_dir = match fs::canonicalize(project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            eprintln!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                project_dir.display()
            );
            return ExitCode::from(1);
        }
    };
    let manifest = match read_manifest(&project_dir.join("package.json")) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let manifest_path = project_dir.join("package.json");
    let mut manifest_transaction = None;
    let manifest = if let Some(spec) = package {
        let (name, requirement) = package_spec::parse(spec);
        let updated = match manifest.with_dependency(name, requirement) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: cannot add dependency '{spec}': {error}");
                return ExitCode::from(1);
            }
        };
        let transaction = match ManifestTransaction::begin(&manifest_path) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: cannot prepare package.json update: {error}");
                return ExitCode::from(1);
            }
        };
        if let Err(error) = transaction.write(&updated.to_json()) {
            eprintln!("error: cannot update package.json: {error}");
            return ExitCode::from(1);
        }
        manifest_transaction = Some(transaction);
        updated
    } else {
        manifest
    };
    let lock_path = project_dir.join("tapid.lock");
    if !offline && !frozen {
        let store = Store::new(
            store_root
                .map(PathBuf::from)
                .unwrap_or_else(|| project_dir.join(".tapid-store")),
        );
        let (lock, input, trees) = match online::resolve_and_fetch(
            &project_dir,
            &manifest,
            &store,
            registry_fixture,
            allow_unverified_registry_artifacts,
        ) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        let lock_json = match lock.to_json() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: cannot serialize lockfile: {error}");
                return ExitCode::from(1);
            }
        };
        let lock_backup = match replace_lockfile(&lock_path, &lock_json) {
            Ok(value) => value,
            Err(error) => {
                eprintln!(
                    "error: cannot replace lockfile {}: {error}",
                    lock_path.display()
                );
                return ExitCode::from(1);
            }
        };
        let result = materialize_install(&project_dir, &lock, input, trees);
        if result != ExitCode::SUCCESS {
            let _ = rollback_lockfile(&lock_path, lock_backup.as_deref());
            return result;
        }
        let _ = discard_lockfile_backup(lock_backup.as_deref());
        if let Some(transaction) = manifest_transaction.take() {
            transaction.commit();
        }
        return result;
    }
    if !lock_path.is_file() {
        let message = if offline {
            "offline install requires tapid.lock"
        } else if frozen {
            "frozen install requires tapid.lock"
        } else {
            "cannot install without tapid.lock; network resolution is unavailable"
        };
        eprintln!("error: {message}: {}", lock_path.display());
        return ExitCode::from(1);
    }
    let lock = match fs::read_to_string(&lock_path)
        .map_err(|e| e.to_string())
        .and_then(|s| Lockfile::from_json(&s).map_err(|e| e.to_string()))
    {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: invalid lockfile {}: {error}", lock_path.display());
            return ExitCode::from(1);
        }
    };
    let current_manifest_digest = match fs::read(project_dir.join("package.json")) {
        Ok(bytes) => digest_bytes(&bytes),
        Err(error) => {
            eprintln!("error: cannot read root manifest for lockfile replay: {error}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = lock.validate_replay(&current_manifest_digest) {
        eprintln!("error: invalid lockfile {}: {error}", lock_path.display());
        return ExitCode::from(1);
    }
    let store = Store::new(
        store_root
            .map(PathBuf::from)
            .unwrap_or_else(|| project_dir.join(".tapid-store")),
    );
    let (input, trees) = match replay_input(&lock, &manifest, &store) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    materialize_with_lock(&project_dir, &lock, input, trees, true)
}

fn materialize_install(
    project_dir: &Path,
    lock: &Lockfile,
    input: LayoutInput,
    trees: BTreeMap<String, PathBuf>,
) -> ExitCode {
    materialize_with_lock(project_dir, lock, input, trees, false)
}

fn materialize_with_lock(
    project_dir: &Path,
    lock: &Lockfile,
    input: LayoutInput,
    trees: BTreeMap<String, PathBuf>,
    replayed: bool,
) -> ExitCode {
    let root = match ManagedRoot::new(project_dir) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                cleanup_replay_snapshots(&trees);
            }
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let platform = current_platform();
    let plan = match plan_layout(root, input.clone(), platform) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                cleanup_replay_snapshots(&trees);
            }
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let stage = project_dir.join(format!(
        ".tapid-install-stage-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    if let Err(error) = fs::create_dir(&stage) {
        if replayed {
            cleanup_replay_snapshots(&trees);
        }
        eprintln!("error: cannot create install staging directory: {error}");
        return ExitCode::from(1);
    }
    let result = materialize_stage(&stage, &plan, &input, &trees)
        .and_then(|_| activate_node_modules(project_dir, &stage));
    if replayed {
        cleanup_replay_snapshots(&trees);
    }
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&stage);
        eprintln!("error: {error}");
        return ExitCode::from(1);
    }
    if replayed {
        println!("Replayed lockfile: {} package(s)", lock.packages().len());
    } else {
        println!("Installed {} package(s)", lock.packages().len());
    }
    ExitCode::SUCCESS
}

fn cleanup_replay_snapshots(trees: &BTreeMap<String, PathBuf>) {
    for tree in trees.values() {
        let _ = fs::remove_dir_all(tree);
    }
}

struct ReplaySnapshotGuard {
    paths: Vec<PathBuf>,
    keep: bool,
}

impl Drop for ReplaySnapshotGuard {
    fn drop(&mut self) {
        if !self.keep {
            for path in &self.paths {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}

fn replay_input(
    lock: &Lockfile,
    manifest: &PackageManifest,
    store: &Store,
) -> Result<(LayoutInput, BTreeMap<String, PathBuf>), String> {
    let mut instances = Vec::new();
    let mut keys = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut snapshots = ReplaySnapshotGuard {
        paths: Vec::new(),
        keep: false,
    };
    let typed_packages = lock.packages_typed().map_err(|e| e.to_string())?;
    for (key, package) in &typed_packages {
        let encoded = key.to_string();
        let digest: ArtifactDigest = package
            .tree_digest()
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
        let tree = store
            .verified_tree_snapshot(&digest)
            .map_err(|e| format!("package {encoded} tree unavailable: {e}"))?;
        snapshots.paths.push(tree.clone());
        let peer = context::parse_peer(&key.peer_context)?;
        let platform = context::parse_platform(&key.platform_context)?;
        let id = PackageInstanceId::new(key.registry.clone(), key.name.clone(), key.version);
        let instance = PackageInstance {
            id,
            peer_context: peer,
            platform_context: platform,
            tree: VerifiedTreeReference::new(package.tree_digest(), &tree)
                .map_err(|e| e.to_string())?,
        };
        keys.insert(encoded.clone(), InstanceKey::from(&instance));
        trees.insert(encoded, tree);
        instances.push(instance);
    }
    let mut roots = Vec::new();
    let root_identities = replay_root_identities(manifest)?;
    let has_root_dependencies = !root_identities.is_empty();
    for (key, _) in &typed_packages {
        let encoded = key.to_string();
        if !has_root_dependencies || replay_root_matches(&root_identities, key) {
            roots.push(keys[&encoded].clone());
        }
    }
    let mut edges = Vec::new();
    for (key, package) in &typed_packages {
        let encoded = key.to_string();
        for dependency in package.dependencies().values() {
            edges.push(DependencyEdge {
                parent: keys[&encoded].clone(),
                child: keys
                    .get(dependency)
                    .cloned()
                    .ok_or_else(|| format!("missing dependency target {dependency}"))?,
            });
        }
    }
    snapshots.keep = true;
    Ok((
        LayoutInput {
            instances,
            root_dependencies: roots,
            dependency_edges: edges,
        },
        trees,
    ))
}

fn replay_root_identities(
    manifest: &PackageManifest,
) -> Result<std::collections::BTreeSet<(tapid_core::RegistryOrigin, tapid_core::PackageName)>, String>
{
    let mut identities = std::collections::BTreeSet::new();
    for map in [
        manifest.dependencies(),
        manifest.dev_dependencies(),
        manifest.optional_dependencies(),
    ] {
        for name in map.keys() {
            let (registry, package) = online::dep_parts(name)?;
            identities.insert((registry, package));
        }
    }
    Ok(identities)
}

fn replay_root_matches(
    roots: &std::collections::BTreeSet<(tapid_core::RegistryOrigin, tapid_core::PackageName)>,
    key: &tapid_lockfile::LockfilePackageKey,
) -> bool {
    roots.contains(&(key.registry.clone(), key.name.clone()))
}

fn current_platform() -> Platform {
    if cfg!(target_family = "windows") {
        Platform::Windows
    } else if cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    )) {
        Platform::Unix
    } else {
        Platform::Other
    }
}

fn materialize_stage(
    stage: &Path,
    plan: &tapid_linker::MaterializationPlan,
    input: &LayoutInput,
    trees: &BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    fs::create_dir_all(stage.join("node_modules")).map_err(|e| e.to_string())?;
    let mut by_source = BTreeMap::new();
    for entry in &plan.entries {
        let tree = trees
            .values()
            .find(|path| **path == entry.source)
            .ok_or_else(|| "tree replay mapping lost".to_owned())?;
        let expected = input
            .instances
            .iter()
            .find(|instance| instance.tree.root == *tree)
            .map(|instance| instance.tree.digest.as_str())
            .ok_or_else(|| "tree replay digest mapping lost".to_owned())?;
        let actual = tapid_archive::canonical_tree_digest(tree).map_err(|e| e.to_string())?;
        if actual != expected {
            return Err(format!(
                "tree changed during replay: expected {expected}, got {actual}"
            ));
        }
        let target = stage.join(
            entry
                .target
                .strip_prefix(&plan.managed_root.path)
                .map_err(|_| "invalid stage target")?,
        );
        copy_tree(tree, &target)?;
        let after = tapid_archive::canonical_tree_digest(tree).map_err(|e| e.to_string())?;
        if after != expected {
            return Err(format!(
                "tree changed while replaying: expected {expected}, got {after}"
            ));
        }
        by_source.insert(entry.target.clone(), target);
    }
    for step in &plan.activation.steps {
        let source = by_source
            .get(&step.source)
            .ok_or_else(|| "activation source missing".to_owned())?;
        let target = stage.join(
            step.target
                .strip_prefix(&plan.managed_root.path)
                .map_err(|_| "invalid activation target")?,
        );
        copy_tree(source, &target)?;
    }
    materialize_package_shims(stage, plan)?;
    Ok(())
}

fn package_root_for_shims(package_dir: &Path) -> PathBuf {
    let nested = package_dir.join("package");
    if !package_dir.join("package.json").is_file() && nested.join("package.json").is_file() {
        nested
    } else {
        package_dir.to_path_buf()
    }
}

fn materialize_package_shims(
    stage: &Path,
    plan: &tapid_linker::MaterializationPlan,
) -> Result<(), String> {
    let managed = ManagedRoot::new(stage.join("node_modules")).map_err(|e| e.to_string())?;
    let mut packages = Vec::new();
    for step in &plan.activation.steps {
        let package_dir = stage.join("node_modules").join(
            step.target
                .strip_prefix(plan.managed_root.path.join("node_modules"))
                .map_err(|_| "invalid package activation target")?,
        );
        let package_root = package_root_for_shims(&package_dir);
        let package_json = fs::read_to_string(package_root.join("package.json"))
            .map_err(|e| format!("cannot read installed package manifest: {e}"))?;
        packages.push(tapid_linker::ShimPackage {
            tree_root: package_root,
            package_json,
            bin_dir: package_dir
                .parent()
                .ok_or_else(|| "installed package has no node_modules parent".to_owned())?
                .to_path_buf(),
        });
    }
    let shims = tapid_linker::plan_shims(managed, packages, current_platform())
        .map_err(|e| e.to_string())?;
    for entry in shims.entries {
        let parent = entry
            .target
            .parent()
            .ok_or_else(|| "shim target has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        match entry.strategy {
            tapid_linker::ShimStrategy::UnixSymlink => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(relative_path(parent, &entry.source), &entry.target)
                    .map_err(|e| format!("cannot materialize package bin shim: {e}"))?;
                #[cfg(not(unix))]
                return Err("package bin shims are unsupported on this platform".into());
            }
            tapid_linker::ShimStrategy::WindowsCmdAndPowerShell => {
                let cmd = entry.target.with_extension("cmd");
                let ps1 = entry.target.with_extension("ps1");
                fs::write(
                    cmd,
                    format!("@echo off\r\n\"{}\" %*\r\n", entry.source.display()),
                )
                .map_err(|e| e.to_string())?;
                fs::write(ps1, format!("& '{}' $args\r\n", entry.source.display()))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
#[cfg(unix)]
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common..] {
        result.push(component.as_os_str());
    }
    result
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    let source_meta = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if !source_meta.is_dir() {
        return Err("store tree root is not a directory".into());
    }
    let package = source.join("package");
    let root = match fs::symlink_metadata(&package) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "symlink in store tree is not replayable: {}",
                package.display()
            ));
        }
        Ok(meta) if meta.is_dir() => &package,
        Ok(_) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => source,
        Err(error) => return Err(error.to_string()),
    };
    copy_tree_contents(root, target)
}

fn copy_tree_contents(source: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(target).map_err(|e| e.to_string())?;
    for item in fs::read_dir(source).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let src = item.path();
        let dst = target.join(item.file_name());
        let meta = fs::symlink_metadata(&src).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "symlink in store tree is not replayable: {}",
                src.display()
            ));
        }
        if meta.is_dir() {
            copy_tree_contents(&src, &dst)?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else if meta.is_file() {
            let mut input = fs::File::open(&src).map_err(|e| e.to_string())?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dst)
                .map_err(|e| e.to_string())?;
            io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("unsupported store tree entry: {}", src.display()));
        }
    }
    Ok(())
}
const MANAGED_MARKER: &[u8] = b"tapid-managed-v1\n";

fn activate_node_modules(project: &Path, stage: &Path) -> Result<(), String> {
    let staged = stage.join("node_modules");
    if !staged.is_dir() {
        return Err("install transaction produced an empty node_modules layout".into());
    }
    let destination = project.join("node_modules");
    let marker = project.join(".tapid-managed");
    let marker_exists = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => true,
        Ok(_) => {
            return Err("refusing to use a non-regular .tapid-managed marker".to_owned());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("cannot inspect .tapid-managed: {error}")),
    };
    if destination.exists() && !marker_exists {
        return Err(
            "refusing to replace unmarked node_modules; create .tapid-managed to opt in".into(),
        );
    }
    if marker_exists
        && fs::read(&marker).map_err(|_| "cannot read .tapid-managed".to_owned())? != MANAGED_MARKER
    {
        return Err(
            "refusing to replace node_modules with an invalid .tapid-managed marker".into(),
        );
    }
    let backup = project.join(format!(
        ".tapid-node-modules-old-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    if destination.exists() {
        fs::rename(&destination, &backup)
            .map_err(|_| "cannot stage existing node_modules for replacement".to_owned())?;
    }
    if std::env::var_os("TAPID_TEST_FAIL_ACTIVATION").is_some() {
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        return Err("install activation failed (injected)".into());
    }
    if let Err(error) = fs::rename(&staged, &destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        return Err(format!("cannot activate node_modules: {error}"));
    }
    let marker_temp = project.join(format!(
        ".tapid-managed-{}-{}.tmp",
        std::process::id(),
        unique_nonce()
    ));
    let marker_backup = project.join(format!(
        ".tapid-managed-old-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    let marker_backed_up = cfg!(windows) && marker_exists;
    if marker_backed_up && let Err(error) = fs::rename(&marker, &marker_backup) {
        let _ = fs::rename(&backup, &destination);
        return Err(format!("cannot stage .tapid-managed marker: {error}"));
    }
    let marker_result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker_temp)
        .and_then(|mut file| {
            io::Write::write_all(&mut file, MANAGED_MARKER)?;
            file.sync_all()?;
            fs::rename(&marker_temp, &marker)
        });
    if let Err(error) = marker_result {
        let _ = fs::remove_file(&marker_temp);
        let _ = fs::remove_dir_all(&destination);
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        if marker_backed_up {
            let _ = fs::rename(&marker_backup, &marker);
        } else if !marker_exists {
            let _ = fs::remove_file(&marker);
        }
        return Err(format!("cannot write .tapid-managed: {error}"));
    }
    if marker_backed_up {
        let _ = fs::remove_file(&marker_backup);
    }
    let _ = fs::remove_dir_all(&backup);
    let _ = fs::remove_dir_all(stage);
    Ok(())
}

fn validate(path: &Path) -> ExitCode {
    match read_manifest(path) {
        Ok(manifest) => {
            println!("Valid manifest: {}@{}", manifest.name(), manifest.version());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
fn read_manifest(path: &Path) -> Result<PackageManifest, String> {
    let input = fs::read_to_string(path)
        .map_err(|source| format!("cannot read manifest {}: {source}", path.display()))?;
    PackageManifest::parse(&input).map_err(|error| error.to_string())
}
fn replace_lockfile(path: &Path, contents: &str) -> Result<Option<PathBuf>, String> {
    let nonce = format!("{}-{}", std::process::id(), unique_nonce());
    let temp = path.with_file_name(format!(".tapid-lock-{nonce}.tmp"));
    let backup = path.with_file_name(format!(".tapid-lock-{nonce}.bak"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    io::Write::write_all(&mut file, contents.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let had_old = path.exists();
    if had_old {
        fs::rename(path, &backup).map_err(|e| e.to_string())?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        if had_old {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }
    Ok(had_old.then_some(backup))
}

fn rollback_lockfile(path: &Path, backup: Option<&Path>) -> Result<(), String> {
    if let Some(backup) = backup {
        let _ = fs::remove_file(path);
        fs::rename(backup, path).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn discard_lockfile_backup(backup: Option<&Path>) -> Result<(), String> {
    if let Some(backup) = backup {
        fs::remove_file(backup).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256-{}", hex::encode(hasher.finalize()))
}

fn init(path: Option<&Path>) -> ExitCode {
    let raw = path.unwrap_or_else(|| Path::new("."));
    let directory = match fs::canonicalize(raw) {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                raw.display()
            );
            return ExitCode::from(1);
        }
    };
    match create_manifest(&directory) {
        Ok(()) => {
            println!("Created {}", directory.join("package.json").display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
fn create_manifest(directory: &Path) -> Result<(), String> {
    if !directory.is_dir() {
        return Err(format!(
            "project path is not a directory: {}",
            directory.display()
        ));
    }
    let name = directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            format!(
                "project directory has no valid UTF-8 name: {}",
                directory.display()
            )
        })?;
    let manifest = PackageManifest::new(name, "0.1.0", true)
        .map_err(|error| format!("cannot create project manifest: {error}"))?;
    let path = directory.join("package.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                format!(
                    "refusing to overwrite existing manifest: {}",
                    path.display()
                )
            } else {
                format!("cannot write manifest {}: {error}", path.display())
            }
        })?;
    if let Err(error) = io::Write::write_all(&mut file, manifest.to_json().as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(format!("cannot write manifest {}: {error}", path.display()));
    }
    Ok(())
}
fn verify_lock(path: &Path) -> ExitCode {
    match fs::read_to_string(path)
        .map_err(|error| error.to_string())
        .and_then(|input| Lockfile::from_json(&input).map_err(|error| error.to_string()))
    {
        Ok(_) => {
            println!("Valid lockfile: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: cannot verify {}: {error}", path.display());
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;

    #[test]
    fn explicit_registry_root_matches_only_its_registry_identity() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"jsr:@std/path":"1.0.0"}}"#,
        )
        .unwrap();
        let roots = replay_root_identities(&manifest).unwrap();
        let jsr: tapid_lockfile::LockfilePackageKey =
            "https://jsr.io|@std/path@1.0.0|peer=-|platform=-"
                .parse()
                .unwrap();
        let npm: tapid_lockfile::LockfilePackageKey =
            "https://registry.npmjs.org|@std/path@1.0.0|peer=-|platform=-"
                .parse()
                .unwrap();

        assert!(replay_root_matches(&roots, &jsr));
        assert!(!replay_root_matches(&roots, &npm));
    }
}

#[cfg(test)]
mod copy_tests {
    #[test]
    fn copy_tree_handles_nested_directories_and_preserves_executable_mode() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested").join("data"), b"nested").unwrap();
        let bin = source.join("bin");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }

        copy_tree(&source, &target).unwrap();
        assert_eq!(
            fs::read(target.join("nested").join("data")).unwrap(),
            b"nested"
        );
        assert!(target.join("bin").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(target.join("nested"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
            assert_eq!(
                fs::metadata(target.join("bin"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        let _ = fs::remove_dir_all(root);
    }
}
