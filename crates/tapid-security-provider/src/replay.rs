//! Durable replay floors, deliberately separate from evictable envelope bodies.
use crate::RetrievalError;

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use tapid_protocol::SecurityRequest;

#[cfg(unix)]
#[path = "replay/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "replay/windows.rs"]
#[allow(unsafe_code)]
mod platform;

/// Owner-only durable replay floors. Requires a trusted, stable parent directory.
pub struct ReplayStore {
    root: PathBuf,
    directory: platform::Directory,
}

impl ReplayStore {
    pub fn open(root: &Path) -> Result<Self, RetrievalError> {
        let directory =
            platform::Directory::open(root).map_err(|_| RetrievalError::StateUnavailable)?;
        let store = Self {
            root: root.to_path_buf(),
            directory,
        };
        let _lock = store
            .directory
            .lock()
            .map_err(|_| RetrievalError::StateUnavailable)?;
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
            let _lock = self.directory.lock()?;
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
        let _lock = self.directory.lock()?;
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
        let mut file = self.directory.stage(&stage)?;
        write!(file, "{sequence}\n{digest}\n")?;
        file.sync_all()?;
        drop(file);
        self.directory.publish(&stage, &path)
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
        let file = match self.directory.read(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() > 128 {
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
