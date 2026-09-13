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
    Validate { path: Option<PathBuf> },
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
    let file = File::open(path)
        .map_err(|source| format!("cannot read manifest {}: {source}", path.display()))?;
    let mut input = Vec::with_capacity(MAX_MANIFEST_BYTES + 1);
    file.take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|source| format!("cannot read manifest {}: {source}", path.display()))?;
    if input.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "manifest exceeds maximum size of {MAX_MANIFEST_BYTES} bytes"
        ));
    }
    let input = String::from_utf8(input)
        .map_err(|source| format!("cannot read manifest {}: {source}", path.display()))?;
    PackageManifest::parse(&input).map_err(|error| error.to_string())
}
