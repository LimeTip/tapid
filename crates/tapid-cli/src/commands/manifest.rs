use clap::{Args as ClapArgs, Subcommand};
use std::{fs, path::PathBuf, process::ExitCode};
use tapid_manifest::PackageManifest;

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
    let input = fs::read_to_string(path)
        .map_err(|source| format!("cannot read manifest {}: {source}", path.display()))?;
    PackageManifest::parse(&input).map_err(|error| error.to_string())
}
