use crate::application::outcome::{
    ChangeState, ErrorKind, OperationFailure, OperationOutcome, OperationalError,
};
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

/// Render npm import outcomes while keeping diagnostic prose out of JSON results.
pub(crate) fn run(
    args: Args,
    json: bool,
    configuration: &crate::registry::ConfigSelection,
) -> ExitCode {
    match import(args, configuration) {
        Ok(outcome) => {
            if json {
                return crate::output::json::completed(
                    &outcome,
                    "import-package-lock",
                    serde_json::json!({"format":"npm_v3", "verification":"pending"}),
                );
            }
            println!(
                "Imported npm selections into tapid.lock. Run tapid install --frozen to verify and install them."
            );
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, "import-package-lock", json),
    }
}

/// Validate npm selections before locking, then publish the single lockfile without a rollback gap.
fn import(
    args: Args,
    configuration: &crate::registry::ConfigSelection,
) -> Result<OperationOutcome, OperationFailure> {
    let project = std::env::current_dir().map_err(|e| {
        OperationFailure::unchanged(
            Path::new("."),
            OperationalError::from_source(ErrorKind::Project, e),
        )
    })?;
    let mut outcome = OperationOutcome::unchanged(&project);
    let result = (|| -> Result<(), OperationalError> {
        let input = fs::read_to_string(&args.path).map_err(|e| {
            OperationalError::from_source(ErrorKind::Lockfile, e).context("cannot read npm lock")
        })?;
        let manifest_path = project.join("package.json");
        let manifest = read_manifest(&manifest_path)
            .map_err(|e| OperationalError::new(ErrorKind::Manifest, e))?;
        tapid_manifest::PackageManifest::parse(&manifest)
            .map_err(|e| OperationalError::from_source(ErrorKind::Manifest, e))?;
        let digest = crate::filesystem::atomic::digest_bytes(manifest.as_bytes());
        let mut lock = ImportedNpmLockfile::import(&input, &manifest, &digest)
            .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
        let config = configuration
            .load(Some(&project))
            .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
        let workspace = crate::online::workspace_materialization(&project, &config)?;
        lock.bind_workspace_manifests(&crate::online::imported_workspace_manifests(
            &project, &workspace,
        )?)
        .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
        for package in lock
            .graph()
            .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?
            .packages
            .values()
        {
            if config
                .origin_for_name(&package.name)
                .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?
                != package.registry
            {
                return Err(OperationalError::new(
                    ErrorKind::RegistryConfiguration,
                    format!(
                        "/packages/{}/resolved: package {}@{}: resolved: artifact origin does not match Tapid registry routing; configure tapid.toml before importing",
                        package.path.replace('/', "~1"),
                        package.name,
                        package.version
                    ),
                ));
            }
        }
        let json = lock
            .to_json()
            .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
        // Validation completes before acquiring the lock, which can recover transactions.
        // Import must never recover or alter an interrupted install.
        let _guard =
            crate::filesystem::activation::ActivationLock::acquire_without_recovery(&project)?;
        let destination = project.join("tapid.lock");
        if let Ok(metadata) = fs::symlink_metadata(&destination)
            && !metadata.is_file()
        {
            return Err(OperationalError::new(
                ErrorKind::Lockfile,
                "tapid.lock must be a regular, non-symlink file",
            ));
        }
        if read_manifest(&manifest_path)
            .map_err(|e| OperationalError::new(ErrorKind::Manifest, e))?
            != manifest
        {
            return Err(OperationalError::new(
                ErrorKind::LockManifestMismatch,
                "package.json changed during import; retry",
            ));
        }
        validate_workspace_unchanged(&project, &config, &lock)?;
        crate::filesystem::atomic::publish_lockfile(&destination, &json)
            .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
        outcome.state = ChangeState::Committed;
        outcome.changed_files.push(destination);
        Ok(())
    })();
    match result {
        Ok(()) => Ok(outcome),
        Err(error) => {
            if error.kind == ErrorKind::Recovery {
                outcome.state = ChangeState::RecoveryRequired;
            }
            Err(OperationFailure::new(error, outcome, None))
        }
    }
}

/// Read exact manifest bytes for digest and concurrent-edit checks, rejecting nonregular files.
fn read_manifest(path: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("cannot inspect package.json: {e}"))?;
    if !metadata.file_type().is_file() {
        return Err("package.json must be a regular, non-symlink file".into());
    }
    fs::read_to_string(path).map_err(|e| format!("cannot read package.json: {e}"))
}

/// Rediscover member inputs and compare them with the receipts bound before publication.
fn validate_workspace_unchanged(
    project: &Path,
    config: &crate::registry::RegistryConfig,
    lock: &ImportedNpmLockfile,
) -> Result<(), OperationalError> {
    let workspace = crate::online::workspace_materialization(project, config)?;
    lock.validate_workspace_manifests(&crate::online::imported_workspace_manifests(
        project, &workspace,
    )?)
    .map_err(|error| OperationalError::from_source(ErrorKind::LockManifestMismatch, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changes before rediscovery must fail as mismatches even when discovery itself succeeds.
    #[test]
    fn final_workspace_validation_preserves_mismatch_kind() {
        for member in [
            "{ \"name\":\"member\",\"version\":\"1.0.0\"}",
            r#"{"name":"member","version":"2.0.0"}"#,
        ] {
            let project = tapid_test_support::TempProject::new("import-final-workspace").unwrap();
            let root = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
            let original = r#"{"name":"member","version":"1.0.0"}"#;
            project.write("package.json", root.as_bytes()).unwrap();
            project
                .write("packages/member/package.json", original.as_bytes())
                .unwrap();
            project
                .write("tapid.lock", b"preserve existing lock")
                .unwrap();
            let input = serde_json::json!({"lockfileVersion":3,"packages":{
                "":serde_json::from_str::<serde_json::Value>(root).unwrap(),
                "packages/member":serde_json::from_str::<serde_json::Value>(original).unwrap(),
                "node_modules/member":{"resolved":"packages/member","link":true}
            }})
            .to_string();
            project
                .write("package-lock.json", input.as_bytes())
                .unwrap();
            let config = crate::registry::RegistryConfig::from_toml("").unwrap();
            let mut lock = ImportedNpmLockfile::import(
                &input,
                root,
                &crate::filesystem::atomic::digest_bytes(root.as_bytes()),
            )
            .unwrap();
            let workspace =
                crate::online::workspace_materialization(project.path(), &config).unwrap();
            lock.bind_workspace_manifests(
                &crate::online::imported_workspace_manifests(project.path(), &workspace).unwrap(),
            )
            .unwrap();
            validate_workspace_unchanged(project.path(), &config, &lock).unwrap();

            project
                .write("packages/member/package.json", member.as_bytes())
                .unwrap();
            let error = validate_workspace_unchanged(project.path(), &config, &lock).unwrap_err();
            assert_eq!(error.kind, ErrorKind::LockManifestMismatch);
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                b"preserve existing lock"
            );
            assert_eq!(
                fs::read(project.path().join("package-lock.json")).unwrap(),
                input.as_bytes()
            );
            assert_eq!(
                fs::read(project.path().join("packages/member/package.json")).unwrap(),
                member.as_bytes()
            );
        }
    }
}
