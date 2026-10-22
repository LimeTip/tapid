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

pub(crate) fn run(args: Args, json_output: bool) -> ExitCode {
    let raw = args.path.as_deref().unwrap_or_else(|| Path::new("."));
    let directory = match fs::canonicalize(raw) {
        Ok(path) => path,
        Err(error) => {
            if json_output {
                return crate::output::json::operation_error("init", "PROJECT_UNAVAILABLE");
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
        Err(CreateManifestError::AlreadyExists(_)) if json_output => {
            crate::output::json::operation_error("init", "MANIFEST_ALREADY_EXISTS")
        }
        Err(CreateManifestError::Unavailable(_)) if json_output => {
            crate::output::json::operation_error("init", "PROJECT_UNAVAILABLE")
        }
        Err(CreateManifestError::Invalid(_)) if json_output => {
            crate::output::json::operation_error("init", "MANIFEST_INVALID")
        }
        Err(CreateManifestError::Write(error)) if json_output => {
            crate::output::json::operation_error_with_context(
                "init",
                "MANIFEST_WRITE_FAILED",
                "operation",
                Some(error),
            )
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

enum CreateManifestError {
    AlreadyExists(String),
    Unavailable(String),
    Invalid(String),
    Write(String),
}

impl std::fmt::Display for CreateManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(message)
            | Self::Unavailable(message)
            | Self::Invalid(message)
            | Self::Write(message) => f.write_str(message),
        }
    }
}

fn create_manifest(directory: &Path) -> Result<(), CreateManifestError> {
    if !directory.is_dir() {
        return Err(CreateManifestError::Unavailable(format!(
            "project path is not a directory: {}",
            directory.display()
        )));
    }
    let name = directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            CreateManifestError::Invalid(format!(
                "project directory has no valid UTF-8 name: {}",
                directory.display()
            ))
        })?;
    let manifest = PackageManifest::new(name, "0.1.0", true).map_err(|error| {
        CreateManifestError::Invalid(format!("cannot create project manifest: {error}"))
    })?;
    let path = directory.join("package.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                CreateManifestError::AlreadyExists(format!(
                    "refusing to overwrite existing manifest: {}",
                    path.display()
                ))
            } else {
                CreateManifestError::Write(format!(
                    "cannot write manifest {}: {error}",
                    path.display()
                ))
            }
        })?;
    if let Err(error) = io::Write::write_all(&mut file, manifest.to_json().as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(CreateManifestError::Write(format!(
            "cannot write manifest {}: {error}",
            path.display()
        )));
    }
    Ok(())
}
