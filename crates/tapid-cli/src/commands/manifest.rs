use crate::application::outcome::{
    ErrorKind, OperationFailure, OperationOutcome, OperationalError,
};
use clap::{Args as ClapArgs, Subcommand};
use std::{path::PathBuf, process::ExitCode};
use tapid_manifest::PackageManifest;

const MAX_MANIFEST_BYTES: usize = 1_048_576;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Check a package.json manifest for valid JSON and supported field values.
    #[command(
        long_about = "Check a package.json manifest for valid JSON and supported field values.\n\nPrint the package name and version on success. Report an error and exit with code 1 if the file cannot be read or the manifest is invalid."
    )]
    Validate {
        /// Manifest file to validate, defaults to package.json in the current directory.
        path: Option<PathBuf>,
    },
}

/// Validate the selected manifest with the same regular-file and size checks used by CLI callers.
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    match args.command {
        Command::Validate { path } => {
            let path = path.unwrap_or_else(|| PathBuf::from("package.json"));
            let project = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            match read_manifest_typed(&path) {
                Ok(manifest) => {
                    if json {
                        return crate::output::json::completed(
                            &OperationOutcome::unchanged(project),
                            "manifest",
                            serde_json::json!({"action":"validate", "valid":true}),
                        );
                    }
                    println!("Valid manifest: {}@{}", manifest.name(), manifest.version());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    if json {
                        return crate::output::json::failure_or_human(
                            &OperationFailure::unchanged(project, error),
                            "manifest",
                            true,
                        );
                    }
                    eprintln!("error: {error}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

pub(crate) fn read_manifest(path: &std::path::Path) -> Result<PackageManifest, String> {
    read_manifest_typed(path).map_err(|error| error.to_string())
}

pub(crate) fn read_manifest_typed(
    path: &std::path::Path,
) -> Result<PackageManifest, OperationalError> {
    let result = (|| {
        let input = crate::filesystem::read::read_bounded_file(path, MAX_MANIFEST_BYTES).map_err(
            |error| match error {
                crate::filesystem::read::ReadError::CapacityExceeded => OperationalError::new(
                    ErrorKind::Manifest,
                    format!("manifest exceeds maximum size of {MAX_MANIFEST_BYTES} bytes"),
                ),
                crate::filesystem::read::ReadError::Io(source)
                    if source.kind() == std::io::ErrorKind::InvalidInput =>
                {
                    OperationalError::new(
                        ErrorKind::Manifest,
                        format!(
                            "manifest must be a regular file, not a symlink: {}",
                            path.display()
                        ),
                    )
                }
                crate::filesystem::read::ReadError::Io(source) => {
                    OperationalError::from_source(ErrorKind::Manifest, source)
                        .context(format!("cannot read manifest {}", path.display()))
                }
            },
        )?;
        let input = String::from_utf8(input).map_err(|source| {
            OperationalError::from_source(ErrorKind::Manifest, source)
                .context(format!("cannot read manifest {}", path.display()))
        })?;
        PackageManifest::parse(&input).map_err(|error| {
            let evidence = error.diagnostic(&input);
            let diagnostic =
                crate::application::outcome::DiagnosticContext::manifest_error(&evidence, path);
            OperationalError::from_source(ErrorKind::Manifest, error).with_diagnostic(diagnostic)
        })
    })();
    result.map_err(|error: OperationalError| error.at_manifest(path))
}
