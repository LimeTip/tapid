use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

const JOURNAL: &str = ".tapid-lifecycle-journal.json";
const MAX_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ENCODED_SNAPSHOT_BYTES: u64 = MAX_SNAPSHOT_BYTES.div_ceil(3) * 4;
// Must match tapid-store's COORDINATOR_MAX.
const MAX_JOURNAL_BYTES: u64 = 96 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    owner: String,
    state: State,
    manifest_existed: bool,
    manifest: String,
    lock_existed: bool,
    lock: String,
    marker_existed: bool,
    node_modules_existed: bool,
    store_root: Option<String>,
}

#[derive(Serialize, Deserialize, PartialEq)]
enum State {
    Prepared,
    Committed,
}

pub(crate) struct LifecycleJournal {
    project: std::path::PathBuf,
    record: Record,
}

impl LifecycleJournal {
    pub(crate) fn begin(
        project: &Path,
        owner: &str,
        manifest: &[u8],
        lock: Option<&[u8]>,
    ) -> Result<Self, String> {
        if !valid_owner(owner) {
            return Err("refusing to create lifecycle journal with invalid owner".into());
        }
        if manifest.len() as u64 > MAX_SNAPSHOT_BYTES
            || lock.is_some_and(|bytes| bytes.len() as u64 > MAX_SNAPSHOT_BYTES)
        {
            return Err("lifecycle recovery state exceeds size limit".into());
        }
        let path = project.join(JOURNAL);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err("an unfinished lifecycle transaction requires recovery".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot inspect lifecycle journal: {error}")),
        }
        let marker_existed = exists(project.join(".tapid-managed"))?;
        let node_modules_existed = exists(project.join("node_modules"))?;
        let record = Record {
            version: 1,
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(manifest),
            lock_existed: lock.is_some(),
            lock: lock.map(|bytes| STANDARD.encode(bytes)).unwrap_or_default(),
            marker_existed,
            node_modules_existed,
            store_root: None,
        };
        write_record(project, &record)?;
        Ok(Self {
            project: project.to_owned(),
            record,
        })
    }

    pub(crate) fn set_store_root(&mut self, store_root: &Path) -> Result<(), String> {
        let canonical = store_root
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize store root for recovery: {error}"))?;
        let value = canonical
            .to_str()
            .ok_or("store root path is not UTF-8")?
            .to_owned();
        self.record.store_root = Some(value);
        write_record(&self.project, &self.record)
    }

    pub(crate) fn coordinator_path(&self) -> std::path::PathBuf {
        self.project.join(JOURNAL)
    }

    pub(crate) fn mark_committed(&mut self) -> Result<(), String> {
        self.record.state = State::Committed;
        write_record(&self.project, &self.record)
    }

    pub(crate) fn finish(self) -> Result<(), String> {
        crate::filesystem::activation::commit_owned_activation(&self.project, &self.record.owner)?;
        remove_journal(&self.project)
    }
}

impl Drop for LifecycleJournal {
    fn drop(&mut self) {
        let recovered = (|| {
            let Some(decision) = recover(&self.project, Some(&self.record.owner))? else {
                return Ok(());
            };
            if let Some(store_root) = &self.record.store_root {
                tapid_store::Store::new(store_root)
                    .recover_transactions()
                    .map_err(|error| format!("cannot recover shared store transaction: {error}"))?;
            }
            if !decision.committed {
                recover_record(&self.project, &self.record)?;
            }
            crate::filesystem::activation::recover_owned_activation(
                &self.project,
                &decision.owner,
                Some(decision.clone()),
            )?;
            crate::filesystem::activation::recover_owned_stages(&self.project, &decision.owner)?;
            remove_journal(&self.project)
        })();
        let _ = recovered;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RecoveryDecision {
    pub(crate) owner: String,
    pub(crate) committed: bool,
    pub(crate) marker_existed: bool,
    pub(crate) node_modules_existed: bool,
}

pub(crate) fn has_pending(project: &Path) -> Result<bool, String> {
    let path = project.join(JOURNAL);
    let backup = journal_backup_path(&path);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= MAX_JOURNAL_BYTES => {
            Ok(true)
        }
        Ok(metadata) if metadata.file_type().is_file() => {
            Err("refusing to inspect an oversized lifecycle journal".into())
        }
        Ok(_) => Err("refusing to inspect a non-regular lifecycle journal".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::symlink_metadata(&backup) {
                Ok(metadata)
                    if metadata.file_type().is_file() && metadata.len() <= MAX_JOURNAL_BYTES =>
                {
                    Ok(true)
                }
                Ok(metadata) if metadata.file_type().is_file() => {
                    Err("refusing to inspect an oversized lifecycle journal backup".into())
                }
                Ok(_) => Err("refusing to inspect a non-regular lifecycle journal backup".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(format!("cannot inspect lifecycle journal backup: {error}")),
            }
        }
        Err(error) => Err(format!("cannot inspect lifecycle journal: {error}")),
    }
}

pub(crate) fn recover(
    project: &Path,
    stale_owner: Option<&str>,
) -> Result<Option<RecoveryDecision>, String> {
    let path = project.join(JOURNAL);
    let backup = journal_backup_path(&path);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => return Err("refusing to recover a non-regular lifecycle journal".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::symlink_metadata(&backup) {
                Ok(metadata)
                    if metadata.file_type().is_file() && metadata.len() <= MAX_JOURNAL_BYTES =>
                {
                    fs::rename(&backup, &path).map_err(|error| {
                        format!("cannot restore interrupted lifecycle journal update: {error}")
                    })?;
                    sync_parent(&path)?;
                    fs::symlink_metadata(&path).map_err(|error| {
                        format!("cannot inspect restored lifecycle journal: {error}")
                    })?
                }
                Ok(metadata) if metadata.file_type().is_file() => {
                    return Err("refusing to recover an oversized lifecycle journal backup".into());
                }
                Ok(_) => {
                    return Err("refusing to recover a non-regular lifecycle journal backup".into());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(format!("cannot inspect lifecycle journal backup: {error}"));
                }
            }
        }
        Err(error) => return Err(format!("cannot inspect lifecycle journal: {error}")),
    };
    if metadata.len() > MAX_JOURNAL_BYTES {
        return Err("refusing to recover an oversized lifecycle journal".into());
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("cannot read lifecycle journal: {error}"))?;
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|error| format!("refusing to recover a malformed lifecycle journal: {error}"))?;
    if record.version != 1
        || !valid_owner(&record.owner)
        || stale_owner.is_some_and(|owner| record.owner != owner)
    {
        return Err("refusing to recover a lifecycle journal with invalid ownership".into());
    }
    if record.manifest.len() as u64 > MAX_ENCODED_SNAPSHOT_BYTES
        || record.lock.len() as u64 > MAX_ENCODED_SNAPSHOT_BYTES
    {
        return Err("refusing to recover oversized lifecycle state".into());
    }
    if let Some(store_root) = &record.store_root {
        let path = Path::new(store_root);
        if !path.is_absolute() || path.canonicalize().as_deref().ok() != Some(path) {
            return Err(
                "refusing to recover lifecycle journal with noncanonical store root".into(),
            );
        }
        tapid_store::Store::new(path)
            .recover_transactions()
            .map_err(|error| format!("cannot recover shared store transaction: {error}"))?;
    }
    if record.state == State::Prepared {
        recover_record(project, &record)?;
    }
    Ok(Some(RecoveryDecision {
        owner: record.owner,
        committed: record.state == State::Committed,
        marker_existed: record.marker_existed,
        node_modules_existed: record.node_modules_existed,
    }))
}

pub(crate) fn finish_recovery(project: &Path) -> Result<(), String> {
    remove_journal(project)
}

fn journal_backup_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("lifecycle.bak")
}

fn clear_journal_backup(path: &Path) -> Result<(), String> {
    let backup = journal_backup_path(path);
    match fs::symlink_metadata(&backup) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(&backup).map_err(|error| {
                format!("cannot remove stale lifecycle journal backup: {error}")
            })?;
            sync_parent(path)
        }
        Ok(_) => Err("refusing to remove a non-regular lifecycle journal backup".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect lifecycle journal backup: {error}")),
    }
}

fn recover_record(project: &Path, record: &Record) -> Result<(), String> {
    let manifest = STANDARD
        .decode(&record.manifest)
        .map_err(|error| format!("invalid manifest bytes in lifecycle journal: {error}"))?;
    atomic_replace(&project.join("package.json"), &manifest)?;
    let lock_path = project.join("tapid.lock");
    if record.lock_existed {
        let lock = STANDARD
            .decode(&record.lock)
            .map_err(|error| format!("invalid lockfile bytes in lifecycle journal: {error}"))?;
        atomic_replace(&lock_path, &lock)?;
    } else {
        match fs::remove_file(&lock_path) {
            Ok(()) => sync_parent(&lock_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot restore prior lockfile state: {error}")),
        }
    }
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("lifecycle path has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid lifecycle filename")?;
    let temp = parent.join(format!(
        ".{name}.lifecycle-{}-{}.tmp",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| error.to_string())?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        if path.exists() {
            let backup = temp.with_extension("bak");
            fs::rename(path, &backup).map_err(|error| error.to_string())?;
            if let Err(error) = fs::rename(&temp, path) {
                let _ = fs::rename(&backup, path);
                return Err(error.to_string());
            }
            fs::remove_file(backup).map_err(|error| error.to_string())?;
        } else {
            fs::rename(&temp, path).map_err(|error| error.to_string())?;
        }
        #[cfg(not(windows))]
        fs::rename(&temp, path).map_err(|error| error.to_string())?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn write_record(project: &Path, record: &Record) -> Result<(), String> {
    let path = project.join(JOURNAL);
    let temp = project.join(format!(
        "{JOURNAL}.{}.tmp",
        crate::filesystem::atomic::unique_nonce()
    ));
    let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err("lifecycle recovery state exceeds encoded size limit".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| format!("cannot create lifecycle journal: {error}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot persist lifecycle journal: {error}"))?;
    replace_journal_file(&temp, &path, false)
        .map_err(|error| format!("cannot publish lifecycle journal: {error}"))?;
    sync_parent(&path)
}

fn replace_journal_file(temp: &Path, target: &Path, force_fallback: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    let _ = force_fallback;
    if !target.exists() {
        return fs::rename(temp, target);
    }

    #[cfg(not(windows))]
    if !force_fallback {
        return fs::rename(temp, target);
    }

    let backup = journal_backup_path(target);
    if backup.exists() {
        if target.exists() {
            fs::remove_file(&backup)?;
        } else {
            fs::rename(&backup, target)?;
        }
    }
    fs::rename(target, &backup)?;
    sync_parent(target).map_err(std::io::Error::other)?;
    if let Err(error) = fs::rename(temp, target) {
        let _ = fs::rename(&backup, target);
        let _ = sync_parent(target);
        return Err(error);
    }
    sync_parent(target).map_err(std::io::Error::other)?;
    fs::remove_file(backup)?;
    sync_parent(target).map_err(std::io::Error::other)
}

fn remove_journal(project: &Path) -> Result<(), String> {
    let path = project.join(JOURNAL);
    clear_journal_backup(&path)?;
    match fs::remove_file(&path) {
        Ok(()) => sync_parent(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove lifecycle journal: {error}")),
    }
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("lifecycle path has no parent")?;
    #[cfg(windows)]
    let sync = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0x1 | 0x2 | 0x4)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)
            .and_then(|directory| directory.sync_all())
    };
    #[cfg(not(windows))]
    let sync = fs::File::open(parent).and_then(|directory| directory.sync_all());
    sync.map_err(|error| format!("cannot sync lifecycle directory: {error}"))
}

fn exists(path: impl AsRef<Path>) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot inspect lifecycle state: {error}")),
    }
}

fn valid_owner(owner: &str) -> bool {
    let Some(value) = owner.strip_suffix('\n') else {
        return false;
    };
    let Some((pid, nonce)) = value.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !nonce.is_empty()
        && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_snapshot_journal_is_recoverable_after_begin() {
        let project = std::env::temp_dir().join(format!(
            "tapid-journal-large-snapshot-{}-{}",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        ));
        fs::create_dir_all(&project).unwrap();
        let owner = "123-deadbeef\n";
        let manifest = b"{\"name\":\"large-project\"}";
        let lock = vec![b'x'; 25 * 1024 * 1024];
        fs::write(project.join("package.json"), b"current manifest").unwrap();

        let journal = LifecycleJournal::begin(&project, owner, manifest, Some(&lock)).unwrap();
        std::mem::forget(journal);
        assert!(has_pending(&project).unwrap());
        let decision = recover(&project, Some(owner)).unwrap().unwrap();

        assert!(!decision.committed);
        assert_eq!(fs::read(project.join("package.json")).unwrap(), manifest);
        assert_eq!(fs::read(project.join("tapid.lock")).unwrap(), lock);
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn recovery_restores_interrupted_existing_journal_replacement() {
        let project = std::env::temp_dir().join(format!(
            "tapid-journal-recovery-{}-{}",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        ));
        fs::create_dir_all(&project).unwrap();
        let owner = "123-deadbeef\n";
        let original_manifest = b"original manifest";
        fs::write(project.join("package.json"), b"partially updated manifest").unwrap();
        let backup = project.join(JOURNAL).with_extension("lifecycle.bak");
        let record = Record {
            version: 1,
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(original_manifest),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
        };
        fs::write(&backup, serde_json::to_vec(&record).unwrap()).unwrap();

        assert!(has_pending(&project).unwrap());
        let decision = recover(&project, Some(owner)).unwrap().unwrap();

        assert!(!decision.committed);
        assert_eq!(
            fs::read(project.join("package.json")).unwrap(),
            original_manifest
        );
        assert!(project.join(JOURNAL).is_file());
        assert!(!backup.exists());
        finish_recovery(&project).unwrap();
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn recovery_prefers_replaced_commit_decision_over_previous_backup() {
        let project = std::env::temp_dir().join(format!(
            "tapid-journal-commit-recovery-{}-{}",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        ));
        fs::create_dir_all(&project).unwrap();
        let owner = "123-deadbeef\n";
        fs::write(project.join("package.json"), b"committed manifest").unwrap();
        let prepared = Record {
            version: 1,
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(b"original manifest"),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
        };
        let committed = Record {
            version: prepared.version,
            owner: prepared.owner.clone(),
            state: State::Committed,
            manifest_existed: prepared.manifest_existed,
            manifest: prepared.manifest.clone(),
            lock_existed: prepared.lock_existed,
            lock: prepared.lock.clone(),
            marker_existed: prepared.marker_existed,
            node_modules_existed: prepared.node_modules_existed,
            store_root: None,
        };
        let path = project.join(JOURNAL);
        let backup = journal_backup_path(&path);
        fs::write(&backup, serde_json::to_vec(&prepared).unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&committed).unwrap()).unwrap();

        let decision = recover(&project, Some(owner)).unwrap().unwrap();

        assert!(decision.committed);
        assert_eq!(
            fs::read(project.join("package.json")).unwrap(),
            b"committed manifest"
        );
        finish_recovery(&project).unwrap();
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn drop_follows_durable_commit_decision_after_publish_error() {
        let project = std::env::temp_dir().join(format!(
            "tapid-journal-drop-commit-{}-{}",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        ));
        fs::create_dir_all(&project).unwrap();
        let owner = "123-deadbeef\n";
        let original_manifest = b"original manifest";
        let committed_manifest = b"committed manifest";
        fs::write(project.join("package.json"), committed_manifest).unwrap();
        let prepared = Record {
            version: 1,
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(original_manifest),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
        };
        let mut committed = Record {
            version: prepared.version,
            owner: prepared.owner.clone(),
            state: State::Committed,
            manifest_existed: prepared.manifest_existed,
            manifest: prepared.manifest.clone(),
            lock_existed: prepared.lock_existed,
            lock: prepared.lock.clone(),
            marker_existed: prepared.marker_existed,
            node_modules_existed: prepared.node_modules_existed,
            store_root: None,
        };
        committed.manifest = STANDARD.encode(committed_manifest);
        fs::write(
            project.join(JOURNAL),
            serde_json::to_vec(&committed).unwrap(),
        )
        .unwrap();

        drop(LifecycleJournal {
            project: project.clone(),
            record: prepared,
        });

        assert_eq!(
            fs::read(project.join("package.json")).unwrap(),
            committed_manifest
        );
        assert!(!project.join(JOURNAL).exists());
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn fallback_replaces_existing_journal_without_losing_durable_contents() {
        let root = std::env::temp_dir().join(format!(
            "tapid-journal-replace-{}-{}",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join(JOURNAL);
        let temp = root.join("replacement.tmp");
        fs::write(&target, b"previous journal").unwrap();
        fs::write(&temp, b"updated journal").unwrap();

        replace_journal_file(&temp, &target, true).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"updated journal");
        assert!(!temp.exists());
        assert!(!target.with_extension("lifecycle.bak").exists());
        let _ = fs::remove_dir_all(root);
    }
}
