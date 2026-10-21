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
                return crate::output::json::operation_error("init", "PROJECT_UNAVAILABLE", None);
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
            let (code, context) = match &error {
                CreationError::Unavailable(_) => ("PROJECT_UNAVAILABLE", None),
                CreationError::Exists(_) => ("MANIFEST_ALREADY_EXISTS", None),
                CreationError::Invalid(_) => ("MANIFEST_INVALID", None),
                CreationError::Write(_, source) => (
                    "MANIFEST_WRITE_FAILED",
                    Some(serde_json::json!({
                        "io_kind": format!("{:?}", source.kind()),
                        "os_code": source.raw_os_error(),
                    })),
                ),
            };
            crate::output::json::operation_error("init", code, context)
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
            Self::Write(path, source) => {
                write!(f, "cannot write manifest {}: {source}", path.display())
            }
        }
    }
}

/// Create a private package manifest without overwriting an existing entry.
/// Distinguish unavailable directories, invalid names, existing entries, and I/O failures.
/// After a write failure, attempt to remove the new file; cleanup is best effort.
fn create_manifest(directory: &Path) -> Result<(), CreationError> {
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
    if let Err(error) = io::Write::write_all(&mut file, manifest.to_json().as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(CreationError::Write(path, error));
    }
    Ok(())
}
