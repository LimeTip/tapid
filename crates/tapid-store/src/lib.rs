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
    pub fn artifact_path(&self, digest: &ArtifactDigest) -> PathBuf {
        self.root.join("artifacts").join(digest.as_str())
    }

    pub fn ingest_archive(
        &self,
        bytes: &[u8],
        expected_archive: &ArtifactDigest,
        expected_tree: &ArtifactDigest,
        format: tapid_archive::ArchiveFormat,
        limits: tapid_archive::ArchiveLimits,
    ) -> Result<IngestResult, IngestError> {
        let actual_archive = digest_bytes(bytes);
        if actual_archive != expected_archive.as_str() {
            return Err(IngestError::DigestMismatch {
                expected: expected_archive.clone(),
                actual: actual_archive,
            });
        }
        let destination = self.root.join("trees").join(expected_tree.as_str());
        if destination.exists() {
            self.verified_tree_path(expected_tree)?;
            return Ok(IngestResult::AlreadyPresent(destination));
        }
        let staging = private_staging_path(&self.root, "tree")?;
        tapid_archive::extract_to(bytes, format, &staging, limits)?;
        let actual_tree = tapid_archive::canonical_tree_digest(&staging)?;
        if actual_tree != expected_tree.as_str() {
            let _ = fs::remove_dir_all(&staging);
            return Err(IngestError::TreeDigestMismatch {
                expected: expected_tree.clone(),
                actual: actual_tree,
            });
        }
        fs::write(staging.join(".tapid-tree"), expected_tree.as_str())?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(&staging, &destination) {
            Ok(()) => Ok(IngestResult::Activated(destination)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_dir_all(&staging);
                self.verified_tree_path(expected_tree)?;
                Ok(IngestResult::AlreadyPresent(destination))
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&staging);
                Err(e.into())
            }
        }
    }

    /// Returns a verified package tree. A tree is activated by store tooling
    /// with a `.tapid-tree` marker containing the exact digest; install never
    /// trusts an unmarked directory or a symlink.
    pub fn verified_tree_path(&self, digest: &ArtifactDigest) -> Result<PathBuf, IngestError> {
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
        if !source.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tree source is not a directory",
            )
            .into());
        }
        let actual = tapid_archive::canonical_tree_digest(source)?;
        if actual != digest.as_str() {
            return Err(IngestError::TreeDigestMismatch {
                expected: digest.clone(),
                actual,
            });
        }
        let destination = self.root.join("trees").join(digest.as_str());
        if destination.exists() {
            self.verified_tree_path(digest)?;
            return Ok(destination);
        }
        let staging = create_private_staging_dir(&self.root, "tree")?;
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
            fs::create_dir_all(destination.parent().expect("tree destination has parent"))?;
            match fs::rename(&staging, &destination) {
                Ok(()) => Ok(destination.clone()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    let existing = self.verified_tree_path(digest);
                    let _ = fs::remove_dir_all(&staging);
                    existing
                }
                Err(error) => Err(error.into()),
            }
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
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
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::str::FromStr;
    fn digest(data: &[u8]) -> ArtifactDigest {
        let mut h = Sha256::new();
        h.update(data);
        ArtifactDigest::from_str(&format!("sha256-{}", hex::encode(h.finalize()))).unwrap()
    }
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "tapid-store-test-{}-{}",
            std::process::id(),
            unique_nonce()
        ))
    }
    #[test]
    fn streams_verifies_and_atomically_activates() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let data = b"hostile input";
        let store = Store::new(&root);
        let expected = digest(data);
        let result = store.ingest(&expected, Cursor::new(data)).unwrap();
        assert!(matches!(result, IngestResult::Activated(_)));
        assert_eq!(fs::read(store.artifact_path(&expected)).unwrap(), data);
        assert!(!root.join(".staging").read_dir().unwrap().any(|x| x.is_ok()));
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn rejects_bad_digest_and_leaves_no_activated_bytes() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let expected = digest(b"right");
        assert!(matches!(
            store.ingest(&expected, Cursor::new(b"wrong")),
            Err(IngestError::DigestMismatch { .. })
        ));
        assert!(!store.artifact_path(&expected).exists());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn existing_files_are_authoritative_and_idempotent() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let data = b"one";
        let expected = digest(data);
        let path = store.artifact_path(&expected);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        assert_eq!(
            store.ingest(&expected, Cursor::new(b"different")).unwrap(),
            IngestResult::AlreadyPresent(path)
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn existing_wrong_bytes_are_rejected() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let expected = digest(b"expected");
        let path = store.artifact_path(&expected);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            store.ingest(&expected, Cursor::new(b"expected")),
            Err(IngestError::DigestMismatch { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activation_rejects_source_that_does_not_match_digest() {
        let root = root();
        let source = root.join("source");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"tampered").unwrap();
        let store = Store::new(&root);
        let expected = digest(b"not the canonical tree digest");
        assert!(matches!(
            store.activate_verified_tree(&expected, &source),
            Err(IngestError::TreeDigestMismatch { .. })
        ));
        assert!(!store.artifact_path(&expected).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verified_tree_rejects_mutation_after_activation() {
        let root = root();
        let source = root.join("source");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"original").unwrap();
        let expected = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse::<ArtifactDigest>()
            .unwrap();
        let store = Store::new(&root);
        store.activate_verified_tree(&expected, &source).unwrap();
        fs::write(
            store
                .root()
                .join("trees")
                .join(expected.as_str())
                .join("package.json"),
            b"tampered",
        )
        .unwrap();
        assert!(matches!(
            store.verified_tree_path(&expected),
            Err(IngestError::TreeDigestMismatch { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_replay_snapshot_cleanup_preserves_live_and_unrelated_entries() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let staging = root.join(".staging");
        let stale = staging.join("replay-tree-4242-1");
        let live = staging.join("replay-tree-4243-2");
        let unrelated = staging.join("tree-4242-3");
        for path in [&stale, &live, &unrelated] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("data"), b"data").unwrap();
        }
        let store = Store::new(&root);

        store
            .cleanup_stale_replay_snapshots_with(|pid| pid == 4243)
            .unwrap();

        assert!(!stale.exists());
        assert!(live.is_dir());
        assert!(unrelated.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_shared_replay_lease_is_recovered_even_when_pid_was_reused() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let staging = root.join(".staging");
        fs::create_dir_all(&staging).unwrap();
        let stale = staging.join("replay-lease-4242-1");
        fs::write(&stale, b"lease-v1\n").unwrap();
        let store = Store::new(&root);

        store
            .cleanup_stale_replay_snapshots_with(|pid| pid == 4242)
            .unwrap();

        assert!(!stale.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unlocked_snapshot_is_stale_even_when_its_pid_was_reused() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let snapshot = root.join(".staging/replay-tree-4242-1");
        fs::create_dir_all(&snapshot).unwrap();
        fs::write(snapshot.join(".tapid-replay-lease"), b"lease-v1\n").unwrap();
        let store = Store::new(&root);

        store
            .cleanup_stale_replay_snapshots_with(|pid| pid == 4242)
            .unwrap();

        assert!(!snapshot.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn created_snapshot_holds_a_live_lease() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"leased\"}").unwrap();
        let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse()
            .unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let store = Store::new(&root);
        let snapshot = store
            .verified_tree_snapshot_with(&digest, |_, _| Ok(false))
            .unwrap();

        store
            .cleanup_stale_replay_snapshots_with(|_| false)
            .unwrap();

        assert!(snapshot.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deleted_snapshot_tree_releases_its_lease_for_parent_recovery() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"released\"}").unwrap();
        let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse()
            .unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let store = Store::new(&root);
        let snapshot = store.verified_tree_snapshot(&digest).unwrap();
        let reservation = snapshot.parent().unwrap().to_owned();
        fs::remove_dir_all(snapshot).unwrap();

        store.cleanup_stale_replay_snapshots().unwrap();

        assert!(!reservation.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_clone_target_is_inside_an_atomically_reserved_lease_directory() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"reserved\"}").unwrap();
        let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse()
            .unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let store = Store::new(&root);

        let result = store.verified_tree_snapshot_with(&digest, |_, target| {
            let reservation = target.parent().unwrap();
            assert!(reservation.join(REPLAY_LEASE).is_file());
            assert!(
                reservation
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("replay-tree-")
            );
            Err(io::Error::other("injected after reservation"))
        });

        assert!(result.is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn failed_snapshot_does_not_delete_a_substituted_reservation() {
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"substitution\"}").unwrap();
        let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse()
            .unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let store = Store::new(&root);
        let mut substituted = None;

        let result = store.verified_tree_snapshot_with(&digest, |_, target| {
            let reservation = target.parent().unwrap().to_owned();
            fs::remove_dir_all(&reservation)?;
            fs::create_dir(&reservation)?;
            fs::write(reservation.join("competitor"), b"keep")?;
            substituted = Some(reservation);
            Err(io::Error::other("injected after substitution"))
        });

        assert!(result.is_err());
        let substituted = substituted.unwrap();
        assert_eq!(fs::read(substituted.join("competitor")).unwrap(), b"keep");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replay_snapshot_forced_clone_fallback_copies_verified_bytes() {
        SNAPSHOT_BYTE_COPY_COUNT.set(0);
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"fallback\"}").unwrap();
        let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse()
            .unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let store = Store::new(&root);

        let snapshot = store
            .verified_tree_snapshot_with(&digest, |_, _| Ok(false))
            .unwrap();

        assert!(SNAPSHOT_BYTE_COPY_COUNT.get() > 0);
        assert_eq!(
            fs::read(snapshot.join("package.json")).unwrap(),
            b"{\"name\":\"fallback\"}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn failed_clone_cleanup_removes_a_partial_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"cleanup\"}").unwrap();
        let digest: ArtifactDigest = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse()
            .unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), b"keep").unwrap();
        let store = Store::new(&root);

        let result = store.verified_tree_snapshot_with(&digest, |_, snapshot| {
            symlink(&outside, snapshot)?;
            Err(io::Error::other("injected clone failure"))
        });

        assert!(result.is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
        let staging = root.join(".staging");
        assert!(fs::read_dir(staging).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("replay-tree-")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verified_tree_rejects_non_exact_marker_contents() {
        let root = root();
        let source = root.join("source");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"original").unwrap();
        let expected = tapid_archive::canonical_tree_digest(&source)
            .unwrap()
            .parse::<ArtifactDigest>()
            .unwrap();
        let store = Store::new(&root);
        let destination = store.activate_verified_tree(&expected, &source).unwrap();

        for marker in [
            format!("{}\n", expected.as_str()),
            format!(" {} ", expected.as_str()),
            format!("{}\nextra", expected.as_str()),
        ] {
            fs::write(destination.join(".tapid-tree"), marker).unwrap();
            assert!(matches!(
                store.verified_tree_path(&expected),
                Err(IngestError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
            ));
        }

        fs::write(destination.join(".tapid-tree"), expected.as_str()).unwrap();
        assert!(store.verified_tree_path(&expected).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replay_snapshot_is_verified_and_detached_from_store_mutations() {
        SNAPSHOT_BYTE_COPY_COUNT.set(0);
        SNAPSHOT_CLONE_COUNT.set(0);
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{\"name\":\"fixture\"}").unwrap();
        let tree_digest = tapid_archive::canonical_tree_digest(&source).unwrap();
        let digest = ArtifactDigest::from_str(&tree_digest).unwrap();
        let destination = root.join("trees").join(digest.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::rename(&source, &destination).unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        let store = Store::new(&root);

        let snapshot = store.verified_tree_snapshot(&digest).unwrap();
        #[cfg(target_os = "macos")]
        {
            assert!(SNAPSHOT_CLONE_COUNT.get() <= 1);
            if SNAPSHOT_CLONE_COUNT.get() == 1 {
                assert_eq!(SNAPSHOT_BYTE_COPY_COUNT.get(), 0);
            } else {
                assert!(SNAPSHOT_BYTE_COPY_COUNT.get() > 0);
            }
        }
        fs::remove_dir_all(&destination).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("package.json"), b"replacement").unwrap();
        fs::write(destination.join(".tapid-tree"), digest.as_str()).unwrap();
        assert_eq!(
            fs::read(snapshot.join("package.json")).unwrap(),
            b"{\"name\":\"fixture\"}"
        );
        assert_eq!(
            tapid_archive::canonical_tree_digest(&snapshot).unwrap(),
            digest.as_str()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn tree_copy_handles_nested_directories_and_preserves_executable_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(source.join("nested").join("data"), b"nested").unwrap();
        let bin = source.join("bin");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(&target).unwrap();
        copy_tree_contents(&source, &target).unwrap();
        assert_eq!(
            fs::read(target.join("nested").join("data")).unwrap(),
            b"nested"
        );
        assert_eq!(
            fs::metadata(target.join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            fs::metadata(target.join("bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reader_failure_does_not_activate_partial_file() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::Interrupted, "stop"))
            }
        }
        let root = root();
        let _ = fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let expected = digest(b"x");
        assert!(store.ingest(&expected, Failing).is_err());
        assert!(!store.artifact_path(&expected).exists());
        let _ = fs::remove_dir_all(root);
    }
}
