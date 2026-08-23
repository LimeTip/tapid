use std::{
    env, fs,
    fs::OpenOptions,
    path::PathBuf,
    process::{Command, ExitCode},
};

const DEFAULT_REPOS: [&str; 2] = ["LimeTip/tapid", "arvid-berndtsson/tapid"];

pub fn upgrade(version: Option<&str>) -> ExitCode {
    if cfg!(windows) {
        eprintln!("error: tapid upgrade is not yet supported on Windows");
        return ExitCode::from(1);
    }

    let repos = env::var("TAPID_REPO")
        .map(|repo| vec![repo])
        .unwrap_or_else(|_| {
            DEFAULT_REPOS
                .iter()
                .map(|repo| (*repo).to_owned())
                .collect()
        });
    if repos.iter().any(|repo| !valid_repo(repo)) {
        eprintln!("error: repository must be OWNER/REPO");
        return ExitCode::from(1);
    }

    let path = match temporary_script_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: cannot create upgrade temporary file: {error}");
            return ExitCode::from(1);
        }
    };

    let mut download_ok = false;
    for repo in &repos {
        let url = format!("https://raw.githubusercontent.com/{repo}/main/scripts/install.sh");
        let download = Command::new("curl")
            .args(["-fsSL", &url, "-o"])
            .arg(&path)
            .status();
        if matches!(download, Ok(status) if status.success()) {
            download_ok = true;
            break;
        }
    }
    if !download_ok {
        let _ = fs::remove_file(&path);
        eprintln!("error: could not download the Tapid installer");
        return ExitCode::from(1);
    }

    let mut command = Command::new("sh");
    command.arg(&path);
    if let Some(version) = version {
        command.args(["--version", version]);
    }
    let result = command.status();
    let _ = fs::remove_file(&path);

    match result {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).try_into().unwrap_or(1)),
        Err(error) => {
            eprintln!("error: could not run the Tapid installer: {error}");
            ExitCode::from(1)
        }
    }
}

fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    parts.next().is_none()
        && !owner.is_empty()
        && !name.is_empty()
        && owner.chars().chain(name.chars()).all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn temporary_script_path() -> std::io::Result<PathBuf> {
    let path = env::temp_dir().join(format!("tapid-upgrade-{}.sh", std::process::id()));
    let _ = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_REPOS, valid_repo};

    #[test]
    fn default_repositories_keep_primary_then_fallback_order() {
        assert_eq!(DEFAULT_REPOS, ["LimeTip/tapid", "arvid-berndtsson/tapid"]);
    }

    #[test]
    fn repository_validation_accepts_owner_and_name() {
        assert!(valid_repo("LimeTip/tapid"));
        assert!(valid_repo("doug-fostery/tapid"));
    }

    #[test]
    fn repository_validation_rejects_url_like_values() {
        assert!(!valid_repo("https://github.com/LimeTip/tapid"));
        assert!(!valid_repo("LimeTip/tapid?x=1"));
        assert!(!valid_repo("LimeTip/tapid/extra"));
    }
}
