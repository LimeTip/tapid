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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<TransactionKind>,
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
    metadata_outputs: Vec<MetadataOutput>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
enum TransactionKind {
    Activation,
    Metadata,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataOutput {
    path: String,
    before: Option<String>,
    after: String,
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
        .map(str::to_owned)
        .ok_or_else(|| "lifecycle manifest target path is not valid UTF-8".to_owned())
}

impl LifecycleJournal {
    // The command writer remains gated on native pending-receipt hydration.
    #[allow(dead_code)]
    pub(crate) fn prepare_metadata(
        project: &Path,
        owner: &str,
        mut outputs: Vec<(String, Option<Vec<u8>>, Vec<u8>)>,
    ) -> Result<Self, String> {
        if !valid_owner(owner) || has_pending(project)? {
            return Err(
                "metadata preparation requires valid ownership and no pending journal".into(),
            );
        }
        let total = outputs.iter().try_fold(0u64, |total, (_, before, after)| {
            total
                .checked_add(before.as_ref().map_or(0, |bytes| bytes.len() as u64))?
                .checked_add(after.len() as u64)
        });
        if outputs.len() > 1024 || total.is_none_or(|total| total > MAX_SNAPSHOT_BYTES) {
            return Err("metadata output set exceeds size limit".into());
        }
        // Callers provide an output set. Persist its canonical order here so
        // every writer shares recovery's ordering and duplicate-path rules.
        outputs.sort_by(|left, right| left.0.cmp(&right.0));
        let record = Record {
            version: 4,
            kind: Some(TransactionKind::Metadata),
            manifest_path: default_manifest_path(),
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: false,
            manifest: String::new(),
            lock_existed: false,
            lock: String::new(),
            marker_existed: false,
            node_modules_existed: false,
            store_root: None,
            members: Vec::new(),
            metadata_outputs: outputs
                .into_iter()
                .map(|(path, before, after)| MetadataOutput {
                    path,
                    before: before.map(|bytes| STANDARD.encode(bytes)),
                    after: STANDARD.encode(after),
                })
                .collect(),
        };
        // Prepared validation never publishes. Use the recovery validator so
        // preparation and interrupted recovery enforce precisely the same rules.
        recover_metadata(project, &record)?;
        write_record(project, &record)?;
        Ok(Self {
            project: project.to_owned(),
            record,
            finished: false,
            settlement_attempted: false,
        })
    }

    pub(crate) fn begin(
        project: &Path,
        manifest_path: &Path,
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
            version: 4,
            kind: Some(TransactionKind::Activation),
            manifest_path: relative_manifest_path(project, manifest_path)?,
            owner: owner.to_owned(),
            state: State::Prepared,
            manifest_existed: true,
            manifest: STANDARD.encode(manifest),
            lock_existed: lock.is_some(),
            lock: lock.map(|bytes| STANDARD.encode(bytes)).unwrap_or_default(),
            marker_existed,
            node_modules_existed,
            store_root: None,
            members: Vec::new(),
            metadata_outputs: Vec::new(),
        };
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
        if self.record.kind == Some(TransactionKind::Metadata) {
            return Err("metadata transactions cannot activate member layouts".into());
        }
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
        if self.record.kind == Some(TransactionKind::Metadata) {
            return Err("metadata transactions cannot publish store content".into());
        }
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
        if self.record.kind == Some(TransactionKind::Metadata) {
            if self.record.state != State::Prepared {
                return Err("metadata transaction already committed".into());
            }
            recover_metadata(&self.project, &self.record)?;
        }
        let previous = std::mem::replace(&mut self.record.state, State::Committed);
        if let Err(error) = write_record(&self.project, &self.record) {
            // A failed write cannot authorize publication through this handle.
            // Explicit settlement reads the durable record, including an
            // uncertain decision whose final directory sync failed.
            self.record.state = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        if self.record.kind == Some(TransactionKind::Metadata) {
            if self.record.state != State::Committed {
                return Err("metadata publication requires a durable commit decision".into());
            }
            recover_metadata(&self.project, &self.record)?;
            remove_journal(&self.project)?;
            self.finished = true;
            return Ok(());
        }
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
        if !decision.metadata_only {
            crate::filesystem::activation::recover_owned_activation(
                &self.project,
                &decision.owner,
                Some(decision.clone()),
            )?;
            crate::filesystem::activation::recover_owned_stages(&self.project, &decision.owner)?;
        }
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
    pub(crate) metadata_only: bool,
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
    recover_mode(project, stale_owner, false)
}

pub(crate) fn recover_metadata_only(
    project: &Path,
    stale_owner: Option<&str>,
) -> Result<Option<RecoveryDecision>, String> {
    recover_mode(project, stale_owner, true)
}

fn recover_mode(
    project: &Path,
    stale_owner: Option<&str>,
    metadata_only: bool,
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
    let supported_kind = match record.version {
        1..=3 => record.kind.is_none(),
        4 => record.kind.is_some(),
        _ => false,
    };
    if !supported_kind
        || (record.version == 1 && record.manifest_path != "package.json")
        || !valid_owner(&record.owner)
        || stale_owner.is_some_and(|owner| record.owner != owner)
    {
        return Err("refusing to recover a lifecycle journal with invalid ownership".into());
    }
    if metadata_only && record.kind != Some(TransactionKind::Metadata) {
        return Err(
            "selection-only execution refuses activation recovery; run tapid install first".into(),
        );
    }
    if record.kind == Some(TransactionKind::Metadata) {
        recover_metadata(project, &record)?;
        return Ok(Some(RecoveryDecision {
            owner: record.owner,
            metadata_only: true,
            committed: record.state == State::Committed,
            marker_existed: false,
            node_modules_existed: false,
        }));
    }
    if !record.metadata_outputs.is_empty() {
        return Err("activation journal contains metadata outputs".into());
    }
    if record.version < 3 && !record.members.is_empty() {
        return Err("member activation requires lifecycle version 3".into());
    }
    let member_paths = member_paths(project, &record.members)?;
    recovery_manifest_path(project, &record.manifest_path)?;
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
    for (member, path) in record.members.iter().zip(member_paths) {
        crate::filesystem::activation::recover_owned_activation(
            &path,
            &record.owner,
            Some(RecoveryDecision {
                owner: record.owner.clone(),
                metadata_only: false,
                committed: record.state == State::Committed,
                marker_existed: member.marker_existed,
                node_modules_existed: member.node_modules_existed,
            }),
        )?;
        crate::filesystem::activation::recover_owned_stages(&path, &record.owner)?;
    }
    Ok(Some(RecoveryDecision {
        metadata_only: false,
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

// Prepared metadata transactions have not published outputs. Committed recovery
// accepts either recorded value so interrupted multi-file publication is resumable.
fn recover_metadata(project: &Path, record: &Record) -> Result<(), String> {
    if record.store_root.is_some()
        || !record.members.is_empty()
        || record.metadata_outputs.is_empty()
        || record.metadata_outputs.len() > 1024
    {
        return Err("invalid metadata transaction effects".into());
    }
    let mut previous = "";
    let mut total = 0u64;
    let mut outputs = Vec::new();
    for output in &record.metadata_outputs {
        let relative = Path::new(&output.path);
        if output.path.as_str() <= previous
            || !(output.path == "tapid.lock"
                || relative
                    .file_name()
                    .is_some_and(|name| name == "package.json"))
            || output.path.split('/').any(|part| {
                part.is_empty()
                    || matches!(part, "." | ".." | "node_modules")
                    || part.contains('\\')
            })
        {
            return Err("unsafe or duplicate metadata output path".into());
        }
        previous = &output.path;
        let path = recovery_manifest_path(project, &output.path)?;
        if path.parent().and_then(|p| p.canonicalize().ok()).as_deref() != path.parent() {
            return Err("metadata output parent is redirected".into());
        }
        let decode = |value: &str| -> Result<Vec<u8>, String> {
            if value.len() as u64 > MAX_ENCODED_SNAPSHOT_BYTES {
                return Err("oversized metadata output".into());
            }
            let bytes = STANDARD.decode(value).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
                return Err("oversized metadata output".into());
            }
            Ok(bytes)
        };
        let before = output.before.as_deref().map(decode).transpose()?;
        let after = decode(&output.after)?;
        total += before.as_ref().map_or(0, |b| b.len() as u64) + after.len() as u64;
        if total > MAX_SNAPSHOT_BYTES {
            return Err("metadata output set exceeds size limit".into());
        }
        let current =
            match crate::filesystem::read::read_bounded_file(&path, MAX_SNAPSHOT_BYTES as usize) {
                Ok(bytes) => Some(bytes),
                Err(crate::filesystem::read::ReadError::Io(e))
                    if e.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(e) => return Err(format!("cannot read metadata output: {e:?}")),
            };
        if current != before
            && (record.state != State::Committed || current.as_deref() != Some(after.as_slice()))
        {
            return Err("metadata output changed since preparation".into());
        }
        if current.as_deref() != Some(after.as_slice()) {
            outputs.push((path, after));
        }
    }
    if record.state == State::Committed {
        for (path, bytes) in outputs {
            atomic_replace(&path, &bytes)?;
        }
    }
    Ok(())
}

fn recover_record(project: &Path, record: &Record) -> Result<(), String> {
    let manifest = STANDARD
        .decode(&record.manifest)
        .map_err(|error| format!("invalid manifest bytes in lifecycle journal: {error}"))?;
    let manifest_path = recovery_manifest_path(project, &record.manifest_path)?;
    atomic_replace(&manifest_path, &manifest)?;
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
    fn metadata_preparation_and_durable_decision_publish_without_activation() {
        for committed in [false, true] {
            let project = tapid_test_support::TempProject::new("metadata-publication").unwrap();
            project.write("package.json", b"old manifest").unwrap();
            project
                .write("node_modules/sentinel", b"running application")
                .unwrap();
            let lock =
                crate::filesystem::activation::ActivationLock::acquire_metadata(project.path())
                    .unwrap();
            let mut journal = LifecycleJournal::prepare_metadata(
                project.path(),
                lock.owner_line(),
                vec![
                    (
                        "package.json".into(),
                        Some(b"old manifest".to_vec()),
                        b"new manifest".to_vec(),
                    ),
                    ("tapid.lock".into(), None, b"new lock".to_vec()),
                ],
            )
            .unwrap();
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                b"old manifest"
            );
            assert!(!project.path().join("tapid.lock").exists());
            if committed {
                journal.mark_committed().unwrap();
            }
            // Model process loss on either side of the durable decision.
            std::mem::forget(journal);
            drop(lock);
            let lock =
                crate::filesystem::activation::ActivationLock::acquire_metadata(project.path())
                    .unwrap();
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                if committed {
                    b"new manifest"
                } else {
                    b"old manifest"
                }
            );
            assert_eq!(project.path().join("tapid.lock").exists(), committed);
            assert_eq!(
                fs::read(project.path().join("node_modules/sentinel")).unwrap(),
                b"running application"
            );
            assert!(!project.path().join(JOURNAL).exists());
            drop(lock);
        }
    }

    #[test]
    fn metadata_publication_rechecks_inputs_and_finishes_without_layout_cleanup() {
        let project = tapid_test_support::TempProject::new("metadata-finish").unwrap();
        project.write("package.json", b"old").unwrap();
        let lock = crate::filesystem::activation::ActivationLock::acquire_metadata(project.path())
            .unwrap();
        let mut journal = LifecycleJournal::prepare_metadata(
            project.path(),
            lock.owner_line(),
            vec![(
                "package.json".into(),
                Some(b"old".to_vec()),
                b"new".to_vec(),
            )],
        )
        .unwrap();
        assert!(journal.finish().unwrap_err().contains("durable commit"));
        project.write("package.json", b"concurrent").unwrap();
        assert!(
            journal
                .mark_committed()
                .unwrap_err()
                .contains("changed since preparation")
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"concurrent"
        );
        project.write("package.json", b"old").unwrap();
        journal.mark_committed().unwrap();
        // Metadata finishing must never clean activation backups, even if an
        // unexpected uncoordinated writer created one after acquisition.
        let backup = project
            .path()
            .join(format!(".tapid-managed-old-{}", lock.owner_line().trim()));
        fs::write(&backup, b"untouched").unwrap();
        journal.finish().unwrap();
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"new"
        );
        assert_eq!(fs::read(backup).unwrap(), b"untouched");
        assert!(!project.path().join(JOURNAL).exists());
    }

    #[test]
    fn metadata_commit_record_failure_cannot_authorize_publication() {
        let project = tapid_test_support::TempProject::new("metadata-decision-failure").unwrap();
        project.write("package.json", b"old").unwrap();
        let mut journal = LifecycleJournal::prepare_metadata(
            project.path(),
            "123-deadbeef\n",
            vec![(
                "package.json".into(),
                Some(b"old".to_vec()),
                b"new".to_vec(),
            )],
        )
        .unwrap();
        let path = project.path().join(JOURNAL);
        let saved = project.path().join("saved-journal");
        fs::rename(&path, &saved).unwrap();
        fs::create_dir(&path).unwrap();
        let obstruction = journal_backup_path(&path);
        fs::create_dir(&obstruction).unwrap();
        assert!(journal.mark_committed().is_err());
        assert!(journal.finish().is_err());
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"old"
        );
        fs::remove_dir(obstruction).unwrap();
        fs::remove_dir(&path).unwrap();
        fs::rename(saved, path).unwrap();
        assert!(!journal.settle().unwrap());
    }

    #[test]
    fn metadata_acquisition_refuses_activation_recovery_without_mutation() {
        let project = tapid_test_support::TempProject::new("metadata-refusal").unwrap();
        project.write("package.json", b"before").unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let lock = crate::filesystem::activation::ActivationLock::acquire(project.path()).unwrap();
        let journal = LifecycleJournal::begin(
            project.path(),
            Path::new("package.json"),
            lock.owner_line(),
            b"before",
            None,
        )
        .unwrap();
        std::mem::forget(journal);
        drop(lock);
        project.write("package.json", b"current").unwrap();
        let before = fs::read(project.path().join(JOURNAL)).unwrap();
        assert!(
            crate::filesystem::activation::ActivationLock::acquire_metadata(project.path())
                .is_err()
        );
        assert_eq!(fs::read(project.path().join(JOURNAL)).unwrap(), before);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"current"
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
    }

    #[test]
    fn metadata_committed_recovery_preserves_root_and_member_installations() {
        let project = tapid_test_support::TempProject::new("metadata-recovery").unwrap();
        for (path, bytes) in [
            ("package.json", "old root"),
            ("member/package.json", "old member"),
            ("tapid.lock", "old lock"),
            ("node_modules/sentinel", "root application"),
            ("member/node_modules/sentinel", "member application"),
            (".tapid-managed", "unowned root"),
            ("member/.tapid-managed", "unowned member"),
        ] {
            project.write(path, bytes.as_bytes()).unwrap();
        }
        let journal = LifecycleJournal::begin(
            project.path(),
            Path::new("package.json"),
            "123-deadbeef\n",
            b"old root",
            Some(b"old lock"),
        )
        .unwrap();
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["kind"] = serde_json::json!("Metadata");
        record["state"] = serde_json::json!("Committed");
        record["metadata_outputs"] = serde_json::json!([
            {"path":"member/package.json", "before":STANDARD.encode(b"old member"), "after":STANDARD.encode(b"new member")},
            {"path":"package.json", "before":STANDARD.encode(b"old root"), "after":STANDARD.encode(b"new root")},
            {"path":"tapid.lock", "before":STANDARD.encode(b"old lock"), "after":STANDARD.encode(b"new lock")}
        ]);
        fs::write(&path, record.to_string()).unwrap();
        let decision = recover(project.path(), Some("123-deadbeef\n"))
            .unwrap()
            .unwrap();
        assert!(decision.metadata_only);
        // Repeated recovery accepts already-published outputs. Ordinary acquisition
        // must dispatch metadata recovery without treating it as layout recovery.
        let lock = crate::filesystem::activation::ActivationLock::acquire(project.path()).unwrap();
        assert!(!project.path().join(JOURNAL).exists());
        drop(lock);
        for (path, expected) in [
            ("package.json", "new root"),
            ("member/package.json", "new member"),
            ("tapid.lock", "new lock"),
            ("node_modules/sentinel", "root application"),
            ("member/node_modules/sentinel", "member application"),
            (".tapid-managed", "unowned root"),
            ("member/.tapid-managed", "unowned member"),
        ] {
            assert_eq!(
                fs::read(project.path().join(path)).unwrap(),
                expected.as_bytes()
            );
        }
        finish_recovery(project.path()).unwrap();
    }

    #[test]
    fn metadata_recovery_validates_all_outputs_before_mutation() {
        let project = tapid_test_support::TempProject::new("metadata-stale").unwrap();
        project.write("package.json", b"old manifest").unwrap();
        project.write("tapid.lock", b"concurrent lock").unwrap();
        let journal = LifecycleJournal::begin(
            project.path(),
            Path::new("package.json"),
            "123-deadbeef\n",
            b"old manifest",
            Some(b"old lock"),
        )
        .unwrap();
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["kind"] = serde_json::json!("Metadata");
        record["state"] = serde_json::json!("Committed");
        record["metadata_outputs"] = serde_json::json!([
            {"path":"package.json", "before":STANDARD.encode(b"old manifest"), "after":STANDARD.encode(b"new manifest")},
            {"path":"tapid.lock", "before":STANDARD.encode(b"old lock"), "after":STANDARD.encode(b"new lock")}
        ]);
        fs::write(&path, record.to_string()).unwrap();
        assert!(
            recover(project.path(), None)
                .unwrap_err()
                .contains("changed since preparation")
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"old manifest"
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            b"concurrent lock"
        );
        project.write("tapid.lock", b"old lock").unwrap();
        record["state"] = serde_json::json!("Prepared");
        fs::write(&path, record.to_string()).unwrap();
        assert!(!recover(project.path(), None).unwrap().unwrap().committed);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"old manifest"
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            b"old lock"
        );
        record["state"] = serde_json::json!("Committed");
        for mutation in [
            serde_json::json!({"store_root":"/invalid-store"}),
            serde_json::json!({"members":[{"path":"member", "marker_existed":false,"node_modules_existed":false}]}),
            serde_json::json!({"metadata_outputs":[]}),
        ] {
            let mut invalid_record = record.clone();
            for (key, value) in mutation.as_object().unwrap() {
                invalid_record[key] = value.clone();
            }
            fs::write(&path, invalid_record.to_string()).unwrap();
            assert!(recover(project.path(), None).is_err());
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                b"old manifest"
            );
        }
        let mut malformed = record.clone();
        malformed["metadata_outputs"][1]["after"] = serde_json::json!("not base64!");
        fs::write(&path, malformed.to_string()).unwrap();
        assert!(recover(project.path(), None).is_err());
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            b"old manifest"
        );
        for invalid in [
            "../package.json",
            "node_modules/package.json",
            "member/tapid.lock",
            "tapid.lock",
        ] {
            let mut invalid_record = record.clone();
            invalid_record["metadata_outputs"][0]["path"] = serde_json::json!(invalid);
            fs::write(&path, invalid_record.to_string()).unwrap();
            assert!(recover(project.path(), None).is_err(), "{invalid}");
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                b"old manifest"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_recovery_rejects_redirected_parent() {
        let project = tapid_test_support::TempProject::new("metadata-redirect").unwrap();
        project.write("package.json", b"old").unwrap();
        project.write("real/package.json", b"member").unwrap();
        std::os::unix::fs::symlink(project.path().join("real"), project.path().join("alias"))
            .unwrap();
        let journal = LifecycleJournal::begin(
            project.path(),
            Path::new("package.json"),
            "123-deadbeef\n",
            b"old",
            None,
        )
        .unwrap();
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["kind"] = serde_json::json!("Metadata");
        record["state"] = serde_json::json!("Committed");
        record["metadata_outputs"] = serde_json::json!([
            {"path":"alias/package.json", "before":STANDARD.encode(b"member"), "after":STANDARD.encode(b"changed")}
        ]);
        fs::write(&path, record.to_string()).unwrap();
        assert!(
            recover(project.path(), None)
                .unwrap_err()
                .contains("redirected")
        );
        assert_eq!(
            fs::read(project.path().join("real/package.json")).unwrap(),
            b"member"
        );
        record["metadata_outputs"][0]["path"] = serde_json::json!("real/./package.json");
        fs::write(&path, record.to_string()).unwrap();
        assert!(recover(project.path(), None).is_err());
        assert_eq!(
            fs::read(project.path().join("real/package.json")).unwrap(),
            b"member"
        );
    }

    #[test]
    fn activation_journal_records_explicit_transaction_kind() {
        let project = tapid_test_support::TempProject::new("activation-journal-kind").unwrap();
        project.write("package.json", b"{}").unwrap();
        let mut journal = LifecycleJournal::begin(
            project.path(),
            Path::new("package.json"),
            "123-deadbeef\n",
            b"{}",
            None,
        )
        .unwrap();
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(project.path().join(JOURNAL)).unwrap()).unwrap();
        assert_eq!(record["version"], 4);
        assert_eq!(record["kind"], "Activation");
        assert!(!journal.settle().unwrap());
        assert!(!project.path().join(JOURNAL).exists());
    }

    #[test]
    fn activation_recovery_rejects_missing_or_unsupported_transaction_kind_before_mutation() {
        let project = tapid_test_support::TempProject::new("journal-kind-rejection").unwrap();
        project.write("package.json", b"current manifest").unwrap();
        project.write("tapid.lock", b"current lock").unwrap();
        project
            .write("node_modules/sentinel", b"running application")
            .unwrap();
        let journal = LifecycleJournal::begin(
            project.path(),
            Path::new("package.json"),
            "123-deadbeef\n",
            b"old manifest",
            Some(b"old lock"),
        )
        .unwrap();
        // Leave the journal as an interrupted transaction without running Drop settlement.
        std::mem::forget(journal);
        let path = project.path().join(JOURNAL);
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for (version, kind) in [
            (4, None),
            (4, Some("Metadata")),
            (4, Some("Unknown")),
            (3, Some("Activation")),
            (5, Some("Activation")),
        ] {
            let mut record = original.clone();
            record["version"] = serde_json::json!(version);
            if let Some(kind) = kind {
                record["kind"] = serde_json::json!(kind);
            } else {
                record.as_object_mut().unwrap().remove("kind");
            }
            fs::write(&path, record.to_string()).unwrap();
            assert!(
                recover(project.path(), Some("123-deadbeef\n")).is_err(),
                "version {version}, kind {kind:?}"
            );
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                b"current manifest"
            );
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                b"current lock"
            );
            assert_eq!(
                fs::read(project.path().join("node_modules/sentinel")).unwrap(),
                b"running application"
            );
        }
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
            record.as_object_mut().unwrap().remove("kind");
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
        record.as_object_mut().unwrap().remove("kind");
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
            kind: None,
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
            metadata_outputs: Vec::new(),
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
            kind: None,
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
            metadata_outputs: Vec::new(),
        };
        let committed = Record {
            version: prepared.version,
            kind: prepared.kind,
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
            metadata_outputs: Vec::new(),
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
            kind: None,
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
            metadata_outputs: Vec::new(),
        };
        let mut committed = Record {
            version: prepared.version,
            kind: prepared.kind,
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
            metadata_outputs: Vec::new(),
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
