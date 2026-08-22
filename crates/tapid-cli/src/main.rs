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
    /// Replay the project's validated lockfile without network access.
    Install {
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        frozen: bool,
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,
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
        Some(Command::Install {
            offline,
            frozen,
            project_dir,
        }) => install(&project_dir, offline, frozen),
    }
}

fn install(project_dir: &Path, offline: bool, frozen: bool) -> ExitCode {
    let project_dir = match fs::canonicalize(project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            eprintln!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                project_dir.display()
            );
            return ExitCode::from(1);
        }
    };
    let manifest_path = project_dir.join("package.json");
    if let Err(error) = read_manifest(&manifest_path) {
        eprintln!("error: {error}");
        return ExitCode::from(1);
    }
    let lock_path = project_dir.join("tapid.lock");
    if !lock_path.is_file() {
        let message = if offline {
            "offline install requires tapid.lock"
        } else if frozen {
            "frozen install requires tapid.lock"
        } else {
            "cannot install without tapid.lock; network resolution is unavailable"
        };
        eprintln!("error: {message}: {}", lock_path.display());
        return ExitCode::from(1);
    }
    // Validate the complete replay input before touching project output.
    let lock = match fs::read_to_string(&lock_path)
        .map_err(|error| format!("cannot read lockfile {}: {error}", lock_path.display()))
        .and_then(|input| {
            Lockfile::from_json(&input)
                .map_err(|error| format!("invalid lockfile {}: {error}", lock_path.display()))
        }) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let modules = project_dir.join("node_modules");
    if let Err(error) = fs::create_dir_all(&modules) {
        eprintln!("error: cannot create {}: {error}", modules.display());
        return ExitCode::from(1);
    }
    // Replay never executes package lifecycle scripts.
    println!("Replayed lockfile: {} package(s)", lock.packages().len());
    ExitCode::SUCCESS
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
