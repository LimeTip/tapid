//! Filesystem-authoritative, content-addressed artifact ingestion.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tapid_core::ArtifactDigest;

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
}
impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "store I/O error: {e}"),
            Self::DigestMismatch { expected, actual } => {
                write!(f, "digest mismatch: expected {expected}, got {actual}")
            }
            Self::InvalidRoot => f.write_str("store root must not be empty"),
        }
    }
}
impl std::error::Error for IngestError {}
impl From<io::Error> for IngestError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
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
        if destination.is_file() {
            return Ok(IngestResult::AlreadyPresent(destination));
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
            std::thread::current().name().unwrap_or("x")
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
