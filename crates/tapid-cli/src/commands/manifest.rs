use crate::application::outcome::{ErrorKind, OperationalError};
use clap::{Args as ClapArgs, Subcommand};
use std::{fs::File, io::Read, path::PathBuf, process::ExitCode};
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

pub(crate) fn run(args: Args) -> ExitCode {
    match args.command {
        Command::Validate { path } => {
            let path = path.unwrap_or_else(|| PathBuf::from("package.json"));
            match read_manifest(&path) {
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
    }
}

pub(crate) fn read_manifest(path: &std::path::Path) -> Result<PackageManifest, String> {
    read_manifest_typed(path).map_err(|error| error.to_string())
}

pub(crate) fn read_manifest_typed(
    path: &std::path::Path,
) -> Result<PackageManifest, OperationalError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| {
        OperationalError::from_source(ErrorKind::Manifest, source)
            .context(format!("cannot read manifest {}", path.display()))
    })?;
    if !metadata.file_type().is_file() {
        return Err(OperationalError::new(
            ErrorKind::Manifest,
            format!(
                "manifest must be a regular file, not a symlink: {}",
                path.display()
            ),
        ));
    }
    let file = File::open(path).map_err(|source| {
        OperationalError::from_source(ErrorKind::Manifest, source)
            .context(format!("cannot read manifest {}", path.display()))
    })?;
    let mut input = Vec::with_capacity(MAX_MANIFEST_BYTES + 1);
    file.take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|source| {
            OperationalError::from_source(ErrorKind::Manifest, source)
                .context(format!("cannot read manifest {}", path.display()))
        })?;
    if input.len() > MAX_MANIFEST_BYTES {
        return Err(OperationalError::new(
            ErrorKind::Manifest,
            format!("manifest exceeds maximum size of {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    let input = String::from_utf8(input).map_err(|source| {
        OperationalError::from_source(ErrorKind::Manifest, source)
            .context(format!("cannot read manifest {}", path.display()))
    })?;
    PackageManifest::parse(&input)
        .map_err(|error| OperationalError::from_source(ErrorKind::Manifest, error))
}
