use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) fn unique_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
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
