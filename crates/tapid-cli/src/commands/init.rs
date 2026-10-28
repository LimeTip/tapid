use crate::application::outcome::{ChangeState, OperationOutcome, RetryAdvice};
use clap::Args as ClapArgs;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tapid_manifest::PackageManifest;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Existing directory in which to create package.json. Defaults to the current directory.
    pub(crate) path: Option<PathBuf>,
}

/// Initialize the selected directory, defaulting to the current directory.
/// Render human diagnostics or a schema-v1 result describing creation or failure.
pub(crate) fn run(args: Args, json_output: bool) -> ExitCode {
    let raw = args.path.as_deref().unwrap_or_else(|| Path::new("."));
    let directory = match fs::canonicalize(raw) {
        Ok(path) => path,
        Err(error) => {
            if json_output {
                return crate::output::json::operation_error(
                    "init",
                    "PROJECT_UNAVAILABLE",
                    None,
                    None,
                    Some(RetryAdvice::AfterCorrection),
                );
            }
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                raw.display()
            );
            return ExitCode::from(1);
        }
    };
    match create_manifest(&directory) {
        Ok(()) if json_output => {
            let mut outcome = crate::application::outcome::OperationOutcome::unchanged(&directory);
            outcome.state = crate::application::outcome::ChangeState::Committed;
            outcome.changed_files.push(directory.join("package.json"));
            crate::output::json::completed(
                &outcome,
                "init",
                serde_json::json!({"manifest": "package.json"}),
            )
        }
        Ok(()) => {
            println!("Created {}", directory.join("package.json").display());
            ExitCode::SUCCESS
        }
        Err(error) if json_output => {
            let outcome = error.outcome(&directory);
            let (code, context) = error.diagnostic();
            crate::output::json::operation_error(
                "init",
                code,
                context,
                Some(&outcome),
                Some(error.retry()),
            )
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

enum CreationError {
    Unavailable(String),
    Exists(PathBuf),
    Invalid(String),
    Write(PathBuf, io::Error),
    RolledBack(PathBuf, io::Error),
    Residual {
        path: PathBuf,
        source: io::Error,
        cleanup: io::Error,
    },
}

impl CreationError {
    fn outcome(&self, directory: &Path) -> OperationOutcome {
        let mut outcome = OperationOutcome::unchanged(directory);
        if matches!(self, Self::RolledBack(..)) {
            outcome.state = ChangeState::RolledBack;
        } else if let Self::Residual { path, .. } = self {
            outcome.state = ChangeState::RecoveryRequired;
            outcome.changed_files.push(path.clone());
        }
        outcome
    }

    fn retry(&self) -> RetryAdvice {
        if matches!(self, Self::Residual { .. }) {
            RetryAdvice::RecoverFirst
        } else {
            RetryAdvice::AfterCorrection
        }
    }

    fn diagnostic(&self) -> (&'static str, Option<serde_json::Value>) {
        match self {
            Self::Unavailable(_) => ("PROJECT_UNAVAILABLE", None),
            Self::Exists(_) => ("MANIFEST_ALREADY_EXISTS", None),
            Self::Invalid(_) => ("MANIFEST_INVALID", None),
            Self::Write(_, source) | Self::RolledBack(_, source) => {
                ("MANIFEST_WRITE_FAILED", Some(io_context(source)))
            }
            Self::Residual {
                source, cleanup, ..
            } => {
                let mut context = io_context(source);
                context["cleanup"] = io_context(cleanup);
                ("MANIFEST_WRITE_FAILED", Some(context))
            }
        }
    }
}

fn io_context(source: &io::Error) -> serde_json::Value {
    serde_json::json!({"io_kind": format!("{:?}", source.kind()), "os_code": source.raw_os_error()})
}

impl std::fmt::Display for CreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Invalid(message) => f.write_str(message),
            Self::Exists(path) => write!(
                f,
                "refusing to overwrite existing manifest: {}",
                path.display()
            ),
            Self::Write(path, source)
            | Self::RolledBack(path, source)
            | Self::Residual { path, source, .. } => {
                write!(f, "cannot write manifest {}: {source}", path.display())
            }
        }
    }
}

/// Create a private package manifest without overwriting an existing entry.
/// Distinguish unavailable directories, invalid names, existing entries, and I/O failures.
/// After a write failure, attempt to remove the new file; cleanup is best effort.
fn create_manifest(directory: &Path) -> Result<(), CreationError> {
    create_manifest_with(directory, io::Write::write_all, |path| {
        fs::remove_file(path)
    })
}

fn create_manifest_with(
    directory: &Path,
    write: impl FnOnce(&mut fs::File, &[u8]) -> io::Result<()>,
    remove: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), CreationError> {
    if !directory.is_dir() {
        return Err(CreationError::Unavailable(format!(
            "project path is not a directory: {}",
            directory.display()
        )));
    }
    let name = directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            CreationError::Invalid(format!(
                "project directory has no valid UTF-8 name: {}",
                directory.display()
            ))
        })?;
    let manifest = PackageManifest::new(name, "0.1.0", true).map_err(|error| {
        CreationError::Invalid(format!("cannot create project manifest: {error}"))
    })?;
    let path = directory.join("package.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                CreationError::Exists(path.clone())
            } else {
                CreationError::Write(path.clone(), error)
            }
        })?;
    if let Err(error) = write(&mut file, manifest.to_json().as_bytes()) {
        drop(file);
        return Err(match remove(&path) {
            Ok(()) => CreationError::RolledBack(path, error),
            Err(cleanup) => CreationError::Residual {
                path,
                source: error,
                cleanup,
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial_write(file: &mut fs::File, _: &[u8]) -> io::Result<()> {
        io::Write::write_all(file, b"partial manifest")?;
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "private write diagnostic",
        ))
    }

    fn result(error: &CreationError, directory: &Path) -> serde_json::Value {
        let (code, context) = error.diagnostic();
        crate::output::json::operation_error_result(
            "init",
            code,
            context,
            Some(&error.outcome(directory)),
            Some(error.retry()),
        )
    }

    #[test]
    fn failed_write_removes_partial_manifest_and_reports_rollback() {
        let project = tapid_test_support::TempProject::new("init-write-cleanup").unwrap();
        let directory = fs::canonicalize(project.path()).unwrap();
        let error = create_manifest_with(&directory, partial_write, |path| fs::remove_file(path))
            .err()
            .unwrap();
        assert!(!directory.join("package.json").exists());
        let result = result(&error, &directory);
        assert_eq!(result["changes"]["state"], "rolled_back");
        assert_eq!(result["changes"]["paths"], serde_json::json!([]));
        assert_eq!(result["retry"], "after_correction");
        assert_eq!(result["errors"][0]["code"], "MANIFEST_WRITE_FAILED");
        assert_eq!(result["errors"][0]["context"]["io_kind"], "WriteZero");
        assert!(!result.to_string().contains("private write diagnostic"));
    }

    #[test]
    fn failed_cleanup_reports_only_residual_manifest_for_recovery() {
        let project = tapid_test_support::TempProject::new("init-residual").unwrap();
        let directory = fs::canonicalize(project.path()).unwrap();
        let error = create_manifest_with(&directory, partial_write, |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private cleanup diagnostic",
            ))
        })
        .err()
        .unwrap();
        let path = directory.join("package.json");
        assert_eq!(fs::read(&path).unwrap(), b"partial manifest");
        let result = result(&error, &directory);
        assert_eq!(result["changes"]["state"], "recovery_required");
        assert_eq!(result["retry"], "recover_first");
        assert_eq!(result["changes"]["files"], serde_json::json!([path]));
        assert_eq!(
            result["changes"]["paths"],
            serde_json::json!([{"encoding":"utf8", "value":path}])
        );
        assert_eq!(result["errors"][0]["code"], "MANIFEST_WRITE_FAILED");
        assert_eq!(result["errors"][0]["context"]["io_kind"], "WriteZero");
        assert_eq!(
            result["errors"][0]["context"]["cleanup"]["io_kind"],
            "PermissionDenied"
        );
        assert!(!result.to_string().contains("private write diagnostic"));
        assert!(!result.to_string().contains("private cleanup diagnostic"));
    }
}
