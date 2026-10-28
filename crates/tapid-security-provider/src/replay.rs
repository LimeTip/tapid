//! Durable replay floors, deliberately separate from evictable envelope bodies.
use crate::RetrievalError;
use fs4::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use tapid_protocol::SecurityRequest;

/// Unix owner-only replay state. The caller supplies a trusted, stable parent
/// directory; it must not delete this directory when evicting cached evidence.
/// Other platforms deliberately have no persistence adapter yet.
pub struct ReplayStore {
    root: PathBuf,
    owner: u32,
    device: u64,
    inode: u64,
}

impl ReplayStore {
    pub fn open(root: &Path) -> Result<Self, RetrievalError> {
        Self::open_inner(root).map_err(|_| RetrievalError::StateUnavailable)
    }

    fn open_inner(root: &Path) -> io::Result<Self> {
        let parent = root.parent().ok_or_else(invalid)?;
        let parent_meta = fs::metadata(parent)?;
        if parent_meta.mode() & 0o022 != 0 {
            return Err(invalid());
        }
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => File::open(parent)?.sync_all()?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(root)?;
        // The caller supplies an owned parent; keep all state under that owner.
        // No HOME or credential lookup participates in this decision.
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.mode() & 0o077 != 0
            || meta.uid() != parent_meta.uid()
        {
            return Err(invalid());
        }
        let store = Self {
            root: root.to_path_buf(),
            owner: meta.uid(),
            device: meta.dev(),
            inode: meta.ino(),
        };
        let _lock = store.lock()?;
        Ok(store)
    }

    /// Load the exact issuer/request floor. Missing state alone means no floor;
    /// malformed, oversized, linked or unreadable state always fails closed.
    pub fn load(
        &self,
        issuer: &str,
        request: &SecurityRequest,
    ) -> Result<Option<(u64, String)>, RetrievalError> {
        (|| {
            let _lock = self.lock()?;
            self.read_floor(&self.path(issuer, request)?)
        })()
        .map_err(|_| RetrievalError::StateUnavailable)
    }

    /// Suitable for `retrieve_recorded`'s callback. Rechecks under the same
    /// cross-process lock, including when verification used an older floor.
    pub fn record(
        &self,
        issuer: &str,
        request: &SecurityRequest,
        sequence: u64,
        digest: &str,
    ) -> Result<(), RetrievalError> {
        self.record_inner(issuer, request, sequence, digest)
            .map_err(|_| RetrievalError::StateUnavailable)
    }

    fn record_inner(
        &self,
        issuer: &str,
        request: &SecurityRequest,
        sequence: u64,
        digest: &str,
    ) -> io::Result<()> {
        if !valid_digest(digest) {
            return Err(invalid());
        }
        let _lock = self.lock()?;
        let path = self.path(issuer, request)?;
        if let Some((old_sequence, old_digest)) = self.read_floor(&path)?
            && (sequence < old_sequence || (sequence == old_sequence && digest != old_digest))
        {
            return Err(invalid());
        }
        // A single staging name is safe under the permanent lock. An interrupted
        // write is never interpreted as committed state; replace it on retry.
        let stage = self.root.join(".pending");
        match fs::remove_file(&stage) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&stage)?;
        write!(file, "{sequence}\n{digest}\n")?;
        file.sync_all()?;
        fs::rename(stage, path)?;
        File::open(&self.root)?.sync_all()
    }

    fn lock(&self) -> io::Result<File> {
        let root = fs::symlink_metadata(&self.root)?;
        if !root.is_dir()
            || root.file_type().is_symlink()
            || root.mode() & 0o077 != 0
            || root.uid() != self.owner
            || root.dev() != self.device
            || root.ino() != self.inode
        {
            return Err(invalid());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join(".lock"))?;
        private_file(&file)?;
        if file.metadata()?.uid() != self.owner {
            return Err(invalid());
        }
        FileExt::lock(&file)?;
        Ok(file)
    }

    fn path(&self, issuer: &str, request: &SecurityRequest) -> io::Result<PathBuf> {
        request.validate_v2().map_err(|_| invalid())?;
        if issuer.is_empty() {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(&(issuer, request)).map_err(|_| invalid())?;
        Ok(self.root.join(format!("{:x}.floor", Sha256::digest(bytes))))
    }

    fn read_floor(&self, path: &Path) -> io::Result<Option<(u64, String)>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        private_file(&file)?;
        if file.metadata()?.uid() != self.owner || file.metadata()?.len() > 128 {
            return Err(invalid());
        }
        let mut text = String::new();
        file.take(128).read_to_string(&mut text)?;
        let mut lines = text.lines();
        let sequence = lines
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let digest = lines
            .next()
            .filter(|v| valid_digest(v))
            .ok_or_else(invalid)?;
        if lines.next().is_some() || text != format!("{sequence}\n{digest}\n") {
            return Err(invalid());
        }
        Ok(Some((sequence, digest.to_owned())))
    }
}

fn private_file(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(())
}
fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256-").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn invalid() -> io::Error {
    io::Error::other("unavailable security replay state")
}
