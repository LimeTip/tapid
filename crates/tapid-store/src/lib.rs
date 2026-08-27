//! Filesystem-authoritative, content-addressed artifact ingestion.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
        let staging_dir = self.root.join(".staging");
        fs::create_dir_all(&staging_dir)?;
        let staging = staging_dir.join(format!("tree-{}-{}", std::process::id(), unique_nonce()));
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
        let path = self.root.join("trees").join(digest.as_str());
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_dir() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "tree is not a directory").into(),
            );
        }
        let marker = path.join(".tapid-tree");
        let marker_meta = fs::symlink_metadata(&marker)?;
        if !marker_meta.file_type().is_file()
            || fs::read_to_string(&marker)?.trim() != digest.as_str()
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "store tree is not verified").into(),
            );
        }
        let actual = tapid_archive::canonical_tree_digest(&path)?;
        if actual != digest.as_str() {
            return Err(IngestError::TreeDigestMismatch {
                expected: digest.clone(),
                actual,
            });
        }
        Ok(path)
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
        let staging_dir = self.root.join(".staging");
        fs::create_dir_all(&staging_dir)?;
        let staging = staging_dir.join(format!("tree-{}-{}", std::process::id(), unique_nonce()));
        copy_tree(source, &staging)?;
        fs::write(staging.join(".tapid-tree"), digest.as_str())?;
        fs::create_dir_all(destination.parent().expect("tree destination has parent"))?;
        match fs::rename(&staging, &destination) {
            Ok(()) => Ok(destination),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_dir_all(&staging);
                self.verified_tree_path(digest)
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                Err(error.into())
            }
        }
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

fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
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
            copy_tree(&src, &dst)?;
        } else if meta.is_file() {
            fs::copy(&src, &dst)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported tree entry",
            ));
        }
    }
    Ok(())
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
