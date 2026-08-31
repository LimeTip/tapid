use fs4::{FileExt, TryLockError};
use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};
const MANAGED_MARKER: &[u8] = b"tapid-managed-v1\n";
const STAGE_OWNER_MARKER: &str = ".tapid-stage-owner";

#[cfg(all(test, unix))]
fn activate_node_modules(project: &Path, stage: &Path) -> Result<(), String> {
    let activation_lock = ActivationLock::acquire(project)?;
    activate_node_modules_with_lock(project, stage, &activation_lock)
}

pub(crate) fn activate_node_modules_with_lock(
    project: &Path,
    stage: &Path,
    activation_lock: &ActivationLock,
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
    let owner = activation_lock.owner_name();
    let marker_backup = project.join(format!(".tapid-managed-old-{owner}"));
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
    let backup = project.join(format!(".tapid-node-modules-old-{owner}"));
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
        let backup_restore = restore_node_modules_backup(&destination, &backup);
        let marker_restore = restore_marker(&marker, &marker_backup, marker_exists);
        let mut error = "refusing to activate a non-directory install staging tree".to_owned();
        if let Err(restore) = backup_restore {
            error.push_str(&format!("; {restore}"));
        }
        if let Err(restore) = marker_restore {
            error.push_str(&format!("; {restore}"));
        }
        return Err(error);
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
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "marker is not a regular file",
        ));
    }
    read_bounded_owner(&mut file)
}

fn read_bounded_owner(reader: &mut impl Read) -> Result<Vec<u8>, io::Error> {
    let mut contents = Vec::with_capacity(129);
    reader.take(129).read_to_end(&mut contents)?;
    if contents.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner value is oversized",
        ));
    }
    Ok(contents)
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

fn restore_node_modules_backup(destination: &Path, backup: &Path) -> Result<(), String> {
    if backup.exists() {
        fs::rename(backup, destination)
            .map_err(|error| format!("cannot restore existing node_modules: {error}"))?;
    }
    Ok(())
}

pub(crate) struct ActivationLock {
    file: fs::File,
    owner: String,
}

impl ActivationLock {
    pub(crate) fn acquire(project: &Path) -> Result<Self, String> {
        let path = project.join(".tapid-activation.lock");
        let mut file = open_lock_file(&path)?;
        FileExt::try_lock(&file).map_err(|error| match error {
            TryLockError::WouldBlock => format!(
                "another node_modules activation is already in progress (lock: {})",
                path.display()
            ),
            TryLockError::Error(error) => {
                format!("cannot acquire node_modules activation lock: {error}")
            }
        })?;
        let previous_owner = read_bounded_owner(&mut file).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                "refusing to recover an oversized node_modules activation lock".to_owned()
            } else {
                format!("cannot read node_modules activation lock: {error}")
            }
        })?;
        let previous_owner = String::from_utf8(previous_owner).map_err(|_| {
            "refusing to recover a malformed node_modules activation lock".to_owned()
        })?;
        if !previous_owner.is_empty() {
            if !activation_owner_is_valid(&previous_owner) {
                return Err("refusing to recover a malformed node_modules activation lock".into());
            }
            recover_owned_activation(project, &previous_owner)?;
            recover_owned_stages(project, &previous_owner)?;
        }
        let owner = format!(
            "{}-{:x}\n",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        );
        file.set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| file.write_all(owner.as_bytes()))
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("cannot initialize node_modules activation lock: {error}"))?;
        Ok(Self { file, owner })
    }

    pub(crate) fn create_stage(&self, project: &Path) -> Result<PathBuf, String> {
        let owner = self
            .owner
            .strip_suffix('\n')
            .expect("activation owner is always newline terminated");
        let stage = project.join(format!(".tapid-install-stage-{owner}"));
        fs::create_dir(&stage)
            .map_err(|error| format!("cannot create install staging directory: {error}"))?;
        if let Err(error) = create_stage_owner_marker(&stage, &self.owner) {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
        Ok(stage)
    }

    fn owner_name(&self) -> &str {
        self.owner
            .strip_suffix('\n')
            .expect("activation owner is always newline terminated")
    }
}

fn create_stage_owner_marker(stage: &Path, owner: &str) -> Result<(), String> {
    let marker = stage.join(STAGE_OWNER_MARKER);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options
        .open(&marker)
        .map_err(|error| format!("cannot create install staging owner marker: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("cannot inspect install staging owner marker: {error}"))?
        .file_type()
        .is_file()
    {
        return Err("install staging owner marker is not a regular file".into());
    }
    file.write_all(owner.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write install staging owner marker: {error}"))
}

impl Drop for ActivationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn open_lock_file(path: &Path) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|error| {
        format!(
            "cannot open node_modules activation lock {}: {error}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect node_modules activation lock: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("refusing to use a non-regular node_modules activation lock".into());
    }
    Ok(file)
}

fn recover_owned_activation(project: &Path, owner: &str) -> Result<(), String> {
    let owner = owner
        .strip_suffix('\n')
        .ok_or_else(|| "cannot recover malformed activation owner".to_owned())?;
    let marker = project.join(".tapid-managed");
    let marker_backup = project.join(format!(".tapid-managed-old-{owner}"));
    let destination = project.join("node_modules");
    let node_backup = project.join(format!(".tapid-node-modules-old-{owner}"));

    let marker_backup_exists = match fs::symlink_metadata(&marker_backup) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if read_marker_backup(&marker_backup)
                .map_err(|error| format!("cannot read stale managed marker: {error}"))?
                != MANAGED_MARKER
            {
                return Err("refusing to recover an invalid stale managed marker".into());
            }
            true
        }
        Ok(_) => return Err("refusing to recover a non-regular stale managed marker".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("cannot inspect stale managed marker: {error}")),
    };
    let marker_exists = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => true,
        Ok(_) => return Err("refusing to recover with a non-regular managed marker".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("cannot inspect managed marker recovery: {error}")),
    };
    if marker_backup_exists {
        if marker_exists {
            fs::remove_file(&marker_backup)
                .map_err(|error| format!("cannot remove stale managed marker: {error}"))?;
        } else {
            fs::rename(&marker_backup, &marker)
                .map_err(|error| format!("cannot restore managed marker: {error}"))?;
        }
    }

    let node_backup_exists = match fs::symlink_metadata(&node_backup) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => return Err("refusing to recover a non-directory node_modules backup".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("cannot inspect node_modules backup: {error}")),
    };
    if node_backup_exists {
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&node_backup)
                    .map_err(|error| format!("cannot remove stale node_modules backup: {error}"))?;
            }
            Ok(_) => {
                return Err(
                    "refusing to recover with a non-directory node_modules destination".into(),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::rename(&node_backup, &destination).map_err(|error| {
                    format!("cannot restore stale node_modules backup: {error}")
                })?;
            }
            Err(error) => {
                return Err(format!("cannot inspect node_modules recovery: {error}"));
            }
        }
    }
    Ok(())
}

fn recover_owned_stages(project: &Path, owner: &str) -> Result<(), String> {
    let owner_name = owner
        .strip_suffix('\n')
        .ok_or_else(|| "cannot recover malformed install stage owner".to_owned())?;
    let ownerless_stage_name = format!(".tapid-install-stage-{owner_name}");
    for entry in fs::read_dir(project)
        .map_err(|error| format!("cannot inspect project for stale install stages: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("cannot inspect stale install stage: {error}"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(".tapid-install-stage-") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("cannot inspect stale install stage: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let marker = entry.path().join(STAGE_OWNER_MARKER);
        let marker_metadata = match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if name == ownerless_stage_name {
                    fs::remove_dir_all(entry.path()).map_err(|error| {
                        format!("cannot remove ownerless stale install stage: {error}")
                    })?;
                }
                continue;
            }
            Err(error) => return Err(format!("cannot inspect stale stage ownership: {error}")),
        };
        if marker_metadata.len() > 128 {
            continue;
        }
        if read_marker_backup(&marker)
            .map_err(|error| format!("cannot read stale stage ownership: {error}"))?
            == owner.as_bytes()
        {
            fs::remove_dir_all(entry.path())
                .map_err(|error| format!("cannot remove stale install stage: {error}"))?;
        }
    }
    Ok(())
}

fn activation_owner_is_valid(owner: &str) -> bool {
    let Some(owner) = owner.strip_suffix('\n') else {
        return false;
    };
    let Some((pid, nonce)) = owner.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !nonce.is_empty()
        && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(all(test, unix))]
mod activation_tests {
    use super::{ActivationLock, MANAGED_MARKER, activate_node_modules, recover_owned_activation};
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
    fn stale_activation_backups_restore_the_previous_layout() {
        let project = temp_project("activation-backup-rollback");
        let owner = "123-deadbeef\n";
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join(".tapid-managed-old-123-deadbeef"),
            MANAGED_MARKER,
        )
        .unwrap();
        let backup = project.join(".tapid-node-modules-old-123-deadbeef");
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("old"), b"old").unwrap();

        recover_owned_activation(&project, owner).unwrap();

        assert_eq!(
            fs::read(project.join(".tapid-managed")).unwrap(),
            MANAGED_MARKER
        );
        assert_eq!(fs::read(project.join("node_modules/old")).unwrap(), b"old");
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn stale_activation_backups_roll_forward_an_activated_layout() {
        let project = temp_project("activation-backup-roll-forward");
        let owner = "123-deadbeef\n";
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join(".tapid-managed-old-123-deadbeef"),
            MANAGED_MARKER,
        )
        .unwrap();
        let backup = project.join(".tapid-node-modules-old-123-deadbeef");
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("old"), b"old").unwrap();
        let destination = project.join("node_modules");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("new"), b"new").unwrap();

        recover_owned_activation(&project, owner).unwrap();

        assert_eq!(
            fs::read(project.join(".tapid-managed")).unwrap(),
            MANAGED_MARKER
        );
        assert_eq!(fs::read(destination.join("new")).unwrap(), b"new");
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn existing_activation_lock_error_includes_the_lock_path() {
        let project = temp_project("lock-path");
        let lock_path = project.join(".tapid-activation.lock");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir(&lock_path).unwrap();

        let error = match ActivationLock::acquire(&project) {
            Ok(_) => panic!("existing lock must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains(&lock_path.display().to_string()));
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn stale_file_lock_reclaims_only_its_owned_stage() {
        let project = temp_project("stale-lock-recovery");
        let lock_path = project.join(".tapid-activation.lock");
        let stale_stage = project.join(".tapid-install-stage-stale");
        let unrelated_stage = project.join(".tapid-install-stage-unrelated");
        fs::create_dir_all(&stale_stage).unwrap();
        fs::create_dir_all(&unrelated_stage).unwrap();
        fs::write(&lock_path, b"123-deadbeef\n").unwrap();
        fs::write(stale_stage.join(".tapid-stage-owner"), b"123-deadbeef\n").unwrap();
        fs::write(
            unrelated_stage.join(".tapid-stage-owner"),
            b"different-owner\n",
        )
        .unwrap();

        let lock = ActivationLock::acquire(&project).unwrap();

        assert!(!stale_stage.exists());
        assert!(unrelated_stage.exists());
        drop(lock);
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn ownerless_pre_marker_stage_is_recovered_from_its_lock_owner_name() {
        let project = temp_project("ownerless-stage-recovery");
        let stale = project.join(".tapid-install-stage-123-deadbeef");
        let unrelated = project.join(".tapid-install-stage-456-cafebabe");
        fs::create_dir_all(&stale).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(project.join(".tapid-activation.lock"), b"123-deadbeef\n").unwrap();

        let lock = ActivationLock::acquire(&project).unwrap();

        assert!(!stale.exists());
        assert!(unrelated.is_dir());
        drop(lock);
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn live_file_lock_preserves_its_stage_and_rejects_a_second_owner() {
        let project = temp_project("live-lock-recovery");
        fs::create_dir_all(&project).unwrap();
        let first = ActivationLock::acquire(&project).unwrap();
        let stage = first.create_stage(&project).unwrap();

        let error = match ActivationLock::acquire(&project) {
            Ok(_) => panic!("live lock must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("already in progress"));
        assert!(stage.is_dir());
        drop(first);
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn malformed_stale_lock_cannot_claim_a_stage() {
        let project = temp_project("malformed-stale-lock");
        let stage = project.join(".tapid-install-stage-untrusted");
        fs::create_dir_all(&stage).unwrap();
        fs::write(project.join(".tapid-activation.lock"), b"not-an-owner").unwrap();
        fs::write(stage.join(".tapid-stage-owner"), b"not-an-owner").unwrap();

        let error = match ActivationLock::acquire(&project) {
            Ok(_) => panic!("malformed stale lock must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("malformed"));
        assert!(stage.is_dir());
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn oversized_stale_lock_is_rejected_before_recovery() {
        let project = temp_project("oversized-stale-lock");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(".tapid-activation.lock"), vec![b'a'; 129]).unwrap();

        let error = match ActivationLock::acquire(&project) {
            Ok(_) => panic!("oversized stale lock must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("oversized"));
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn stage_owner_marker_creation_does_not_follow_a_symlink() {
        use super::create_stage_owner_marker;

        let project = temp_project("stage-owner-symlink");
        let stage = project.join("stage");
        let target = project.join("target");
        fs::create_dir_all(&stage).unwrap();
        fs::write(&target, b"unchanged").unwrap();
        symlink(&target, stage.join(".tapid-stage-owner")).unwrap();

        let error = create_stage_owner_marker(&stage, "123-deadbeef\n").unwrap_err();

        assert!(error.contains("cannot create install staging owner marker"));
        assert_eq!(fs::read(&target).unwrap(), b"unchanged");
        let _ = fs::remove_dir_all(project);
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

    #[test]
    fn non_directory_staging_failure_restores_node_modules_and_marker() {
        let project = temp_project("restore-node-modules");
        let stage = project.join("stage");
        let external = project.join("external");
        fs::create_dir_all(project.join("node_modules")).unwrap();
        fs::write(project.join("node_modules/retained"), b"old layout").unwrap();
        fs::write(project.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&external).unwrap();
        symlink(&external, stage.join("node_modules")).unwrap();

        let error = activate_node_modules(&project, &stage).unwrap_err();

        assert!(error.contains("non-directory install staging tree"));
        assert_eq!(
            fs::read(project.join("node_modules/retained")).unwrap(),
            b"old layout"
        );
        assert_eq!(
            fs::read(project.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        let _ = fs::remove_dir_all(project);
    }
}
