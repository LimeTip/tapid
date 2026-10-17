use clap::Args as ClapArgs;
use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tapid_lockfile::ImportedNpmLockfile;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// npm lockfileVersion 3 file to import into the current project.
    path: PathBuf,
}

pub(crate) fn run(args: Args) -> ExitCode {
    match import(args) {
        Ok(()) => {
            println!(
                "Imported npm selections into tapid.lock. Run tapid install --frozen to verify and install them."
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn import(args: Args) -> Result<(), String> {
    let project = std::env::current_dir().map_err(|e| e.to_string())?;
    let input = fs::read_to_string(&args.path).map_err(|e| format!("cannot read npm lock: {e}"))?;
    let manifest_path = project.join("package.json");
    let manifest = read_manifest(&manifest_path)?;
    tapid_manifest::PackageManifest::parse(&manifest).map_err(|e| e.to_string())?;
    let digest = crate::filesystem::atomic::digest_bytes(manifest.as_bytes());
    let mut lock =
        ImportedNpmLockfile::import(&input, &manifest, &digest).map_err(|e| e.to_string())?;
    let config = crate::registry::RegistryConfig::load(&project)?;
    let workspace = crate::online::workspace_materialization(&project, &config)?;
    lock.bind_workspace_manifests(&crate::online::imported_workspace_manifests(
        &project, &workspace,
    )?)
    .map_err(|e| e.to_string())?;
    for package in lock.graph().map_err(|e| e.to_string())?.packages.values() {
        if config.origin_for_name(&package.name)? != package.registry {
            return Err(format!(
                "/packages/{}/resolved: package {}@{}: resolved: artifact origin does not match Tapid registry routing; configure tapid.toml before importing",
                package.path.replace('/', "~1"),
                package.name,
                package.version
            ));
        }
    }
    let json = lock.to_json().map_err(|e| e.to_string())?;
    // Validation completes before acquiring the lock, which can recover transactions.
    // Import must never recover or alter an interrupted install.
    let _guard = crate::filesystem::activation::ActivationLock::acquire_without_recovery(&project)
        .map_err(|e| e.to_string())?;
    let destination = project.join("tapid.lock");
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && !metadata.is_file()
    {
        return Err("tapid.lock must be a regular, non-symlink file".into());
    }
    if read_manifest(&manifest_path)? != manifest {
        return Err("package.json changed during import; retry".into());
    }
    let current_workspace = crate::online::workspace_materialization(&project, &config)?;
    lock.validate_workspace_manifests(&crate::online::imported_workspace_manifests(
        &project,
        &current_workspace,
    )?)
    .map_err(|e| e.to_string())?;
    let backup = crate::filesystem::atomic::replace_lockfile(&destination, &json)?;
    crate::filesystem::atomic::discard_lockfile_backup(backup.as_deref())
}

fn read_manifest(path: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("cannot inspect package.json: {e}"))?;
    if !metadata.file_type().is_file() {
        return Err("package.json must be a regular, non-symlink file".into());
    }
    fs::read_to_string(path).map_err(|e| format!("cannot read package.json: {e}"))
}
