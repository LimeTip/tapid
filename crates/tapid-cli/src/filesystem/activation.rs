use std::{
    fs, io,
    path::{Path, PathBuf},
};
const MANAGED_MARKER: &[u8] = b"tapid-managed-v1\n";

#[cfg(all(test, unix))]
fn activate_node_modules(project: &Path, stage: &Path) -> Result<(), String> {
    let activation_lock = ActivationLock::acquire(project)?;
    activate_node_modules_with_lock(project, stage, &activation_lock)
}

pub(crate) fn activate_node_modules_with_lock(
    project: &Path,
    stage: &Path,
    _activation_lock: &ActivationLock,
) -> Result<(), String> {
    let staged = stage.join("node_modules");
    if !staged.is_dir() {
        return Err("install transaction produced an empty node_modules layout".into());
    }
    let destination = project.join("node_modules");
    let marker = project.join(".tapid-managed");
    let marker_exists = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => true,
        Ok(_) => {
            return Err("refusing to use a non-regular .tapid-managed marker".to_owned());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("cannot inspect .tapid-managed: {error}")),
    };
    if destination.exists() && !marker_exists {
        return Err(
            "refusing to replace unmarked node_modules; create .tapid-managed to opt in".into(),
        );
    }
    let marker_backup = project.join(format!(
        ".tapid-managed-old-{}-{}",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    // Move the existing marker by name before reading it to close the check/read race.
    if marker_exists {
        fs::rename(&marker, &marker_backup)
            .map_err(|e| format!("cannot stage .tapid-managed marker: {e}"))?;
        let contents = match read_marker_backup(&marker_backup) {
            Ok(contents) => contents,
            Err(error) => {
                let restore = restore_marker(&marker, &marker_backup, marker_exists);
                return Err(match restore {
                    Ok(()) => format!("cannot read .tapid-managed: {error}"),
                    Err(restore) => format!("cannot read .tapid-managed: {error}; {restore}"),
                });
            }
        };
        let still_regular = fs::symlink_metadata(&marker_backup)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false);
        if !still_regular || contents != MANAGED_MARKER {
            let restore = restore_marker(&marker, &marker_backup, marker_exists);
            let reason = if still_regular {
                "refusing to replace node_modules with an invalid .tapid-managed marker".into()
            } else {
                "refusing to use a non-regular .tapid-managed marker".into()
            };
            return Err(match restore {
                Ok(()) => reason,
                Err(restore) => format!("{reason}; {restore}"),
            });
        }
    }
    let backup = project.join(format!(
        ".tapid-node-modules-old-{}-{}",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    if destination.exists()
        && let Err(error) = fs::rename(&destination, &backup)
    {
        let restore = restore_marker(&marker, &marker_backup, marker_exists);
        return Err(match restore {
            Ok(()) => format!("cannot stage existing node_modules for replacement: {error}"),
            Err(restore) => {
                format!("cannot stage existing node_modules for replacement: {error}; {restore}")
            }
        });
    }
    if std::env::var_os("TAPID_TEST_FAIL_ACTIVATION").is_some() {
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        let restore = restore_marker(&marker, &marker_backup, marker_exists);
        return Err(match restore {
            Ok(()) => "install activation failed (injected)".into(),
            Err(restore) => format!("install activation failed (injected); {restore}"),
        });
    }
    let staged_is_directory = fs::symlink_metadata(&staged)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false);
    if !staged_is_directory {
        let restore = restore_marker(&marker, &marker_backup, marker_exists);
        return Err(match restore {
            Ok(()) => "refusing to activate a non-directory install staging tree".into(),
            Err(restore) => {
                format!("refusing to activate a non-directory install staging tree; {restore}")
            }
        });
    }
    if let Err(error) = fs::rename(&staged, &destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        let restore = restore_marker(&marker, &marker_backup, marker_exists);
        return Err(match restore {
            Ok(()) => format!("cannot activate node_modules: {error}"),
            Err(restore) => format!("cannot activate node_modules: {error}; {restore}"),
        });
    }
    let destination_is_directory = fs::symlink_metadata(&destination)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false);
    if !destination_is_directory {
        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir_all(&destination);
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        let restore = restore_marker(&marker, &marker_backup, marker_exists);
        return Err(match restore {
            Ok(()) => "refusing to activate a symlinked install staging tree".into(),
            Err(restore) => {
                format!("refusing to activate a symlinked install staging tree; {restore}")
            }
        });
    }
    let marker_temp = project.join(format!(
        ".tapid-managed-{}-{}.tmp",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    let marker_result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker_temp)
        .and_then(|mut file| {
            io::Write::write_all(&mut file, MANAGED_MARKER)?;
            file.sync_all()?;
            fs::rename(&marker_temp, &marker)
        });
    if let Err(error) = marker_result {
        let _ = fs::remove_file(&marker_temp);
        let _ = fs::remove_dir_all(&destination);
        if backup.exists() {
            let _ = fs::rename(&backup, &destination);
        }
        let restore = restore_marker(&marker, &marker_backup, marker_exists);
        if !marker_exists {
            let _ = fs::remove_file(&marker);
        }
        return Err(match restore {
            Ok(()) => format!("cannot write .tapid-managed: {error}"),
            Err(restore) => format!("cannot write .tapid-managed: {error}; {restore}"),
        });
    }
    if marker_exists {
        let _ = fs::remove_file(&marker_backup);
    }
    let _ = fs::remove_dir_all(&backup);
    let _ = fs::remove_dir_all(stage);
    Ok(())
}

fn read_marker_backup(path: &Path) -> Result<Vec<u8>, io::Error> {
    use io::Read;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        Ok(contents)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        Ok(contents)
    }
    #[cfg(not(any(unix, windows)))]
    fs::read(path)
}

fn restore_marker(marker: &Path, backup: &Path, marker_exists: bool) -> Result<(), String> {
    if marker_exists {
        match fs::symlink_metadata(backup) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                let _ = fs::remove_file(backup);
                return Err(
                    "cannot restore .tapid-managed: staged marker is not a regular file".into(),
                );
            }
            Err(error) => return Err(format!("cannot restore .tapid-managed: {error}")),
        }
        fs::rename(backup, marker)
            .map_err(|error| format!("cannot restore .tapid-managed: {error}"))
    } else {
        Ok(())
    }
}

pub(crate) struct ActivationLock {
    path: PathBuf,
}

impl ActivationLock {
    pub(crate) fn acquire(project: &Path) -> Result<Self, String> {
        let path = project.join(".tapid-activation.lock");
        fs::create_dir(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "another node_modules activation is already in progress".to_owned()
            } else {
                format!("cannot acquire node_modules activation lock: {error}")
            }
        })?;
        Ok(Self { path })
    }
}

impl Drop for ActivationLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(all(test, unix))]
mod activation_tests {
    use super::activate_node_modules;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_project(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tapid-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn symlinked_marker_is_rejected_without_touching_target() {
        let project = temp_project("marker-symlink");
        let stage = project.join("stage");
        let target = project.join("attacker-target");
        fs::create_dir_all(stage.join("node_modules")).unwrap();
        fs::write(&target, b"must remain unchanged").unwrap();
        symlink(&target, project.join(".tapid-managed")).unwrap();

        let error = activate_node_modules(&project, &stage).unwrap_err();

        assert!(error.contains("non-regular"));
        assert_eq!(fs::read(&target).unwrap(), b"must remain unchanged");
        assert!(
            fs::symlink_metadata(project.join(".tapid-managed"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(project);
    }
}
