use sha2::{Digest, Sha256};

pub(crate) const MAX_ARTIFACT_BYTES: usize = 512 * 1024 * 1024;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tapid_archive::{ArchiveFormat, ArchiveLimits, extract_to};

pub(crate) fn unique_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

pub(crate) fn materialize_artifact(name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if name.ends_with(".zip") {
        return Err(
            "verified Windows zip artifacts are unsupported: refusing unsafe extraction".into(),
        );
    }
    if !name.ends_with(".tar.gz") {
        return Err("verified artifact has an unsupported archive format".into());
    }
    let temp = std::env::temp_dir().join(format!(
        "tapid-artifact-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    let result = (|| {
        extract_to(bytes, ArchiveFormat::TarGz, &temp, ArchiveLimits::default())
            .map_err(|e| format!("cannot safely extract verified artifact: {e}"))?;
        let mut entries = fs::read_dir(&temp).map_err(|e| e.to_string())?;
        let first = entries.next().transpose().map_err(|e| e.to_string())?;
        let Some(first) = first else {
            return Err("verified artifact does not contain a tapid executable".into());
        };
        if entries.next().is_some() {
            return Err("verified artifact must contain exactly one member named tapid".into());
        }
        let path = first.path();
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        if name != "tapid" && name != "tapid.exe" {
            return Err("verified artifact must contain exactly one member named tapid".into());
        }
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_file() {
            return Err("verified artifact tapid member must be a regular file".into());
        }
        fs::read(path).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

pub(crate) fn validate_upgrade_destination(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "cannot inspect upgrade destination '{}': {e}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err("upgrade destination must be a regular file".into());
    }
    let marker = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".tapid-managed");
    let marker_metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Err(format!(
                "refusing to replace unmarked non-Tapid-managed destination '{}'; expected {}",
                path.display(),
                marker.display()
            ));
        }
    };
    if !marker_metadata.file_type().is_file() {
        return Err("Tapid ownership marker must be a regular file".into());
    }
    if fs::read(&marker).map_or(true, |bytes| bytes != b"tapid-managed-v1\n") {
        return Err(format!(
            "refusing to replace unmarked non-Tapid-managed destination '{}'; expected {}",
            path.display(),
            marker.display()
        ));
    }
    Ok(())
}

pub(crate) fn replace_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_file_name(format!(
        ".tapid-upgrade-{}-{}",
        std::process::id(),
        unique_nonce()
    ));
    fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        if let Err(error) = fs::set_permissions(&temp, fs::Permissions::from_mode(mode)) {
            let _ = fs::remove_file(&temp);
            return Err(error.to_string());
        }
        fs::rename(&temp, path).map_err(|e| {
            let _ = fs::remove_file(&temp);
            e.to_string()
        })
    }
    #[cfg(windows)]
    {
        let backup = path.with_file_name(format!(
            ".tapid-old-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        fs::rename(path, &backup).map_err(|e| {
            let _ = fs::remove_file(&temp);
            e.to_string()
        })?;
        match fs::rename(&temp, path) {
            Ok(()) => {
                let _ = fs::remove_file(&backup);
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(path);
                let restore = fs::rename(&backup, path);
                if let Err(restore_error) = restore {
                    return Err(format!(
                        "{error}; could not restore old executable: {restore_error}"
                    ));
                }
                let _ = fs::remove_file(&temp);
                Err(error.to_string())
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fs::remove_file(&temp);
        Err("executable replacement is unsupported on this operating system".into())
    }
}
pub(crate) fn replace_lockfile(path: &Path, contents: &str) -> Result<Option<PathBuf>, String> {
    let nonce = format!("{}-{}", std::process::id(), unique_nonce());
    let temp = path.with_file_name(format!(".tapid-lock-{nonce}.tmp"));
    let backup = path.with_file_name(format!(".tapid-lock-{nonce}.bak"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    io::Write::write_all(&mut file, contents.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let had_old = path.exists();
    if had_old {
        fs::rename(path, &backup).map_err(|e| e.to_string())?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        if had_old {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }
    Ok(had_old.then_some(backup))
}

pub(crate) fn rollback_lockfile(path: &Path, backup: Option<&Path>) -> Result<(), String> {
    if let Some(backup) = backup {
        let _ = fs::remove_file(path);
        fs::rename(backup, path).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn discard_lockfile_backup(backup: Option<&Path>) -> Result<(), String> {
    if let Some(backup) = backup {
        fs::remove_file(backup).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256-{}", hex::encode(hasher.finalize()))
}
