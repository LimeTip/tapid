use clap::{Parser, Subcommand};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;

#[derive(Debug, Parser)]
#[command(
    name = "tapid",
    version,
    about = "A deterministic JavaScript and TypeScript package manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Create a private package.json manifest.
    Init { path: Option<PathBuf> },
    Manifest {
        #[command(subcommand)]
        command: ManifestCommand,
    },
    Lock {
        #[command(subcommand)]
        command: LockCommand,
    },
}
#[derive(Debug, Subcommand)]
enum ManifestCommand {
    Validate { path: Option<PathBuf> },
}
#[derive(Debug, Subcommand)]
enum LockCommand {
    Verify,
}

fn main() -> ExitCode {
    dispatch(Cli::parse())
}

fn dispatch(cli: Cli) -> ExitCode {
    match cli.command {
        None => {
            println!("Run 'tapid --help' for usage");
            ExitCode::SUCCESS
        }
        Some(Command::Init { path }) => init(path.as_deref()),
        Some(Command::Manifest {
            command: ManifestCommand::Validate { path },
        }) => validate(path.as_deref().unwrap_or(Path::new("package.json"))),
        Some(Command::Lock {
            command: LockCommand::Verify,
        }) => verify_lock(Path::new("tapid.lock")),
    }
}

fn validate(path: &Path) -> ExitCode {
    match read_manifest(path) {
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
fn read_manifest(path: &Path) -> Result<PackageManifest, String> {
    let input = fs::read_to_string(path)
        .map_err(|source| format!("cannot read manifest {}: {source}", path.display()))?;
    PackageManifest::parse(&input).map_err(|error| error.to_string())
}
fn init(path: Option<&Path>) -> ExitCode {
    let raw = path.unwrap_or_else(|| Path::new("."));
    let directory = match fs::canonicalize(raw) {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                raw.display()
            );
            return ExitCode::from(1);
        }
    };
    match create_manifest(&directory) {
        Ok(()) => {
            println!("Created {}", directory.join("package.json").display());
            ExitCode::SUCCESS
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
fn verify_lock(path: &Path) -> ExitCode {
    match fs::read_to_string(path)
        .map_err(|error| error.to_string())
        .and_then(|input| Lockfile::from_json(&input).map_err(|error| error.to_string()))
    {
        Ok(_) => {
            println!("Valid lockfile: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: cannot verify {}: {error}", path.display());
            ExitCode::from(1)
        }
    }
}
