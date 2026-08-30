use crate::transport::release;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tapid_release_client::{
    Error as ReleaseError, Fetcher, ReleaseState, accept_release, read_release_state,
    write_release_state,
};
use tapid_signatures::KeyRing;

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

pub(crate) fn run(
    endpoint_args: &[String],
    keyring_arg: Option<&Path>,
    destination_arg: Option<&Path>,
    dry_run: bool,
) -> Result<UpgradeReport, String> {
    let keyring = match keyring_arg
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TAPID_RELEASE_KEYRING").map(PathBuf::from))
    {
        Some(keyring_path) => {
            let keyring_bytes = match fs::read(&keyring_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Err(format!(
                        "cannot read trusted release keyring '{}': {error}",
                        keyring_path.display()
                    ));
                }
            };
            match KeyRing::from_embedded_json(&keyring_bytes) {
                Ok(value) => value,
                Err(error) => return Err(format!("invalid trusted release keyring: {error}")),
            }
        }
        None => match KeyRing::production() {
            Ok(value) => value,
            Err(error) => {
                return Err(format!(
                    "embedded production release keyring is invalid: {error}"
                ));
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
            Err(error) => return Err(format!("cannot determine current executable: {error}")),
        },
    };
    crate::filesystem::atomic::validate_upgrade_destination(&destination)?;
    let target = release::release_target();
    let mut fetcher = release::CurlFetcher;
    let mut pending_state = None;
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
                        return Err(format!("cannot accept verified release state: {error}"));
                    }
                };
                pending_state = Some((state_path, state, digest));
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
                        return Err(format!(
                            "stable discovery unavailable and recovery failed: {recovery}"
                        ));
                    }
                }
            }
            Err(error) => return Err(error.to_string()),
        };
    let executable = crate::filesystem::atomic::materialize_artifact(&artifact_name, &bytes)?;
    if dry_run {
        return Ok(UpgradeReport {
            version: manifest_version,
            target: target.to_owned(),
            destination,
            dry_run: true,
        });
    }
    match activate_and_persist(
        &destination,
        &executable,
        pending_state,
        &bytes,
        crate::filesystem::atomic::replace_executable,
    ) {
        Ok(()) => Ok(UpgradeReport {
            version: manifest_version,
            target: target.to_owned(),
            destination,
            dry_run: false,
        }),
        Err(error) => Err(error),
    }
}

pub(crate) struct UpgradeReport {
    pub(crate) version: String,
    pub(crate) target: String,
    pub(crate) destination: PathBuf,
    pub(crate) dry_run: bool,
}

fn activate_and_persist<F>(
    destination: &Path,
    executable: &[u8],
    pending_state: Option<(PathBuf, ReleaseState, String)>,
    bytes: &[u8],
    activate: F,
) -> Result<(), String>
where
    F: FnOnce(&Path, &[u8]) -> Result<(), String>,
{
    activate(destination, executable)
        .map_err(|error| format!("cannot activate verified artifact: {error}"))?;
    if let Some((state_path, state, digest)) = pending_state {
        persist_activated_release(destination, &state_path, &state, &digest, bytes).map_err(
            |error| format!("activated release but cannot persist last-known-good state: {error}"),
        )?;
    }
    Ok(())
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

fn persist_activated_release(
    destination: &Path,
    state_path: &Path,
    state: &ReleaseState,
    digest: &str,
    bytes: &[u8],
) -> Result<(), String> {
    write_cached_artifact(destination, digest, bytes)
        .and_then(|_| write_release_state(state_path, state).map_err(|e| e.to_string()))
}

fn write_cached_artifact(destination: &Path, digest: &str, bytes: &[u8]) -> Result<(), String> {
    let path = cached_artifact_path(destination, digest);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err("cached artifact must be a regular file".into());
        }
        let existing = fs::read(&path).map_err(|error| error.to_string())?;
        let existing_digest = format!("{:x}", Sha256::digest(&existing));
        if existing_digest != digest {
            return Err("cached artifact digest does not match expected digest".into());
        }
        return Ok(());
    }
    let temp = path.with_file_name(format!(
        ".tapid-release-artifact-tmp-{}",
        crate::filesystem::atomic::unique_nonce()
    ));
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
    let max_bytes = usize::try_from(artifact.size)
        .map_err(|_| ReleaseError::Fetch("artifact is too large for this platform".into()))?;
    if max_bytes > crate::filesystem::atomic::MAX_ARTIFACT_BYTES {
        return Err(ReleaseError::Fetch(
            "artifact exceeds maximum archive size".into(),
        ));
    }
    let bytes = fetcher
        .fetch_with_limit(&artifact.url, max_bytes)
        .map_err(ReleaseError::Fetch)?;
    manifest.verify_artifact(&bytes)?;
    Ok((manifest, bytes))
}

#[cfg(test)]
mod upgrade_tests {
    use super::{DEFAULT_STABLE_ENDPOINTS, persist_activated_release, stable_discovery_endpoints};
    use crate::filesystem::atomic::materialize_artifact;
    use sha2::Digest;
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tapid_release_client::{ReleaseState, read_release_state};

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
    fn activated_release_persistence_writes_cache_and_last_known_good_state() {
        let root = temp("persist");
        let destination = root.join("tapid");
        let state_path = root.join(".tapid-release-state.json");
        let bytes = b"artifact";
        let digest = format!("{:x}", sha2::Sha256::digest(bytes));
        let state = ReleaseState::new("0.0.7", 7, digest.clone()).unwrap();

        persist_activated_release(&destination, &state_path, &state, &digest, bytes).unwrap();

        assert_eq!(
            fs::read(super::cached_artifact_path(&destination, &digest)).unwrap(),
            bytes
        );
        assert_eq!(read_release_state(&state_path).unwrap(), state);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activated_release_persistence_rejects_mismatched_existing_cache() {
        let root = temp("mismatched-cache");
        let destination = root.join("tapid");
        let state_path = root.join(".tapid-release-state.json");
        let bytes = b"verified artifact";
        let digest = format!("{:x}", sha2::Sha256::digest(bytes));
        let state = ReleaseState::new("0.0.7", 7, digest.clone()).unwrap();
        let cache_path = super::cached_artifact_path(&destination, &digest);
        fs::write(&cache_path, b"different artifact").unwrap();

        let result = persist_activated_release(&destination, &state_path, &state, &digest, bytes);

        assert_eq!(
            result.unwrap_err(),
            "cached artifact digest does not match expected digest"
        );
        assert_eq!(fs::read(cache_path).unwrap(), b"different artifact");
        assert!(!state_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_activation_does_not_persist_pending_release_state() {
        let root = temp("failed-activation");
        let destination = root.join("tapid");
        let state_path = root.join(".tapid-release-state.json");
        let bytes = b"artifact";
        let digest = format!("{:x}", sha2::Sha256::digest(bytes));
        let state = ReleaseState::new("0.0.7", 7, digest.clone()).unwrap();

        let result = super::activate_and_persist(
            &destination,
            b"executable",
            Some((state_path.clone(), state, digest.clone())),
            bytes,
            |_destination, _executable| Err("injected activation failure".to_owned()),
        );

        assert!(result.is_err());
        assert!(!state_path.exists());
        assert!(!super::cached_artifact_path(&destination, &digest).exists());
        fs::remove_dir_all(root).unwrap();
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
