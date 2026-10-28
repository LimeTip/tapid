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
    #[serde(default = "default_manifest_path")]
    manifest_path: String,
    owner: String,
    state: State,
    manifest_existed: bool,
    manifest: String,
    lock_existed: bool,
    lock: String,
    marker_existed: bool,
    node_modules_existed: bool,
    store_root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<MemberActivation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    manifests: Vec<ManifestSnapshot>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSnapshot {
    path: String,
    bytes: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberActivation {
    path: String,
    marker_existed: bool,
    node_modules_existed: bool,
}

fn member_paths(
    project: &Path,
    members: &[MemberActivation],
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut paths = Vec::new();
    let mut previous: Option<&str> = None;
    for member in members {
        let relative = Path::new(&member.path);
        if relative.is_absolute() || previous.is_some_and(|p| p >= member.path.as_str()) {
            return Err("refusing unsafe or duplicate lifecycle member path".into());
        }
        let path = project.join(relative);
        validate_member_root(project, &path)?;
        previous = Some(&member.path);
        paths.push(path);
    }
    Ok(paths)
}

/// Revalidate the recorded directory identity before writing or recovering a member layout.
pub(crate) fn validate_member_root(project: &Path, member: &Path) -> Result<(), String> {
    let relative = member
        .strip_prefix(project)
        .map_err(|_| "member activation escapes project")?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|c| {
            !matches!(c, std::path::Component::Normal(_)) || c.as_os_str() == "node_modules"
        })
    {
        return Err("refusing unsafe lifecycle member path".into());
    }
    if member.canonicalize().as_deref().ok() != Some(member) {
        return Err("refusing noncanonical lifecycle member path".into());
    }
    if !fs::symlink_metadata(member)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_dir()
    {
        return Err("lifecycle member must be a directory".into());
    }
    Ok(())
}

fn default_manifest_path() -> String {
    "package.json".to_owned()
}

#[derive(Serialize, Deserialize, PartialEq)]
enum State {
    Prepared,
    Committed,
}

pub(crate) struct LifecycleJournal {
    project: std::path::PathBuf,
    record: Record,
    finished: bool,
    settlement_attempted: bool,
}

fn relative_manifest_path(project: &Path, manifest_path: &Path) -> Result<String, String> {
    let project = fs::canonicalize(project)
        .map_err(|error| format!("cannot resolve lifecycle project root: {error}"))?;
    let candidate = if manifest_path.is_absolute() {
        manifest_path.to_path_buf()
    } else {
        project.join(manifest_path)
    };
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| format!("cannot inspect lifecycle manifest target: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("lifecycle manifest target must be a regular, non-symlink file".into());
    }
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| format!("cannot resolve lifecycle manifest target: {error}"))?;
    let relative = canonical
        .strip_prefix(&project)
        .map_err(|_| "lifecycle manifest target escapes the project root")?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("lifecycle manifest target has an unsafe relative path".into());
    }
    relative
        .to_str()
        .map(|path| path.replace(std::path::MAIN_SEPARATOR, "/"))
        .ok_or_else(|| "lifecycle manifest target path is not valid UTF-8".to_owned())
}

impl LifecycleJournal {
    pub(crate) fn begin(
        project: &Path,
        manifest_path: &Path,
        owner: &str,
        manifest: &[u8],
        lock: Option<&[u8]>,
    ) -> Result<Self, String> {
        Self::begin_many(project, &[(manifest_path, manifest)], owner, lock)
    }

    pub(crate) fn begin_many(
        project: &Path,
        manifests: &[(&Path, &[u8])],
        owner: &str,
        lock: Option<&[u8]>,
    ) -> Result<Self, String> {
        if !valid_owner(owner) {
            return Err("refusing to create lifecycle journal with invalid owner".into());
        }
        if manifests.is_empty()
            || manifests
                .iter()
                .any(|(_, bytes)| bytes.len() as u64 > MAX_SNAPSHOT_BYTES)
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
        let mut snapshots = manifests
            .iter()
            .map(|(path, bytes)| {
                Ok(ManifestSnapshot {
                    path: relative_manifest_path(project, path)?,
                    bytes: STANDARD.encode(bytes),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        snapshots.sort_by(|a, b| a.path.cmp(&b.path));
        if snapshots
            .windows(2)
            .any(|pair| pair[0].path == pair[1].path)
        {
            return Err("duplicate lifecycle manifest target".into());
        }
        let single = snapshots.len() == 1;
        let (manifest_path, manifest) = if single {
            let snapshot = snapshots.pop().expect("one snapshot");
            (snapshot.path, snapshot.bytes)
        } else {
            (default_manifest_path(), String::new())
        };
        let record = Record {
            version: if single { 2 } else { 4 },
            manifest_path,
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest,
            lock_existed: lock.is_some(),
            lock: lock.map(|bytes| STANDARD.encode(bytes)).unwrap_or_default(),
            marker_existed,
            node_modules_existed,
            store_root: None,
            members: Vec::new(),
            manifests: snapshots,
        };
        validated_manifest_snapshots(project, &record)?;
        write_record(project, &record)?;
        Ok(Self {
            project: project.to_owned(),
            record,
            finished: false,
            settlement_attempted: false,
        })
    }

    pub(crate) fn set_member_roots<'a>(
        &mut self,
        roots: impl Iterator<Item = &'a std::path::PathBuf>,
    ) -> Result<(), String> {
        if !self.record.members.is_empty() {
            return Err("member activation roots already recorded".into());
        }
        let mut members = Vec::new();
        for root in roots {
            crate::filesystem::activation::validate_managed_node_modules(root)?;
            let path = root
                .strip_prefix(&self.project)
                .map_err(|_| "member activation escapes project")?
                .to_str()
                .ok_or("member path is not UTF-8")?
                .to_owned();
            members.push(MemberActivation {
                path,
                marker_existed: exists(root.join(".tapid-managed"))?,
                node_modules_existed: exists(root.join("node_modules"))?,
            });
        }
        members.sort_by(|a, b| a.path.cmp(&b.path));
        member_paths(&self.project, &members)?;
        if !members.is_empty() {
            self.record.version = self.record.version.max(3);
            self.record.members = members;
            write_record(&self.project, &self.record)?;
        }
        Ok(())
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

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        for member in member_paths(&self.project, &self.record.members)? {
            crate::filesystem::activation::commit_owned_activation(&member, &self.record.owner)?;
        }
        crate::filesystem::activation::commit_owned_activation(&self.project, &self.record.owner)?;
        remove_journal(&self.project)?;
        self.finished = true;
        Ok(())
    }

    /// Explicitly settle the durable decision while the caller still owns the
    /// project lock. Return whether the transaction committed, not just whether
    /// cleanup succeeded.
    pub(crate) fn settle(&mut self) -> Result<bool, String> {
        if self.finished {
            return Ok(self.record.state == State::Committed);
        }
        self.settlement_attempted = true;
        let decision = recover(&self.project, Some(&self.record.owner))?
            .ok_or("lifecycle journal disappeared before recovery")?;
        crate::filesystem::activation::recover_owned_activation(
            &self.project,
            &decision.owner,
            Some(decision.clone()),
        )?;
        crate::filesystem::activation::recover_owned_stages(&self.project, &decision.owner)?;
        remove_journal(&self.project)?;
        self.finished = true;
        Ok(decision.committed)
    }
}

impl Drop for LifecycleJournal {
    fn drop(&mut self) {
        // Panic fallback only; application results use explicit settlement.
        if !self.finished && !self.settlement_attempted {
            let _ = self.settle();
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RecoveryDecision {
    pub(crate) owner: String,
    pub(crate) committed: bool,
    pub(crate) marker_existed: bool,
    pub(crate) node_modules_existed: bool,
}

/// Check for primary or backup journal metadata without reading or recovering it.
/// Invalid file types, oversized journals, and inspection failures remain errors.
pub(crate) fn has_pending(project: &Path) -> Result<bool, String> {
    inspect_pending(project).1
}

/// Inspect metadata only, retaining the selected artifact's path even on failure.
/// Prefer the primary journal; inspect the backup only when the primary is absent.
/// A present valid artifact means recovery is pending, not that its contents are valid.
pub(crate) fn inspect_pending(project: &Path) -> (std::path::PathBuf, Result<bool, String>) {
    let path = project.join(JOURNAL);
    let backup = journal_backup_path(&path);
    let (path, metadata, label) = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let metadata = fs::symlink_metadata(&backup);
            (backup, metadata, "lifecycle journal backup")
        }
        metadata => (path, metadata, "lifecycle journal"),
    };
    let state = match metadata {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= MAX_JOURNAL_BYTES => {
            Ok(true)
        }
        Ok(metadata) if metadata.file_type().is_file() => {
            Err(format!("refusing to inspect an oversized {label}"))
        }
        Ok(_) => Err(format!("refusing to inspect a non-regular {label}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot inspect {label}: {error}")),
    };
    (path, state)
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
    if !matches!(record.version, 1..=4)
        || (record.version == 1 && record.manifest_path != "package.json")
        || !valid_owner(&record.owner)
        || stale_owner.is_some_and(|owner| record.owner != owner)
    {
        return Err("refusing to recover a lifecycle journal with invalid ownership".into());
    }
    if record.version < 3 && !record.members.is_empty() {
        return Err("member activation requires lifecycle version 3".into());
    }
    let member_paths = member_paths(project, &record.members)?;
    // Validate and decode the complete recovery set before store recovery or writes.
    let manifests = validated_manifest_snapshots(project, &record)?;
    let lock = decode_snapshot(&record.lock)?;
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
        recover_record(project, &record, &manifests, &lock)?;
    }
    for (member, path) in record.members.iter().zip(member_paths) {
        crate::filesystem::activation::recover_owned_activation(
            &path,
            &record.owner,
            Some(RecoveryDecision {
                owner: record.owner.clone(),
                committed: record.state == State::Committed,
                marker_existed: member.marker_existed,
                node_modules_existed: member.node_modules_existed,
            }),
        )?;
        crate::filesystem::activation::recover_owned_stages(&path, &record.owner)?;
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

fn decode_snapshot(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() as u64 > MAX_ENCODED_SNAPSHOT_BYTES {
        return Err("refusing to recover oversized lifecycle state".into());
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid snapshot bytes in lifecycle journal: {error}"))?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err("refusing to recover oversized lifecycle state".into());
    }
    Ok(bytes)
}

type DecodedManifest = (std::path::PathBuf, Vec<u8>);

fn validated_manifest_snapshots(
    project: &Path,
    record: &Record,
) -> Result<Vec<DecodedManifest>, String> {
    if record.version < 4 {
        if !record.manifests.is_empty() {
            return Err("manifest collection requires lifecycle version 4".into());
        }
        return Ok(vec![(
            recovery_manifest_path(project, &record.manifest_path)?,
            decode_snapshot(&record.manifest)?,
        )]);
    }
    if record.manifests.is_empty()
        || !record.manifest.is_empty()
        || record.manifest_path != "package.json"
    {
        return Err("invalid version 4 lifecycle manifest collection".into());
    }
    let mut paths = std::collections::BTreeSet::new();
    record
        .manifests
        .iter()
        .map(|snapshot| {
            if snapshot.path.contains('\\')
                || snapshot.path.as_bytes().get(1) == Some(&b':')
                || snapshot.path.split('/').any(|part| {
                    part.is_empty() || part == "." || part == ".." || part == "node_modules"
                })
                || Path::new(&snapshot.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    != Some("package.json")
            {
                return Err("refusing unsafe lifecycle manifest snapshot path".into());
            }
            let path = recovery_manifest_path(project, &snapshot.path)?;
            // Canonical parent identity also detects aliases of an earlier target.
            let identity = path
                .parent()
                .ok_or("manifest has no parent")?
                .canonicalize()
                .map_err(|error| error.to_string())?
                .join(path.file_name().ok_or("manifest has no filename")?);
            if identity != path {
                return Err("refusing noncanonical lifecycle manifest snapshot path".into());
            }
            if !paths.insert(identity) {
                return Err("duplicate lifecycle manifest target".into());
            }
            Ok((path, decode_snapshot(&snapshot.bytes)?))
        })
        .collect()
}

fn recover_record(
    project: &Path,
    record: &Record,
    manifests: &[DecodedManifest],
    lock: &[u8],
) -> Result<(), String> {
    for (path, bytes) in manifests {
        atomic_replace(path, bytes)?;
    }
    let lock_path = project.join("tapid.lock");
    if record.lock_existed {
        atomic_replace(&lock_path, lock)?;
    } else {
        match fs::remove_file(&lock_path) {
            Ok(()) => sync_parent(&lock_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot restore prior lockfile state: {error}")),
        }
    }
    Ok(())
}

fn recovery_manifest_path(project: &Path, relative: &str) -> Result<std::path::PathBuf, String> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("refusing to recover lifecycle manifest through an unsafe path".into());
    }
    let root = fs::canonicalize(project)
        .map_err(|error| format!("cannot resolve lifecycle project root: {error}"))?;
    let target = root.join(relative);
    let parent = target
        .parent()
        .ok_or("lifecycle manifest path has no parent")?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| format!("cannot resolve lifecycle manifest parent: {error}"))?;
    if !canonical_parent.starts_with(&root) {
        return Err("refusing to recover lifecycle manifest outside project root".into());
    }
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err("refusing to recover lifecycle manifest over a non-regular file".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect lifecycle manifest during recovery: {error}"
            ));
        }
    }
    Ok(target)
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
    fn batch_journal_recovers_every_manifest_after_interruption() {
        let project = tapid_test_support::TempProject::new("batch-journal").unwrap();
        project.write("package.json", b"root unchanged").unwrap();
        let a = project.write("apps/a/package.json", b"old a").unwrap();
        let b = project.write("apps/b/package.json", b"old b").unwrap();
        let owner = "123-deadbeef\n";
        let journal = LifecycleJournal::begin_many(
            project.path(),
            &[
                (a.as_path(), b"old a".as_slice()),
                (b.as_path(), b"old b".as_slice()),
            ],
            owner,
            Some(b"old lock"),
        )
        .unwrap();
        project.write("apps/a/package.json", b"new a").unwrap();
        project.write("apps/b/package.json", b"new b").unwrap();
        project.write("tapid.lock", b"new lock").unwrap();
        std::mem::forget(journal);
        assert!(
            !recover(project.path(), Some(owner))
                .unwrap()
                .unwrap()
                .committed
        );
        assert_eq!(fs::read(a).unwrap(), b"old a");
        assert_eq!(fs::read(b).unwrap(), b"old b");
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"root unchanged"
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            b"old lock"
        );
    }

    #[test]
    fn batch_journal_validates_all_snapshots_before_any_restoration() {
        let project = tapid_test_support::TempProject::new("batch-journal-invalid").unwrap();
        let a = project.write("a/package.json", b"current a").unwrap();
        let b = project.write("b/package.json", b"current b").unwrap();
        project.write("tapid.lock", b"current lock").unwrap();
        let owner = "123-deadbeef\n";
        let journal = LifecycleJournal::begin_many(
            project.path(),
            &[
                (a.as_path(), b"old a".as_slice()),
                (b.as_path(), b"old b".as_slice()),
            ],
            owner,
            Some(b"old lock"),
        )
        .unwrap();
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for replacement in [
            serde_json::json!({"path":"../outside/package.json", "bytes":"e30="}),
            serde_json::json!({"path":"a/package.json", "bytes":"e30="}),
            serde_json::json!({"path":"b/package.json", "bytes":"invalid base64!"}),
            serde_json::json!({"path":"b/./package.json", "bytes":"e30="}),
        ] {
            let mut record = original.clone();
            record["manifests"][1] = replacement;
            fs::write(&path, record.to_string()).unwrap();
            assert!(recover(project.path(), Some(owner)).is_err(), "{record}");
            assert_eq!(fs::read(&a).unwrap(), b"current a");
            assert_eq!(fs::read(&b).unwrap(), b"current b");
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                b"current lock"
            );
            assert!(path.exists());
        }
    }

    #[test]
    fn batch_journal_preserves_aggregate_budget_before_publication() {
        let project = tapid_test_support::TempProject::new("batch-journal-budget").unwrap();
        let a = project.write("a/package.json", b"a").unwrap();
        let b = project.write("b/package.json", b"b").unwrap();
        let c = project.write("c/package.json", b"c").unwrap();
        let bytes = vec![b'x'; 25 * 1024 * 1024];
        let result = LifecycleJournal::begin_many(
            project.path(),
            &[
                (a.as_path(), &bytes),
                (b.as_path(), &bytes),
                (c.as_path(), &bytes),
            ],
            "123-deadbeef\n",
            None,
        );
        assert!(result.err().unwrap().contains("encoded size limit"));
        assert!(!project.path().join(JOURNAL).exists());
        assert_eq!(fs::read(a).unwrap(), b"a");
        assert_eq!(fs::read(b).unwrap(), b"b");
        assert_eq!(fs::read(c).unwrap(), b"c");
    }

    #[test]
    fn batch_journal_commit_retains_edits_and_rejects_duplicate_targets() {
        let project = tapid_test_support::TempProject::new("batch-journal-commit").unwrap();
        let a = project.write("a/package.json", b"old a").unwrap();
        let b = project.write("b/package.json", b"old b").unwrap();
        let owner = "123-deadbeef\n";
        assert!(
            LifecycleJournal::begin_many(
                project.path(),
                &[
                    (a.as_path(), b"old a".as_slice()),
                    (a.as_path(), b"old a".as_slice())
                ],
                owner,
                None
            )
            .is_err()
        );
        assert!(!project.path().join(JOURNAL).exists());
        let mut journal = LifecycleJournal::begin_many(
            project.path(),
            &[
                (a.as_path(), b"old a".as_slice()),
                (b.as_path(), b"old b".as_slice()),
            ],
            owner,
            None,
        )
        .unwrap();
        project.write("a/package.json", b"new a").unwrap();
        project.write("b/package.json", b"new b").unwrap();
        journal.mark_committed().unwrap();
        assert!(journal.settle().unwrap());
        assert_eq!(fs::read(a).unwrap(), b"new a");
        assert_eq!(fs::read(b).unwrap(), b"new b");
        assert!(!project.path().join(JOURNAL).exists());
    }

    #[cfg(unix)]
    #[test]
    fn batch_journal_keeps_evidence_until_interrupted_restoration_can_finish() {
        let project = tapid_test_support::TempProject::new("batch-journal-retry").unwrap();
        let a = project.write("a/package.json", b"new a").unwrap();
        let b = project.write("b/package.json", b"new b").unwrap();
        let owner = "123-deadbeef\n";
        let journal = LifecycleJournal::begin_many(
            project.path(),
            &[
                (a.as_path(), b"old a".as_slice()),
                (b.as_path(), b"old b".as_slice()),
            ],
            owner,
            Some(b"old lock"),
        )
        .unwrap();
        std::mem::forget(journal);
        // Fail after manifest restoration, before lock restoration completes.
        let lock_path = project.path().join("tapid.lock");
        fs::create_dir(&lock_path).unwrap();
        assert!(recover(project.path(), Some(owner)).is_err());
        assert!(project.path().join(JOURNAL).exists());
        fs::remove_dir(&lock_path).unwrap();
        assert!(
            !recover(project.path(), Some(owner))
                .unwrap()
                .unwrap()
                .committed
        );
        assert_eq!(fs::read(a).unwrap(), b"old a");
        assert_eq!(fs::read(b).unwrap(), b"old b");
        assert_eq!(fs::read(lock_path).unwrap(), b"old lock");
        finish_recovery(project.path()).unwrap();
        assert!(!project.path().join(JOURNAL).exists());
    }

    #[test]
    fn invalid_member_recovery_paths_fail_before_mutation() {
        let project = tapid_test_support::TempProject::new("journal-member-paths").unwrap();
        project.write("package.json", b"current manifest").unwrap();
        project.write("tapid.lock", b"current lock").unwrap();
        project.write("member/package.json", b"{}").unwrap();
        let owner = "123-deadbeef\n";
        let journal = LifecycleJournal::begin(
            project.path(),
            &project.path().join("package.json"),
            owner,
            b"old manifest",
            Some(b"old lock"),
        )
        .unwrap();
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for paths in [
            vec!["../outside"],
            vec!["/absolute"],
            vec!["member/../member"],
            vec!["member/node_modules"],
            vec!["member", "member"],
        ] {
            let mut record = original.clone();
            record["version"] = serde_json::json!(3);
            record["members"] = serde_json::Value::Array(paths.iter().map(|path| serde_json::json!({"path":path,"marker_existed":false,"node_modules_existed":false})).collect());
            fs::write(&path, record.to_string()).unwrap();
            assert!(recover(project.path(), Some(owner)).is_err(), "{paths:?}");
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                b"current manifest"
            );
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                b"current lock"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn member_recovery_rejects_redirected_directory_before_mutation() {
        let project = tapid_test_support::TempProject::new("journal-member-symlink").unwrap();
        let outside = tapid_test_support::TempProject::new("journal-member-outside").unwrap();
        project.write("package.json", b"current manifest").unwrap();
        outside.write("node_modules/sentinel", b"outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), project.path().join("member")).unwrap();
        let owner = "123-deadbeef\n";
        let journal = LifecycleJournal::begin(
            project.path(),
            &project.path().join("package.json"),
            owner,
            b"old manifest",
            None,
        )
        .unwrap();
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["version"] = serde_json::json!(3);
        record["members"] = serde_json::json!([{"path":"member","marker_existed":false,"node_modules_existed":false}]);
        fs::write(&path, record.to_string()).unwrap();
        assert!(recover(project.path(), Some(owner)).is_err());
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"current manifest"
        );
        assert_eq!(
            fs::read(outside.path().join("node_modules/sentinel")).unwrap(),
            b"outside"
        );
    }
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

        let journal = LifecycleJournal::begin(
            &project,
            &project.join("package.json"),
            owner,
            manifest,
            Some(&lock),
        )
        .unwrap();
        std::mem::forget(journal);
        assert!(has_pending(&project).unwrap());
        let decision = recover(&project, Some(owner)).unwrap().unwrap();

        assert!(!decision.committed);
        assert_eq!(fs::read(project.join("package.json")).unwrap(), manifest);
        assert_eq!(fs::read(project.join("tapid.lock")).unwrap(), lock);
        let _ = fs::remove_dir_all(project);
    }

    #[test]
    fn interrupted_selected_member_transaction_restores_only_that_manifest() {
        let project = std::env::temp_dir().join(format!(
            "tapid-journal-member-recovery-{}-{}",
            std::process::id(),
            crate::filesystem::atomic::unique_nonce()
        ));
        let member_dir = project.join("packages/news");
        fs::create_dir_all(&member_dir).unwrap();
        let root_manifest = br#"{"name":"root","version":"1.0.0"}"#;
        let original_member = br#"{"name":"news","version":"1.0.0"}"#;
        let interrupted_member =
            br#"{"name":"news","version":"1.0.0","dependencies":{"local":"1.0.0"}}"#;
        let root_manifest_path = project.join("package.json");
        let member_manifest_path = member_dir.join("package.json");
        fs::write(&root_manifest_path, root_manifest).unwrap();
        fs::write(&member_manifest_path, original_member).unwrap();
        let owner = "123-deadbeef\n";

        let journal = LifecycleJournal::begin(
            &project,
            &member_manifest_path,
            owner,
            original_member,
            None,
        )
        .unwrap();
        fs::write(&member_manifest_path, interrupted_member).unwrap();
        std::mem::forget(journal);

        let decision = recover(&project, Some(owner)).unwrap().unwrap();
        assert!(!decision.committed);
        assert_eq!(fs::read(&root_manifest_path).unwrap(), root_manifest);
        assert_eq!(fs::read(&member_manifest_path).unwrap(), original_member);
        finish_recovery(&project).unwrap();
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
            manifest_path: "package.json".to_owned(),
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(original_manifest),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
            members: Vec::new(),
            manifests: Vec::new(),
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
            manifest_path: "package.json".to_owned(),
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(b"original manifest"),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
            members: Vec::new(),
            manifests: Vec::new(),
        };
        let committed = Record {
            version: prepared.version,
            manifest_path: prepared.manifest_path.clone(),
            owner: prepared.owner.clone(),
            state: State::Committed,
            manifest_existed: prepared.manifest_existed,
            manifest: prepared.manifest.clone(),
            lock_existed: prepared.lock_existed,
            lock: prepared.lock.clone(),
            marker_existed: prepared.marker_existed,
            node_modules_existed: prepared.node_modules_existed,
            store_root: None,
            members: Vec::new(),
            manifests: Vec::new(),
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
            manifest_path: "package.json".to_owned(),
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(original_manifest),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
            members: Vec::new(),
            manifests: Vec::new(),
        };
        let mut committed = Record {
            version: prepared.version,
            manifest_path: prepared.manifest_path.clone(),
            owner: prepared.owner.clone(),
            state: State::Committed,
            manifest_existed: prepared.manifest_existed,
            manifest: prepared.manifest.clone(),
            lock_existed: prepared.lock_existed,
            lock: prepared.lock.clone(),
            marker_existed: prepared.marker_existed,
            node_modules_existed: prepared.node_modules_existed,
            store_root: None,
            members: Vec::new(),
            manifests: Vec::new(),
        };
        committed.manifest = STANDARD.encode(committed_manifest);
        fs::write(
            project.join(JOURNAL),
            serde_json::to_vec(&committed).unwrap(),
        )
        .unwrap();

        drop(LifecycleJournal {
            finished: false,
            settlement_attempted: false,
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
