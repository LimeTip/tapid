mod online;
#[allow(dead_code)]
mod run;
mod upgrade;

use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tapid_core::{ArtifactDigest, PackageInstanceId, PeerContext, PlatformContext};
use tapid_linker::{
    DependencyEdge, InstanceKey, LayoutInput, ManagedRoot, PackageInstance, Platform,
    VerifiedTreeReference, plan_layout,
};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;
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
#[derive(Debug, Subcommand)]
enum Command {
    /// Create a private package.json manifest.
    Init { path: Option<PathBuf> },
    Manifest {
        #[command(subcommand)]
        command: ManifestCommand,
    },
    Lock {
        #[command(subcommand)]
        command: LockCommand,
    },
    /// Replay the project's validated lockfile without network access.
    Run {
        /// Root package script name.
        script: String,
        /// Project directory containing package.json.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,
        /// Arguments forwarded after `--` to the script.
        #[arg(last = true)]
        arguments: Vec<String>,
    },
    Install {
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        frozen: bool,
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,
        /// Store root containing verified `trees/<sha256-...>` directories.
        #[arg(long)]
        store_dir: Option<PathBuf>,
        /// Local JSON registry fixture used by tests and air-gapped development.
        #[arg(long)]
        registry_fixture: Option<PathBuf>,
    },
    /// Upgrade Tapid to the latest stable release.
    Upgrade {
        /// Install a specific stable release instead of the latest one.
        #[arg(long)]
        version: Option<String>,
    },
}
#[derive(Debug, Subcommand)]
enum ManifestCommand {
    Validate { path: Option<PathBuf> },
}
#[derive(Debug, Subcommand)]
enum LockCommand {
    Verify,
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
        Some(Command::Install {
            offline,
            frozen,
            project_dir,
            store_dir,
            registry_fixture,
        }) => install(
            &project_dir,
            store_dir.as_deref(),
            offline,
            frozen,
            registry_fixture.as_deref(),
        ),
        Some(Command::Upgrade { version }) => upgrade::upgrade(version.as_deref()),
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

fn install(
    project_dir: &Path,
    store_root: Option<&Path>,
    offline: bool,
    frozen: bool,
    registry_fixture: Option<&Path>,
) -> ExitCode {
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
    let lock_path = project_dir.join("tapid.lock");
    if !offline && !frozen {
        let store = Store::new(
            store_root
                .map(PathBuf::from)
                .unwrap_or_else(|| project_dir.join(".tapid-store")),
        );
        let (lock, input, trees) =
            match online::resolve_and_fetch(&project_dir, &manifest, &store, registry_fixture) {
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
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let platform = current_platform();
    let plan = match plan_layout(root, input, platform) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let stage = project_dir.join(format!(".tapid-install-stage-{}", std::process::id()));
    let _ = fs::remove_dir_all(&stage);
    let result = materialize_stage(&stage, &plan, &trees)
        .and_then(|_| activate_node_modules(project_dir, &stage));
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

fn replay_input(
    lock: &Lockfile,
    manifest: &PackageManifest,
    store: &Store,
) -> Result<(LayoutInput, BTreeMap<String, PathBuf>), String> {
    let mut instances = Vec::new();
    let mut keys = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let typed_packages = lock.packages_typed().map_err(|e| e.to_string())?;
    for (key, package) in &typed_packages {
        let encoded = key.to_string();
        let digest: ArtifactDigest = package
            .tree_digest()
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
        let tree = store
            .verified_tree_path(&digest)
            .map_err(|e| format!("package {encoded} tree unavailable: {e}"))?;
        let peer = parse_peer(&key.peer_context)?;
        let platform = parse_platform(&key.platform_context)?;
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
    let mut root_names = BTreeMap::new();
    for map in [
        manifest.dependencies(),
        manifest.dev_dependencies(),
        manifest.optional_dependencies(),
    ] {
        for name in map.keys() {
            root_names.insert(name.clone(), ());
        }
    }
    for (key, _) in &typed_packages {
        let encoded = key.to_string();
        if root_names.is_empty() || root_names.contains_key(&key.name.to_string()) {
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
    Ok((
        LayoutInput {
            instances,
            root_dependencies: roots,
            dependency_edges: edges,
        },
        trees,
    ))
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

fn parse_peer(value: &str) -> Result<PeerContext, String> {
    if value.is_empty() || value == "-" {
        return Ok(PeerContext::default());
    }
    let mut result = PeerContext::default();
    for item in value.split(',') {
        let (name, version) = item
            .rsplit_once('@')
            .ok_or_else(|| format!("invalid peer context: {value}"))?;
        result = result.with(
            name.parse::<tapid_core::PackageName>()
                .map_err(|e| e.to_string())?,
            version
                .parse::<tapid_core::PackageVersion>()
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(result)
}
fn parse_platform(value: &str) -> Result<PlatformContext, String> {
    if value.is_empty() || value == "-" {
        return PlatformContext::new(None, None, None).map_err(|e| e.to_string());
    }
    let parts: Vec<_> = value.split('-').collect();
    PlatformContext::new(
        parts.first().copied(),
        parts.get(1).copied(),
        parts.get(2).copied(),
    )
    .map_err(|e| e.to_string())
}

fn materialize_stage(
    stage: &Path,
    plan: &tapid_linker::MaterializationPlan,
    trees: &BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    fs::create_dir_all(stage.join("node_modules")).map_err(|e| e.to_string())?;
    let mut by_source = BTreeMap::new();
    for entry in &plan.entries {
        let tree = trees
            .values()
            .find(|path| **path == entry.source)
            .ok_or_else(|| "tree replay mapping lost".to_owned())?;
        let target = stage.join(
            entry
                .target
                .strip_prefix(&plan.managed_root.path)
                .map_err(|_| "invalid stage target")?,
        );
        copy_tree(tree, &target)?;
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
        let package_root = if package_dir.join("package").is_dir() {
            package_dir.join("package")
        } else {
            package_dir.clone()
        };
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
    let package = source.join("package");
    let root = if package.is_dir() { &package } else { source };
    copy_tree_contents(root, target)
}

fn copy_tree_contents(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|e| e.to_string())?;
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
            copy_tree(&src, &dst)?;
        } else if meta.is_file() {
            fs::copy(&src, &dst).map_err(|e| e.to_string())?;
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
    let marker_exists = marker.exists();
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
    let backup = project.join(format!(".tapid-node-modules-old-{}", std::process::id()));
    let _ = fs::remove_dir_all(&backup);
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
    if let Err(error) = fs::write(&marker, MANAGED_MARKER) {
        let _ = fs::remove_dir_all(&destination);
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        if !marker_exists {
            let _ = fs::remove_file(&marker);
        }
        return Err(format!("cannot write .tapid-managed: {error}"));
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

fn unique_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
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
