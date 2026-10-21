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
        Err(error) if json_output => {
            let code = if error.starts_with("refusing to overwrite existing manifest") {
                "MANIFEST_ALREADY_EXISTS"
            } else if error.starts_with("project path is not a directory") {
                "PROJECT_UNAVAILABLE"
            } else {
                "MANIFEST_INVALID"
            };
            crate::output::json::operation_error("init", code)
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
