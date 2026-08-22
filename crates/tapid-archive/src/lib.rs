//! Bounded validation for hostile package archive manifests.

#![deny(unsafe_code)]

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read};
use std::path::{Component, Path};

/// Formats accepted by consumer registries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveFormat {
    Tar,
    TarGz,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveLimits {
    pub validation: ValidationLimits,
    pub max_archive_bytes: usize,
}
impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            validation: ValidationLimits::default(),
            max_archive_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    Io(io::Error),
    Invalid(ValidationError),
    InvalidArchive(String),
    ArchiveTooLarge,
}
impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "archive I/O error: {e}"),
            Self::Invalid(e) => e.fmt(f),
            Self::InvalidArchive(e) => write!(f, "invalid archive: {e}"),
            Self::ArchiveTooLarge => f.write_str("archive compressed size limit exceeded"),
        }
    }
}
impl std::error::Error for ExtractError {}
impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Validate, then extract an archive into an empty staging directory.
pub fn extract_to(
    bytes: &[u8],
    format: ArchiveFormat,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<(), ExtractError> {
    if bytes.len() > limits.max_archive_bytes {
        return Err(ExtractError::ArchiveTooLarge);
    }
    if destination.exists() {
        return Err(ExtractError::InvalidArchive(
            "destination already exists".into(),
        ));
    }
    fs::create_dir(destination)?;
    let result = extract_inner(bytes, format, destination, limits);
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn extract_inner(
    bytes: &[u8],
    format: ArchiveFormat,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<(), ExtractError> {
    let metadata = archive_metadata(bytes, format)?;
    validate_entries(metadata, limits.validation).map_err(ExtractError::Invalid)?;
    extract_entries(bytes, format, destination)?;
    Ok(())
}

fn archive_metadata(
    bytes: &[u8],
    format: ArchiveFormat,
) -> Result<Vec<ArchiveEntry>, ExtractError> {
    let reader: Box<dyn Read + '_> = match format {
        ArchiveFormat::Tar => Box::new(Cursor::new(bytes)),
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes))),
    };
    let mut archive = tar::Archive::new(reader);
    let mut result = Vec::new();
    for item in archive.entries().map_err(ExtractError::Io)? {
        let e = item.map_err(ExtractError::Io)?;
        let path = e
            .path()
            .map_err(|x| ExtractError::InvalidArchive(x.to_string()))?
            .to_string_lossy()
            .into_owned();
        let kind = if e.header().entry_type().is_dir() {
            EntryKind::Directory
        } else if e.header().entry_type().is_file() {
            EntryKind::File
        } else if e.header().entry_type().is_symlink() {
            EntryKind::Symlink
        } else if e.header().entry_type().is_hard_link() {
            EntryKind::Hardlink
        } else {
            EntryKind::Other
        };
        let size = e.header().size().map_err(ExtractError::Io)?;
        let target = if matches!(kind, EntryKind::Symlink) {
            e.link_name()
                .map_err(|x| ExtractError::InvalidArchive(x.to_string()))?
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        result.push(ArchiveEntry {
            path,
            kind,
            size,
            link_target: target,
        });
    }
    Ok(result)
}

fn extract_entries(
    bytes: &[u8],
    format: ArchiveFormat,
    destination: &Path,
) -> Result<(), ExtractError> {
    let reader: Box<dyn Read + '_> = match format {
        ArchiveFormat::Tar => Box::new(Cursor::new(bytes)),
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes))),
    };
    let mut archive = tar::Archive::new(reader);
    for item in archive.entries().map_err(ExtractError::Io)? {
        let mut entry = item.map_err(ExtractError::Io)?;
        let raw = entry
            .path()
            .map_err(|x| ExtractError::InvalidArchive(x.to_string()))?
            .to_string_lossy()
            .into_owned();
        let normalized = normalized_path(&raw).map_err(ExtractError::Invalid)?;
        let target = destination.join(
            normalized
                .split('/')
                .collect::<Vec<_>>()
                .as_slice()
                .iter()
                .collect::<std::path::PathBuf>(),
        );
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(ExtractError::InvalidArchive(
                "non-regular entry after validation".into(),
            ));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        io::copy(&mut entry, &mut out)?;
        out.sync_all()?;
    }
    Ok(())
}

/// Hash a tree using sorted relative paths and explicit type/length framing.
pub fn canonical_tree_digest(root: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    collect_tree(root, root, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for (path, kind, data) in files {
        h.update((path.len() as u64).to_be_bytes());
        h.update(path.as_bytes());
        h.update([kind]);
        h.update((data.len() as u64).to_be_bytes());
        h.update(data);
    }
    Ok(format!("sha256-{}", hex::encode(h.finalize())))
}
fn collect_tree(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, u8, Vec<u8>)>,
) -> io::Result<()> {
    for item in fs::read_dir(current)? {
        let item = item?;
        let p = item.path();
        let rel = p
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == ".tapid-tree" {
            continue;
        }
        let m = fs::symlink_metadata(&p)?;
        if m.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "symlink in extracted tree",
            ));
        }
        if m.is_dir() {
            out.push((rel.clone(), 1, Vec::new()));
            collect_tree(root, &p, out)?;
        } else if m.is_file() {
            out.push((rel, 0, fs::read(&p)?));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "special file in extracted tree",
            ));
        }
    }
    Ok(())
}

/// The kind of an archive member. Archives must not contain device nodes or
/// other filesystem objects with behavior beyond regular files and directories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Hardlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveEntry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub link_target: Option<String>,
}

impl ArchiveEntry {
    pub fn file(path: impl Into<String>, size: u64) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::File,
            size,
            link_target: None,
        }
    }
    pub fn directory(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::Directory,
            size: 0,
            link_target: None,
        }
    }
    pub fn symlink(path: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::Symlink,
            size: 0,
            link_target: Some(target.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationLimits {
    pub max_entries: usize,
    pub max_entry_size: u64,
    pub max_total_size: u64,
    pub max_path_bytes: usize,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_entry_size: 512 * 1024 * 1024,
            max_total_size: 2 * 1024 * 1024 * 1024,
            max_path_bytes: 4096,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    TooManyEntries,
    EntryTooLarge { path: String, size: u64 },
    TotalSizeExceeded,
    PathTooLong,
    AbsolutePath(String),
    Traversal(String),
    InvalidPath(String),
    DuplicatePath(String),
    CaseCollision(String),
    SpecialFile(String),
    SymlinkEscape(String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries => f.write_str("archive has too many entries"),
            Self::EntryTooLarge { path, size } => {
                write!(f, "archive entry {path} is too large ({size} bytes)")
            }
            Self::TotalSizeExceeded => f.write_str("archive total size limit exceeded"),
            Self::PathTooLong => f.write_str("archive path is too long"),
            Self::AbsolutePath(path) => write!(f, "absolute archive path: {path}"),
            Self::Traversal(path) => write!(f, "archive path traversal: {path}"),
            Self::InvalidPath(path) => write!(f, "invalid archive path: {path}"),
            Self::DuplicatePath(path) => write!(f, "duplicate archive path: {path}"),
            Self::CaseCollision(path) => write!(f, "case-colliding archive path: {path}"),
            Self::SpecialFile(path) => write!(f, "special archive file: {path}"),
            Self::SymlinkEscape(path) => write!(f, "symlink escapes archive root: {path}"),
        }
    }
}
impl std::error::Error for ValidationError {}

/// Validate entries without touching the host filesystem.
pub fn validate_entries<I>(entries: I, limits: ValidationLimits) -> Result<(), ValidationError>
where
    I: IntoIterator<Item = ArchiveEntry>,
{
    let mut count = 0usize;
    let mut total = 0u64;
    let mut exact = HashSet::new();
    let mut folded = HashSet::new();
    for entry in entries {
        count = count
            .checked_add(1)
            .ok_or(ValidationError::TooManyEntries)?;
        if count > limits.max_entries {
            return Err(ValidationError::TooManyEntries);
        }
        if entry.path.len() > limits.max_path_bytes {
            return Err(ValidationError::PathTooLong);
        }
        let normalized = normalized_path(&entry.path)?;
        if !exact.insert(normalized.clone()) {
            return Err(ValidationError::DuplicatePath(entry.path));
        }
        if !folded.insert(normalized.to_ascii_lowercase()) {
            return Err(ValidationError::CaseCollision(entry.path));
        }
        match entry.kind {
            EntryKind::File => {
                if entry.size > limits.max_entry_size {
                    return Err(ValidationError::EntryTooLarge {
                        path: entry.path,
                        size: entry.size,
                    });
                }
                total = total
                    .checked_add(entry.size)
                    .ok_or(ValidationError::TotalSizeExceeded)?;
                if total > limits.max_total_size {
                    return Err(ValidationError::TotalSizeExceeded);
                }
            }
            EntryKind::Directory => {
                if entry.size != 0 {
                    return Err(ValidationError::EntryTooLarge {
                        path: entry.path,
                        size: entry.size,
                    });
                }
            }
            EntryKind::Symlink => {
                let target = entry
                    .link_target
                    .as_deref()
                    .ok_or_else(|| ValidationError::InvalidPath(entry.path.clone()))?;
                if symlink_target_escapes(&entry.path, target) {
                    return Err(ValidationError::SymlinkEscape(entry.path));
                }
            }
            EntryKind::Hardlink | EntryKind::Other => {
                return Err(ValidationError::SpecialFile(entry.path));
            }
        }
    }
    Ok(())
}

fn normalized_path(value: &str) -> Result<String, ValidationError> {
    if value.is_empty() || value.contains('\0') {
        return Err(ValidationError::InvalidPath(value.to_owned()));
    }
    // Treat both separators as separators so a Unix extractor cannot be tricked
    // by a Windows-shaped path (and vice versa).
    let value = value.replace('\\', "/");
    let path = Path::new(&value);
    if path.is_absolute()
        || value.starts_with('/')
        || has_drive_prefix(&value)
        || value.starts_with("//")
    {
        return Err(ValidationError::AbsolutePath(value));
    }
    let mut output = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => output.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => return Err(ValidationError::Traversal(value)),
            _ => return Err(ValidationError::AbsolutePath(value)),
        }
    }
    if output.is_empty() {
        return Err(ValidationError::InvalidPath(value));
    }
    Ok(output.join("/"))
}

fn has_drive_prefix(path: &str) -> bool {
    path.as_bytes().get(1) == Some(&b':')
        && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
}

fn symlink_target_escapes(link: &str, target: &str) -> bool {
    let link = link.replace('\\', "/");
    let target = target.replace('\\', "/");
    if target.is_empty() || target.contains('\0') {
        return true;
    }
    if target.starts_with('/') || target.starts_with("//") || has_drive_prefix(&target) {
        return true;
    }
    let mut depth = link
        .split('/')
        .filter(|s| !s.is_empty())
        .count()
        .saturating_sub(1) as i32;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ok(entry: ArchiveEntry) -> Result<(), ValidationError> {
        validate_entries([entry], ValidationLimits::default())
    }
    #[test]
    fn rejects_traversal_and_absolute_cross_platform_forms() {
        for path in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:\\temp\\x",
            "\\\\server\\share\\x",
            "a\\..\\..\\x",
        ] {
            assert!(ok(ArchiveEntry::file(path, 1)).is_err(), "{path}");
        }
    }
    #[test]
    fn rejects_duplicates_and_case_collisions() {
        assert!(
            validate_entries(
                [ArchiveEntry::file("A", 1), ArchiveEntry::file("A", 1)],
                ValidationLimits::default()
            )
            .is_err()
        );
        assert!(
            validate_entries(
                [ArchiveEntry::file("A", 1), ArchiveEntry::file("a", 1)],
                ValidationLimits::default()
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_escaping_and_absolute_symlinks() {
        assert!(ok(ArchiveEntry::symlink("dir/link", "../../outside")).is_err());
        assert!(ok(ArchiveEntry::symlink("link", "/outside")).is_err());
        assert!(ok(ArchiveEntry::symlink("dir/link", "../sibling")).is_ok());
    }
    #[test]
    fn rejects_special_files_and_resource_limits() {
        let mut special = ArchiveEntry::file("device", 0);
        special.kind = EntryKind::Other;
        assert!(ok(special).is_err());
        assert!(
            validate_entries(
                [ArchiveEntry::file("x", 11)],
                ValidationLimits {
                    max_entry_size: 10,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            validate_entries(
                [ArchiveEntry::file("a", 6), ArchiveEntry::file("b", 6)],
                ValidationLimits {
                    max_total_size: 10,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn extracts_tar_and_cleans_failed_adversarial_archive() {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("package/index.js").unwrap();
            header.set_size(2);
            header.set_cksum();
            builder.append(&header, &b"ok"[..]).unwrap();
            builder.finish().unwrap();
        }
        let root = std::env::temp_dir().join(format!("tapid-archive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let dest = root.join("tree");
        extract_to(&bytes, ArchiveFormat::Tar, &dest, ArchiveLimits::default()).unwrap();
        assert_eq!(fs::read(dest.join("package/index.js")).unwrap(), b"ok");
        assert!(canonical_tree_digest(&dest).unwrap().starts_with("sha256-"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn extraction_rejects_traversal_before_writing() {
        let bytes = vec![0u8; 32];
        let root = std::env::temp_dir().join(format!("tapid-archive-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dest = root.join("tree");
        let limits = ArchiveLimits {
            max_archive_bytes: 1,
            ..Default::default()
        };
        assert!(extract_to(&bytes, ArchiveFormat::Tar, &dest, limits).is_err());
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(root);
    }
}
