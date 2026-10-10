//! Filesystem-authoritative, content-addressed artifact ingestion.

use fs4::{FileExt, TryLockError};
use same_file::Handle;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tapid_core::ArtifactDigest;

mod lifecycle;

const REPLAY_LEASE: &str = ".tapid-replay-lease";

#[cfg(test)]
thread_local! {
    static SNAPSHOT_BYTE_COPY_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SNAPSHOT_CLONE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IngestResult {
    Activated(PathBuf),
    AlreadyPresent(PathBuf),
}

/// Holds a shared store lock while callers consume paths returned by `Store`.
pub struct StoreReadGuard {
    _file: File,
}

/// Stages verified trees privately until they are published as one transaction.
pub struct StoreTransaction {
    store: Store,
    staged: Vec<(ArtifactDigest, PathBuf)>,
}

/// Published trees remain rollback-capable until this guard is committed.
pub struct StorePublication {
    _file: File,
    published: Vec<PathBuf>,
    staged_paths: Vec<(PathBuf, PathBuf)>,
    trees_dir: PathBuf,
    created_trees_dir: bool,
    pending_journal: Option<PathBuf>,
    committed: bool,
}

#[derive(Debug)]
pub enum IngestError {
    Io(io::Error),
    DigestMismatch {
        expected: ArtifactDigest,
        actual: String,
    },
    InvalidRoot,
    Archive(tapid_archive::ExtractError),
    TreeDigestMismatch {
        expected: ArtifactDigest,
        actual: String,
    },
}
impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "store I/O error: {e}"),
            Self::DigestMismatch { expected, actual } => {
                write!(f, "digest mismatch: expected {expected}, got {actual}")
            }
            Self::InvalidRoot => f.write_str("store root must not be empty"),
            Self::Archive(e) => write!(f, "archive extraction error: {e}"),
            Self::TreeDigestMismatch { expected, actual } => {
                write!(f, "tree digest mismatch: expected {expected}, got {actual}")
            }
        }
    }
}
impl std::error::Error for IngestError {}
impl From<io::Error> for IngestError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<tapid_archive::ExtractError> for IngestError {
    fn from(e: tapid_archive::ExtractError) -> Self {
        Self::Archive(e)
    }
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn recover_transactions(&self) -> Result<(), IngestError> {
        drop(lock_file(&self.root, true)?);
        Ok(())
    }
    pub fn artifact_path(&self, digest: &ArtifactDigest) -> PathBuf {
        self.root.join("artifacts").join(digest.as_str())
    }

    pub fn read_guard(&self) -> Result<StoreReadGuard, IngestError> {
        Ok(StoreReadGuard {
            _file: lock_file(&self.root, false)?,
        })
    }

    pub fn transaction(&self) -> StoreTransaction {
        StoreTransaction {
            store: self.clone(),
            staged: Vec::new(),
        }
    }

    pub fn ingest_archive(
        &self,
        bytes: &[u8],
        expected_archive: &ArtifactDigest,
        expected_tree: &ArtifactDigest,
        format: tapid_archive::ArchiveFormat,
        limits: tapid_archive::ArchiveLimits,
    ) -> Result<IngestResult, IngestError> {
        let mut transaction = self.transaction();
        transaction.stage_archive(bytes, expected_archive, expected_tree, format, limits)?;
        let destination = self.root.join("trees").join(expected_tree.as_str());
        let publication = transaction.publish()?;
        let already_present = !publication.published.contains(&destination);
        publication.commit()?;
        if already_present {
            Ok(IngestResult::AlreadyPresent(destination))
        } else {
            Ok(IngestResult::Activated(destination))
        }
    }

    /// Verifies a package tree's exact marker and canonical digest. The returned
    /// path is checked under a short-lived shared lock; callers that continue
    /// using it must hold `read_guard()` for the entire use.
    pub fn verified_tree_path(&self, digest: &ArtifactDigest) -> Result<PathBuf, IngestError> {
        let _guard = self.read_guard()?;
        self.verified_tree_path_unlocked(digest)
    }

    fn verified_tree_path_unlocked(&self, digest: &ArtifactDigest) -> Result<PathBuf, IngestError> {
        let path = self.marked_tree_path(digest)?;
        let actual = tapid_archive::canonical_tree_digest(&path)?;
        if actual != digest.as_str() {
            return Err(IngestError::TreeDigestMismatch {
                expected: digest.clone(),
                actual,
            });
        }
        Ok(path)
    }

    /// Checks the directory and exact marker without hashing package contents.
    /// Callers must separately verify the tree digest before trusting its bytes.
    fn marked_tree_path(&self, digest: &ArtifactDigest) -> Result<PathBuf, IngestError> {
        let path = self.root.join("trees").join(digest.as_str());
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_dir() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "tree is not a directory").into(),
            );
        }
        let marker = path.join(".tapid-tree");
        let marker_meta = fs::symlink_metadata(&marker)?;
        if !marker_meta.file_type().is_file() || fs::read_to_string(&marker)? != digest.as_str() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "store tree is not verified").into(),
            );
        }
        Ok(path)
    }

    /// Removes replay snapshots whose advisory ownership lease is no longer held.
    /// Legacy PID-only snapshots are removed only when their process is gone.
    pub fn cleanup_stale_replay_snapshots(&self) -> Result<(), IngestError> {
        self.cleanup_stale_replay_snapshots_with(process_is_alive)
    }

    fn cleanup_stale_replay_snapshots_with<F>(&self, mut is_alive: F) -> Result<(), IngestError>
    where
        F: FnMut(u32) -> bool,
    {
        let staging = self.root.join(".staging");
        let metadata = match fs::symlink_metadata(&staging) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store staging path is not a directory",
            )
            .into());
        }
        for (index, entry) in fs::read_dir(&staging)?.enumerate() {
            if index >= 100_000 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "store staging directory contains too many entries",
                )
                .into());
            }
            let entry = entry?;
            let name = entry.file_name();
            if shared_replay_lease_owner(&name).is_some() {
                let path = entry.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if !metadata.file_type().is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "shared replay lease is not a regular file",
                    )
                    .into());
                }
                let identity = match Handle::from_path(&path) {
                    Ok(identity) => identity,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let stale = if shared_replay_lease_is_registered(&path) {
                    false
                } else {
                    match try_acquire_stale_replay_lease(&path) {
                        Ok(lease) => lease.is_some(),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    }
                };
                if stale {
                    remove_file_if_unchanged(&path, &identity);
                }
                continue;
            }
            let Some(pid) = replay_snapshot_owner(&name) else {
                continue;
            };
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !metadata.file_type().is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "replay snapshot staging entry is not a directory",
                )
                .into());
            }
            let identity = match Handle::from_path(entry.path()) {
                Ok(identity) => identity,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let lease = entry.path().join(REPLAY_LEASE);
            let snapshot = entry.path().join("tree");
            let registered = replay_lease_state(&snapshot);
            if registered == Some(true) && !snapshot.exists() {
                release_replay_lease(&snapshot);
                remove_dir_all_if_unchanged(&entry.path(), &identity);
                continue;
            }
            let (stale_lease, legacy_stale) = match fs::symlink_metadata(&lease) {
                Ok(metadata) if metadata.file_type().is_file() && registered.is_none() => {
                    match try_acquire_stale_replay_lease(&lease) {
                        Ok(lease) => (lease, false),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(_) => (None, false),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (None, !is_alive(pid)),
                Err(error) => return Err(error.into()),
            };
            if stale_lease.is_some() || legacy_stale {
                drop(stale_lease);
                remove_dir_all_if_unchanged(&entry.path(), &identity);
            }
        }
        Ok(())
    }

    /// Creates and validates a private replay snapshot from a marked store tree.
    ///
    /// Cloning or copying into private staging before hashing avoids validating
    /// one mutable view and then materializing another. If the source changes
    /// during snapshot creation, the completed snapshot digest fails closed.
    pub fn verified_tree_snapshot(&self, digest: &ArtifactDigest) -> Result<PathBuf, IngestError> {
        self.verified_tree_snapshot_with(digest, clone_snapshot_tree)
    }

    fn verified_tree_snapshot_with<F>(
        &self,
        digest: &ArtifactDigest,
        clone_tree: F,
    ) -> Result<PathBuf, IngestError>
    where
        F: FnOnce(&Path, &Path) -> io::Result<bool>,
    {
        let _guard = self.read_guard()?;
        self.verified_tree_snapshot_unlocked(digest, clone_tree)
    }

    fn verified_tree_snapshot_unlocked<F>(
        &self,
        digest: &ArtifactDigest,
        clone_tree: F,
    ) -> Result<PathBuf, IngestError>
    where
        F: FnOnce(&Path, &Path) -> io::Result<bool>,
    {
        let source = self.marked_tree_path(digest)?;
        let reservation = create_replay_reservation(&self.root)?;
        let reservation_identity = Handle::from_path(&reservation)?;
        let snapshot = reservation.join("tree");
        let result = (|| {
            if !clone_tree(&source, &snapshot)? {
                fs::create_dir(&snapshot)?;
                copy_tree_contents(&source, &snapshot)?;
            }
            let actual = tapid_archive::canonical_tree_digest(&snapshot)?;
            if actual != digest.as_str() {
                return Err(IngestError::TreeDigestMismatch {
                    expected: digest.clone(),
                    actual,
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            release_replay_lease(&snapshot);
            remove_dir_all_if_unchanged(&reservation, &reservation_identity);
            return Err(error);
        }
        if let Err(error) = mark_replay_lease_ready(&snapshot) {
            release_replay_lease(&snapshot);
            remove_dir_all_if_unchanged(&reservation, &reservation_identity);
            return Err(error.into());
        }
        Ok(snapshot)
    }

    /// Activates a tree only after recomputing its canonical digest. This is
    /// intentionally a copy operation: callers cannot mark arbitrary bytes as
    /// verified merely by supplying a digest.
    pub fn activate_verified_tree(
        &self,
        digest: &ArtifactDigest,
        source: &Path,
    ) -> Result<PathBuf, IngestError> {
        let mut transaction = self.transaction();
        transaction.stage_verified_tree(digest, source)?;
        transaction.publish()?.commit()?;
        Ok(self.root.join("trees").join(digest.as_str()))
    }

    /// Stream bytes into a private staging file, verify SHA-256, then atomically
    /// activate it under the digest path. Existing activated bytes are never
    /// replaced: the filesystem is the source of truth for idempotency.
    pub fn ingest<R: Read>(
        &self,
        expected: &ArtifactDigest,
        mut input: R,
    ) -> Result<IngestResult, IngestError> {
        if self.root.as_os_str().is_empty() {
            return Err(IngestError::InvalidRoot);
        }
        let destination = self.artifact_path(expected);
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            if !metadata.file_type().is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "artifact path is not a regular file",
                )
                .into());
            }
            let actual = digest_file(&destination)?;
            if actual == expected.as_str() {
                return Ok(IngestResult::AlreadyPresent(destination));
            }
            return Err(IngestError::DigestMismatch {
                expected: expected.clone(),
                actual,
            });
        }
        if destination.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "artifact path is not a regular file",
            )
            .into());
        }
        let staging_dir = self.root.join(".staging");
        fs::create_dir_all(&staging_dir)?;
        let (staging, mut file) = create_staging_file(&staging_dir)?;
        let result = (|| {
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = input.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                file.write_all(&buffer[..read])?;
                hasher.update(&buffer[..read]);
            }
            file.sync_all()?;
            let actual_hex = hex::encode(hasher.finalize());
            let actual = format!("sha256-{actual_hex}");
            if actual != expected.as_str() {
                return Err(IngestError::DigestMismatch {
                    expected: expected.clone(),
                    actual,
                });
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            // hard_link is atomic create-without-replace on the same
            // filesystem, unlike Unix rename which may overwrite a race.
            match fs::hard_link(&staging, &destination) {
                Ok(()) => {
                    fs::remove_file(&staging)?;
                    Ok(IngestResult::Activated(destination.clone()))
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    Ok(IngestResult::AlreadyPresent(destination.clone()))
                }
                Err(error) => Err(error.into()),
            }
        })();
        drop(file);
        let _ = fs::remove_file(&staging);
        result
    }
}

fn copy_tree_contents(source: &Path, target: &Path) -> io::Result<()> {
    for item in fs::read_dir(source)? {
        let item = item?;
        let src = item.path();
        let dst = target.join(item.file_name());
        let meta = fs::symlink_metadata(&src)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "symlink in tree source",
            ));
        }
        if meta.is_dir() {
            fs::create_dir(&dst)?;
            copy_tree_contents(&src, &dst)?;
            fs::set_permissions(&dst, meta.permissions())?;
        } else if meta.is_file() {
            let mut input = OpenOptions::new().read(true).open(&src)?;
            let mut output = OpenOptions::new().write(true).create_new(true).open(&dst)?;
            #[cfg(test)]
            SNAPSHOT_BYTE_COPY_COUNT.set(SNAPSHOT_BYTE_COPY_COUNT.get() + 1);
            io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            fs::set_permissions(&dst, meta.permissions())?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported tree entry",
            ));
        }
    }
    Ok(())
}

fn replay_snapshot_owner(name: &std::ffi::OsStr) -> Option<u32> {
    replay_owner(name, "replay-tree-")
}

fn shared_replay_lease_owner(name: &std::ffi::OsStr) -> Option<u32> {
    replay_owner(name, "replay-lease-")
}

fn replay_owner(name: &std::ffi::OsStr, prefix: &str) -> Option<u32> {
    let name = name.to_str()?.strip_prefix(prefix)?;
    let (pid, nonce) = name.split_once('-')?;
    if pid.is_empty()
        || nonce.is_empty()
        || !pid.bytes().all(|byte| byte.is_ascii_digit())
        || !nonce.bytes().all(|byte| byte.is_ascii_digit())
        || nonce.parse::<u128>().is_err()
    {
        return None;
    }
    pid.parse::<u32>().ok().filter(|pid| *pid > 0)
}

fn try_acquire_stale_replay_lease(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid as _) else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(rustix::io::Errno::SRCH) => false,
        Err(_) => true,
    }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError},
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    // SAFETY: OpenProcess receives a numeric PID and returns an owned handle;
    // successful handles are closed exactly once below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle != 0 {
        // SAFETY: handle is nonzero and owned by this function.
        unsafe { CloseHandle(handle) };
        true
    } else {
        // Access denied still proves that a process occupies the PID.
        (unsafe { GetLastError() }) == ERROR_ACCESS_DENIED
    }
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    // Preserve snapshots when this platform lacks a trusted process probe.
    true
}

#[cfg(target_os = "macos")]
fn clone_snapshot_tree(source: &Path, target: &Path) -> io::Result<bool> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    const CLONE_NOFOLLOW_ANY: u32 = 0x0008;
    let canonical_source = fs::canonicalize(source)?;
    let target_parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot has no parent"))?;
    let canonical_target =
        fs::canonicalize(target_parent)?.join(target.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "snapshot has no file name")
        })?);
    let source_path = CString::new(canonical_source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let target_path = CString::new(canonical_target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target path contains NUL"))?;
    // SAFETY: Both pointers remain valid NUL-terminated paths for the call.
    // Store and staging prefixes are intentionally canonicalized after their
    // types are checked. NOFOLLOW_ANY rejects links within the cloned tree,
    // and the completed private snapshot is independently digest-verified.
    if unsafe {
        libc::clonefile(
            source_path.as_ptr(),
            target_path.as_ptr(),
            CLONE_NOOWNERCOPY | CLONE_NOFOLLOW_ANY,
        )
    } == 0
    {
        #[cfg(test)]
        SNAPSHOT_CLONE_COUNT.set(SNAPSHOT_CLONE_COUNT.get() + 1);
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    let unsupported = error.raw_os_error().is_some_and(|code| {
        [libc::EXDEV, libc::ENOTSUP, libc::ENOSYS, libc::EINVAL].contains(&code)
    });
    if unsupported && !target.exists() {
        return Ok(false);
    }
    Err(error)
}

#[cfg(not(target_os = "macos"))]
fn clone_snapshot_tree(_source: &Path, _target: &Path) -> io::Result<bool> {
    Ok(false)
}

struct ReplayLeaseGroup {
    root: PathBuf,
    lease_path: PathBuf,
    _file: File,
    snapshots: BTreeMap<PathBuf, bool>,
}

static REPLAY_LEASES: OnceLock<Mutex<Vec<ReplayLeaseGroup>>> = OnceLock::new();

fn create_replay_reservation(root: &Path) -> io::Result<PathBuf> {
    let reservation = create_private_staging_dir(root, "replay-tree")?;
    let lease_path = reservation.join(REPLAY_LEASE);
    let result = (|| {
        let groups = REPLAY_LEASES.get_or_init(|| Mutex::new(Vec::new()));
        let mut groups = groups
            .lock()
            .map_err(|_| io::Error::other("replay lease registry is poisoned"))?;
        let group_index = if let Some(index) = groups.iter().position(|group| group.root == root) {
            index
        } else {
            let staging = reservation
                .parent()
                .ok_or_else(|| io::Error::other("replay reservation has no staging parent"))?;
            let provisional = reservation.join(".tapid-replay-group-lease");
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&provisional)?;
            file.write_all(b"lease-v1\n")?;
            file.sync_all()?;
            lock_replay_lease(&file)?;
            let mut claimed = None;
            for _ in 0..64 {
                let candidate = staging.join(format!(
                    "replay-lease-{}-{}",
                    std::process::id(),
                    unique_nonce()
                ));
                match fs::hard_link(&provisional, &candidate) {
                    Ok(()) => {
                        claimed = Some(candidate);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            let lease_path = claimed.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cannot allocate shared replay lease",
                )
            })?;
            fs::remove_file(&provisional)?;
            groups.push(ReplayLeaseGroup {
                root: root.to_owned(),
                lease_path,
                _file: file,
                snapshots: BTreeMap::new(),
            });
            groups.len() - 1
        };
        let group = &mut groups[group_index];
        fs::hard_link(&group.lease_path, &lease_path)?;
        group.snapshots.insert(reservation.join("tree"), false);
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&reservation);
        return Err(error);
    }
    Ok(reservation)
}

fn replay_lease_state(snapshot: &Path) -> Option<bool> {
    REPLAY_LEASES
        .get()
        .and_then(|groups| groups.lock().ok())
        .and_then(|groups| {
            groups
                .iter()
                .find_map(|group| group.snapshots.get(snapshot).copied())
        })
}

fn shared_replay_lease_is_registered(path: &Path) -> bool {
    REPLAY_LEASES
        .get()
        .and_then(|groups| groups.lock().ok())
        .is_some_and(|groups| groups.iter().any(|group| group.lease_path == path))
}

fn mark_replay_lease_ready(snapshot: &Path) -> io::Result<()> {
    let groups = REPLAY_LEASES
        .get()
        .ok_or_else(|| io::Error::other("replay lease registry is unavailable"))?;
    let mut groups = groups
        .lock()
        .map_err(|_| io::Error::other("replay lease registry is poisoned"))?;
    let ready = groups
        .iter_mut()
        .find_map(|group| group.snapshots.get_mut(snapshot))
        .ok_or_else(|| io::Error::other("replay lease registration was lost"))?;
    *ready = true;
    Ok(())
}

fn release_replay_lease(snapshot: &Path) {
    if let Some(groups) = REPLAY_LEASES.get()
        && let Ok(mut groups) = groups.lock()
    {
        for group in groups.iter_mut() {
            group.snapshots.remove(snapshot);
        }
    }
}

fn remove_dir_all_if_unchanged(path: &Path, expected: &Handle) {
    let Ok(actual) = Handle::from_path(path) else {
        return;
    };
    if expected == &actual {
        let _ = fs::remove_dir_all(path);
    }
}

fn remove_file_if_unchanged(path: &Path, expected: &Handle) {
    let Ok(actual) = Handle::from_path(path) else {
        return;
    };
    if expected == &actual {
        let _ = fs::remove_file(path);
    }
}

fn lock_replay_lease(file: &File) -> io::Result<()> {
    FileExt::try_lock(file).map_err(io::Error::from)
}

fn create_staging_file(dir: &Path) -> io::Result<(PathBuf, File)> {
    for nonce in 0u64..1024 {
        let path = dir.join(format!("artifact-{}-{nonce}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate staging file",
    ))
}

fn digest_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256-{}", hex::encode(hasher.finalize())))
}

impl StoreTransaction {
    pub fn new(store: Store) -> Self {
        store.transaction()
    }

    pub fn stage_archive(
        &mut self,
        bytes: &[u8],
        expected_archive: &ArtifactDigest,
        expected_tree: &ArtifactDigest,
        format: tapid_archive::ArchiveFormat,
        limits: tapid_archive::ArchiveLimits,
    ) -> Result<PathBuf, IngestError> {
        let actual_archive = digest_bytes(bytes);
        if actual_archive != expected_archive.as_str() {
            return Err(IngestError::DigestMismatch {
                expected: expected_archive.clone(),
                actual: actual_archive,
            });
        }
        if let Some((_, path)) = self
            .staged
            .iter()
            .find(|(digest, _)| digest == expected_tree)
        {
            return Ok(path.clone());
        }
        let staging = private_staging_path(&self.store.root, "transaction-archive")?;
        let result = (|| {
            tapid_archive::extract_to(bytes, format, &staging, limits)?;
            let actual_tree = tapid_archive::canonical_tree_digest(&staging)?;
            if actual_tree != expected_tree.as_str() {
                return Err(IngestError::TreeDigestMismatch {
                    expected: expected_tree.clone(),
                    actual: actual_tree,
                });
            }
            fs::write(staging.join(".tapid-tree"), expected_tree.as_str())?;
            self.staged.push((expected_tree.clone(), staging.clone()));
            Ok(staging.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Copies and verifies a tree in private staging. The returned path is not
    /// visible under the shared `trees/` namespace until `publish` is called.
    pub fn stage_verified_tree(
        &mut self,
        digest: &ArtifactDigest,
        source: &Path,
    ) -> Result<PathBuf, IngestError> {
        if let Some((_, staged)) = self.staged.iter().find(|(existing, _)| existing == digest) {
            return Ok(staged.clone());
        }
        let actual = tapid_archive::canonical_tree_digest(source)?;
        if actual != digest.as_str() {
            return Err(IngestError::TreeDigestMismatch {
                expected: digest.clone(),
                actual,
            });
        }
        let staging = create_private_staging_dir(&self.store.root, "transaction-tree")?;
        let result = (|| {
            copy_tree_contents(source, &staging)?;
            let staged_digest = tapid_archive::canonical_tree_digest(&staging)?;
            if staged_digest != digest.as_str() {
                return Err(IngestError::TreeDigestMismatch {
                    expected: digest.clone(),
                    actual: staged_digest,
                });
            }
            fs::write(staging.join(".tapid-tree"), digest.as_str())?;
            self.staged.push((digest.clone(), staging.clone()));
            Ok(staging.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Publishes staged trees under an exclusive lock. The returned guard must
    /// remain alive through project activation; dropping it rolls back only the
    /// trees introduced by this transaction while readers remain blocked.
    pub fn publish(self) -> Result<StorePublication, IngestError> {
        self.publish_inner(None)
    }

    pub fn publish_for_lifecycle(
        self,
        coordinator: &Path,
    ) -> Result<StorePublication, IngestError> {
        self.publish_inner(Some(coordinator))
    }

    fn publish_inner(
        mut self,
        coordinator: Option<&Path>,
    ) -> Result<StorePublication, IngestError> {
        let file = lock_file(&self.store.root, true).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot acquire store publication lock: {error}"),
            )
        })?;
        let trees_dir = self.store.root.join("trees");
        let created_trees_dir = !trees_dir.exists() && !self.staged.is_empty();
        let mut new_digests = Vec::new();
        for (digest, _) in &self.staged {
            let destination = trees_dir.join(digest.as_str());
            if destination.exists() {
                self.store.verified_tree_path_unlocked(digest)?;
            } else {
                new_digests.push(digest.as_str().to_owned());
            }
        }
        for (digest, staged) in &self.staged {
            let destination = trees_dir.join(digest.as_str());
            if !destination.exists() {
                sync_tree(staged)?;
            }
        }
        let pending_journal = match coordinator {
            Some(coordinator) => Some(write_store_journal(
                &self.store.root,
                coordinator,
                &new_digests,
                created_trees_dir,
            )?),
            None => None,
        };
        let mut published = Vec::new();
        let mut staged_paths = Vec::new();
        for (digest, staged) in &self.staged {
            let destination = trees_dir.join(digest.as_str());
            let operation = if destination.exists() {
                self.store.verified_tree_path_unlocked(digest).map(|_| {
                    let _ = fs::remove_dir_all(staged);
                    staged_paths.push((staged.clone(), destination.clone()));
                })
            } else {
                fs::create_dir_all(destination.parent().expect("tree destination has parent"))
                    .map_err(IngestError::from)
                    .and_then(|()| fs::rename(staged, &destination).map_err(IngestError::from))
                    .map(|()| {
                        published.push(destination.clone());
                        staged_paths.push((staged.clone(), destination.clone()));
                    })
            };
            if let Err(error) = operation {
                for path in &published {
                    match fs::remove_dir_all(path) {
                        Ok(()) => {}
                        Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {}
                        Err(cleanup) => return Err(IngestError::Io(cleanup)),
                    }
                }
                if trees_dir.exists() {
                    sync_directory(&trees_dir)?;
                }
                if created_trees_dir {
                    match fs::remove_dir(&trees_dir) {
                        Ok(()) => {}
                        Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {}
                        Err(cleanup) => return Err(IngestError::Io(cleanup)),
                    }
                    sync_directory(&self.store.root)?;
                }
                if let Some(journal) = &pending_journal {
                    remove_store_journal(journal)?;
                }
                return Err(error);
            }
        }
        let sync_result = if trees_dir.exists() {
            sync_directory(&trees_dir).and_then(|()| {
                if created_trees_dir {
                    sync_directory(&self.store.root)
                } else {
                    Ok(())
                }
            })
        } else {
            Ok(())
        };
        if let Err(error) = sync_result {
            for path in &published {
                match fs::remove_dir_all(path) {
                    Ok(()) => {}
                    Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {}
                    Err(cleanup) => return Err(IngestError::Io(cleanup)),
                }
            }
            if trees_dir.exists() {
                sync_directory(&trees_dir)?;
            }
            if created_trees_dir {
                match fs::remove_dir(&trees_dir) {
                    Ok(()) => {}
                    Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {}
                    Err(cleanup) => return Err(IngestError::Io(cleanup)),
                }
                sync_directory(&self.store.root)?;
            }
            if let Some(journal) = &pending_journal {
                remove_store_journal(journal)?;
            }
            return Err(error.into());
        }
        self.staged.clear();
        Ok(StorePublication {
            _file: file,
            published,
            staged_paths,
            trees_dir,
            created_trees_dir,
            pending_journal,
            committed: false,
        })
    }
}

impl Drop for StoreTransaction {
    fn drop(&mut self) {
        for (_, path) in &self.staged {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl Drop for StorePublication {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.published {
                let _ = fs::remove_dir_all(path);
            }
            if self.created_trees_dir {
                let _ = fs::remove_dir(&self.trees_dir);
            }
        }
    }
}

impl StorePublication {
    /// Creates a digest-verified replay snapshot while this publication holds
    /// the exclusive store lock, without trying to acquire a second lock.
    /// The snapshot remains independent if the publication rolls back.
    pub fn verified_tree_snapshot(&self, digest: &ArtifactDigest) -> Result<PathBuf, IngestError> {
        let store = Store::new(
            self.trees_dir
                .parent()
                .ok_or(IngestError::InvalidRoot)?
                .to_path_buf(),
        );
        store.verified_tree_snapshot_unlocked(digest, clone_snapshot_tree)
    }

    pub fn resolve_path(&self, staged_or_existing: &Path) -> PathBuf {
        self.staged_paths
            .iter()
            .find_map(|(staged, published)| {
                (staged == staged_or_existing).then(|| published.clone())
            })
            .unwrap_or_else(|| staged_or_existing.to_owned())
    }

    pub fn rollback(mut self) -> Result<(), IngestError> {
        let mut first_error = None;
        for path in &self.published {
            match fs::remove_dir_all(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound && first_error.is_none() => {
                    first_error = Some(IngestError::Io(error));
                }
                _ => {}
            }
        }
        if self.created_trees_dir {
            match fs::remove_dir(&self.trees_dir) {
                Err(error) if error.kind() != io::ErrorKind::NotFound && first_error.is_none() => {
                    first_error = Some(IngestError::Io(error));
                }
                _ => {}
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if self.trees_dir.exists() {
            sync_directory(&self.trees_dir)?;
        }
        if self.created_trees_dir {
            sync_directory(
                self.trees_dir
                    .parent()
                    .expect("store trees directory has a parent"),
            )?;
        }
        if let Some(journal) = &self.pending_journal {
            remove_store_journal(journal)?;
        }
        self.committed = true;
        Ok(())
    }

    pub fn commit(mut self) -> Result<(), IngestError> {
        self.committed = true;
        if let Some(journal) = &self.pending_journal {
            remove_store_journal(journal)?;
        }
        Ok(())
    }
}

fn sync_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "staged verified tree is not a regular directory",
        ));
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            sync_tree(&child)?;
        } else if metadata.is_file() {
            #[cfg(windows)]
            if metadata.permissions().readonly() {
                // Archive extraction and verified-tree copying sync file contents before
                // applying read-only permissions; opening these files for flushing fails.
                continue;
            }
            #[cfg(windows)]
            let sync = OpenOptions::new().write(true).open(&child);
            #[cfg(not(windows))]
            let sync = OpenOptions::new().read(true).open(&child);
            sync.and_then(|file| file.sync_all()).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot sync staged store file {}: {error}", child.display()),
                )
            })?;
        } else if !metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staged verified tree contains an unsupported file type",
            ));
        }
    }
    sync_directory(path)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    let sync = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0x1 | 0x2 | 0x4)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .and_then(|directory| directory.sync_all())
    };
    #[cfg(not(windows))]
    let sync = File::open(path).and_then(|directory| directory.sync_all());
    sync.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot sync store directory {}: {error}", path.display()),
        )
    })
}

const STORE_JOURNAL: &str = ".tapid-transaction.json";
const STORE_JOURNAL_MAX: u64 = 1024 * 1024;
// Lifecycle coordinator embeds base64 manifest and lockfile snapshots (each <= 32 MiB).
const COORDINATOR_MAX: u64 = 96 * 1024 * 1024;

fn write_store_journal(
    root: &Path,
    coordinator: &Path,
    created: &[String],
    created_trees_dir: bool,
) -> Result<PathBuf, IngestError> {
    let coordinator = coordinator.canonicalize()?;
    let metadata = fs::symlink_metadata(&coordinator)?;
    if !metadata.file_type().is_file() || metadata.len() > COORDINATOR_MAX {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "invalid lifecycle coordinator").into(),
        );
    }
    let coordinator_record: serde_json::Value = serde_json::from_slice(&fs::read(&coordinator)?)
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid lifecycle coordinator: {error}"),
            )
        })?;
    if !matches!(
        coordinator_record
            .get("version")
            .and_then(serde_json::Value::as_u64),
        Some(1 | 2)
    ) || coordinator_record
        .get("state")
        .and_then(serde_json::Value::as_str)
        != Some("Prepared")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lifecycle coordinator is not prepared",
        )
        .into());
    }
    let coordinator_text = coordinator.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "lifecycle coordinator path is not UTF-8",
        )
    })?;
    let record = serde_json::json!({
        "version": 1,
        "coordinator": coordinator_text,
        "owner": coordinator_record.get("owner").and_then(serde_json::Value::as_str).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "lifecycle coordinator lacks owner")
        })?,
        "created": created,
        "created_trees_dir": created_trees_dir,
    });
    let path = root.join(STORE_JOURNAL);
    let temp = root.join(format!(
        "{STORE_JOURNAL}.{}-{}.tmp",
        std::process::id(),
        unique_nonce()
    ));
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        sync_directory(root)?;
        Ok::<(), io::Error>(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result?;
    Ok(path)
}

fn recover_store_journal(root: &Path) -> io::Result<()> {
    let path = root.join(STORE_JOURNAL);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store transaction journal is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.len() > STORE_JOURNAL_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "store transaction journal is oversized",
        ));
    }
    let record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("malformed store transaction journal: {error}"),
        )
    })?;
    if record.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported store transaction journal version",
        ));
    }
    let coordinator = record
        .get("coordinator")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "store transaction journal lacks coordinator",
            )
        })?;
    let coordinator_path = Path::new(coordinator);
    if !coordinator_path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "store coordinator path is not absolute",
        ));
    }
    let owner = record
        .get("owner")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "store transaction journal lacks owner",
            )
        })?;
    let coordinator_parent = coordinator_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "store coordinator path has no project directory",
        )
    })?;
    let project_gone = match fs::symlink_metadata(coordinator_parent) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store coordinator project path is not a regular directory",
            ));
        }
        Ok(_) => false,
    };
    let committed = if project_gone {
        false
    } else {
        if coordinator_path
            .canonicalize()
            .map(|canonical| canonical != coordinator_path)
            .unwrap_or(true)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store coordinator path is unavailable or noncanonical",
            ));
        }
        let coordinator_meta = fs::symlink_metadata(coordinator_path)?;
        if !coordinator_meta.file_type().is_file() || coordinator_meta.len() > COORDINATOR_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid store transaction coordinator",
            ));
        }
        let decision: serde_json::Value = serde_json::from_slice(&fs::read(coordinator_path)?)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed lifecycle coordinator: {error}"),
                )
            })?;
        if !matches!(
            decision.get("version").and_then(serde_json::Value::as_u64),
            Some(1 | 2)
        ) || decision.get("owner").and_then(serde_json::Value::as_str) != Some(owner)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store coordinator identity mismatch",
            ));
        }
        match decision.get("state").and_then(serde_json::Value::as_str) {
            Some("Prepared") => false,
            Some("Committed") => true,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown lifecycle transaction decision",
                ));
            }
        }
    };
    let created_trees_dir = record
        .get("created_trees_dir")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "store transaction journal lacks trees-directory baseline",
            )
        })?;
    if !committed {
        let created = record
            .get("created")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "store journal lacks created tree list",
                )
            })?;
        for digest_text in created {
            let digest_text = digest_text.as_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid tree digest in store journal",
                )
            })?;
            let digest = digest_text
                .parse::<ArtifactDigest>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            let tree = root.join("trees").join(digest.as_str());
            match fs::symlink_metadata(&tree) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "refusing to remove non-directory transaction tree",
                    ));
                }
                Ok(_) => {
                    let actual = tapid_archive::canonical_tree_digest(&tree).map_err(|error| {
                        io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                    })?;
                    let marker = fs::read(tree.join(".tapid-tree"))?;
                    if actual != digest.as_str() || marker != digest.as_str().as_bytes() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "refusing to remove unverified transaction tree",
                        ));
                    }
                    fs::remove_dir_all(&tree)?;
                }
            }
        }
    }
    if !committed {
        let trees = root.join("trees");
        if trees.exists() {
            sync_directory(&trees)?;
        }
        if created_trees_dir {
            match fs::remove_dir(&trees) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            sync_directory(root)?;
        }
    }
    remove_store_journal(&path).map_err(|error| io::Error::other(error.to_string()))
}

fn remove_store_journal(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn lock_file(root: &Path, exclusive: bool) -> io::Result<File> {
    fs::create_dir_all(root)?;
    let path = root.join(".store.lock");
    let open = || -> io::Result<File> {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "store lock path is not a regular file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
    };
    if exclusive {
        let file = open()?;
        FileExt::lock(&file)?;
        if let Err(error) = recover_store_journal(root) {
            let _ = FileExt::unlock(&file);
            return Err(error);
        }
        return Ok(file);
    }
    loop {
        let file = open()?;
        FileExt::lock_shared(&file)?;
        if !root.join(STORE_JOURNAL).exists() {
            return Ok(file);
        }
        let _ = FileExt::unlock(&file);
        drop(file);

        let recovery_lock = open()?;
        FileExt::lock(&recovery_lock)?;
        let recovery = recover_store_journal(root);
        let _ = FileExt::unlock(&recovery_lock);
        recovery?;
    }
}

fn digest_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256-{}", hex::encode(h.finalize()))
}
fn unique_nonce() -> u128 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (timestamp << 32) | u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn private_staging_path(root: &Path, prefix: &str) -> io::Result<PathBuf> {
    if !root.exists() {
        fs::create_dir_all(root)?;
    }
    let root_metadata = fs::symlink_metadata(root)?;
    if !root_metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "store root is not a directory",
        ));
    }
    let staging_dir = root.join(".staging");
    match fs::symlink_metadata(&staging_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store staging path is not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(&staging_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(&staging_dir)?;
                    if !metadata.file_type().is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "store staging path is not a directory",
                        ));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }
    for _ in 0..1024 {
        let path = staging_dir.join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate private store staging path",
    ))
}

fn create_private_staging_dir(root: &Path, prefix: &str) -> io::Result<PathBuf> {
    for _ in 0..1024 {
        let path = private_staging_path(root, prefix)?;
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate private store staging directory",
    ))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
