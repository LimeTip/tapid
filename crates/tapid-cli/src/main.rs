use std::{env, fmt, fs, io, path::Path};

use tapid_manifest::PackageManifest;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None | Some("--help") | Some("-h") => print_help(),
        Some("--version") | Some("-V") => println!("tapid {VERSION}"),
        Some("init") => {
            let path = args.next().unwrap_or_else(|| ".".to_owned());
            if args.next().is_some() {
                eprintln!("error: tapid init accepts at most one project directory");
                std::process::exit(2);
            }
            let directory = match fs::canonicalize(&path) {
                Ok(directory) => directory,
                Err(error) => {
                    eprintln!("error: cannot access project directory '{path}': {error}");
                    std::process::exit(1);
                }
            };
            match init_project(&directory) {
                Ok(()) => println!("Created {}", directory.join("package.json").display()),
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(1);
                }
            }
        }
        Some(command) => {
            eprintln!("error: unknown command '{command}'");
            eprintln!("run 'tapid --help' for usage");
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "Tapid, a deterministic JavaScript and TypeScript package manager\n\nUsage:\n  tapid [OPTIONS]\n  tapid init [PATH]\n\nCommands:\n  init [PATH]       Create a private package.json manifest\n\nOptions:\n  -h, --help       Print help information\n  -V, --version    Print version information"
    );
}

fn init_project(directory: &Path) -> Result<(), InitError> {
    if !directory.is_dir() {
        return Err(InitError::NotDirectory(directory.to_owned()));
    }

    let name = directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| InitError::InvalidProjectDirectory(directory.to_owned()))?;
    let manifest = PackageManifest::new(name, "0.1.0", true).map_err(InitError::Manifest)?;
    let manifest_path = directory.join("package.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)
        .map_err(|error| InitError::ManifestFile(manifest_path.clone(), error))?;

    if let Err(error) = io::Write::write_all(&mut file, manifest.to_json().as_bytes()) {
        let _ = fs::remove_file(&manifest_path);
        return Err(InitError::ManifestFile(manifest_path, error));
    }
    Ok(())
}

#[derive(Debug)]
enum InitError {
    NotDirectory(std::path::PathBuf),
    InvalidProjectDirectory(std::path::PathBuf),
    Manifest(tapid_manifest::ManifestError),
    ManifestFile(std::path::PathBuf, io::Error),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDirectory(path) => {
                write!(f, "project path is not a directory: {}", path.display())
            }
            Self::InvalidProjectDirectory(path) => {
                write!(
                    f,
                    "project directory has no valid UTF-8 name: {}",
                    path.display()
                )
            }
            Self::Manifest(error) => write!(f, "cannot create project manifest: {error}"),
            Self::ManifestFile(path, error) if error.kind() == io::ErrorKind::AlreadyExists => {
                write!(
                    f,
                    "refusing to overwrite existing manifest: {}",
                    path.display()
                )
            }
            Self::ManifestFile(path, error) => {
                write!(f, "cannot write manifest {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for InitError {}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::init_project;

    #[test]
    fn package_version_is_set() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }

    #[test]
    fn init_creates_a_private_manifest_for_an_empty_directory() {
        let directory = test_directory("creates");
        fs::create_dir_all(&directory).unwrap();

        init_project(&directory).unwrap();

        let name = directory.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("package.json")).unwrap(),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}}\n"
            )
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn init_refuses_to_overwrite_an_existing_manifest() {
        let directory = test_directory("existing");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("package.json"), "original\n").unwrap();

        assert!(init_project(&directory).is_err());
        assert_eq!(
            fs::read_to_string(directory.join("package.json")).unwrap(),
            "original\n"
        );
        let _ = fs::remove_dir_all(directory);
    }

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tapid-init-{name}-{}", std::process::id()))
    }
}
