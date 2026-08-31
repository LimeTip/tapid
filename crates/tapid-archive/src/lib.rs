//! Bounded validation for hostile package archive manifests.

#![deny(unsafe_code)]

use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read};
use std::path::{Component, Path};

const EXECUTABLE_MANIFEST: &str = ".tapid-executable-modes";

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
    let executable_paths = extract_entries(bytes, format, destination)?;
    write_executable_manifest(destination, &executable_paths)?;
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
) -> Result<Vec<String>, ExtractError> {
    let reader: Box<dyn Read + '_> = match format {
        ArchiveFormat::Tar => Box::new(Cursor::new(bytes)),
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes))),
    };
    let mut archive = tar::Archive::new(reader);
    let mut executable_paths = Vec::new();
    for item in archive.entries().map_err(ExtractError::Io)? {
        let mut entry = item.map_err(ExtractError::Io)?;
        let raw = entry
            .path()
            .map_err(|x| ExtractError::InvalidArchive(x.to_string()))?
            .to_string_lossy()
            .into_owned();
        let normalized = normalized_path(&raw).map_err(ExtractError::Invalid)?;
        if normalized == EXECUTABLE_MANIFEST {
            return Err(ExtractError::InvalidArchive(
                "archive uses a reserved Tapid metadata path".into(),
            ));
        }
        let archive_mode = entry.header().mode()?;
        if archive_mode & 0o111 != 0 && normalized.bytes().any(|byte| matches!(byte, b'\n' | b'\r'))
        {
            return Err(ExtractError::InvalidArchive(
                "executable entry path contains a line break".into(),
            ));
        }
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
        apply_portable_file_mode(archive_mode, &target)?;
        if archive_mode & 0o111 != 0 {
            executable_paths.push(normalized);
        }
        out.sync_all()?;
    }
    executable_paths.sort();
    Ok(executable_paths)
}

fn write_executable_manifest(destination: &Path, executable_paths: &[String]) -> io::Result<()> {
    let mut contents = executable_paths.join("\n");
    if !contents.is_empty() {
        contents.push('\n');
    }
    let mut manifest = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination.join(EXECUTABLE_MANIFEST))?;
    std::io::Write::write_all(&mut manifest, contents.as_bytes())?;
    manifest.sync_all()
}

#[cfg(unix)]
fn apply_portable_file_mode(archive_mode: u32, target: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Preserve only the portable executable distinction. Fixed modes avoid
    // restoring archive-controlled ownership-style or privilege permissions.
    let mode = if archive_mode & 0o111 == 0 {
        0o644
    } else {
        0o755
    };
    fs::set_permissions(target, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn apply_portable_file_mode(_archive_mode: u32, _target: &Path) -> io::Result<()> {
    Ok(())
}

/// Hash a tree using sorted relative paths and explicit type/length framing.
///
/// On Unix, executable and non-executable regular files are distinct while all
/// other permission bits are ignored. Platforms without Unix executable bits
/// retain the non-executable regular-file framing.
pub fn canonical_tree_digest(root: &Path) -> io::Result<String> {
    let executable_paths = read_executable_manifest(root)?;
    let mut files = Vec::new();
    collect_tree(root, root, executable_paths.as_ref(), &mut files)?;
    if let Some(paths) = &executable_paths {
        let mut manifest = paths.iter().cloned().collect::<Vec<_>>().join("\n");
        if !manifest.is_empty() {
            manifest.push('\n');
        }
        files.push((EXECUTABLE_MANIFEST.to_owned(), 3, manifest.into_bytes()));
    }
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

fn read_executable_manifest(root: &Path) -> io::Result<Option<BTreeSet<String>>> {
    let path = root.join(EXECUTABLE_MANIFEST);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "executable mode manifest is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.len() > 4 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executable mode manifest is oversized",
        ));
    }
    let contents = fs::read_to_string(&path)?;
    if !contents.is_empty() && !contents.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executable mode manifest is not newline terminated",
        ));
    }
    let mut paths = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for value in contents.lines() {
        let normalized = normalized_path(value)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid executable path"))?;
        if normalized != value || previous.is_some_and(|previous| previous >= value) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "executable mode manifest is not canonical",
            ));
        }
        let target = root.join(value.split('/').collect::<std::path::PathBuf>());
        let target_metadata = fs::symlink_metadata(target)?;
        if !target_metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "executable mode manifest references a non-regular file",
            ));
        }
        paths.insert(value.to_owned());
        previous = Some(value);
    }
    Ok(Some(paths))
}

fn collect_tree(
    root: &Path,
    current: &Path,
    executable_paths: Option<&BTreeSet<String>>,
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
        if rel == ".tapid-tree" || rel == EXECUTABLE_MANIFEST {
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
            collect_tree(root, &p, executable_paths, out)?;
        } else if m.is_file() {
            let kind = executable_paths.map_or_else(
                || portable_file_kind(&m),
                |paths| u8::from(paths.contains(&rel)) * 2,
            );
            #[cfg(unix)]
            if executable_paths.is_some() && kind != portable_file_kind(&m) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "file executable mode differs from the canonical manifest",
                ));
            }
            out.push((rel, kind, fs::read(&p)?));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "special file in extracted tree",
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn portable_file_kind(metadata: &fs::Metadata) -> u8 {
    use std::os::unix::fs::PermissionsExt;

    u8::from(metadata.permissions().mode() & 0o111 != 0) * 2
}

#[cfg(not(unix))]
fn portable_file_kind(_metadata: &fs::Metadata) -> u8 {
    0
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
            header.set_mode(0o755);
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dest.join("package/index.js"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
        }
        assert_eq!(
            fs::read_to_string(dest.join(EXECUTABLE_MANIFEST)).unwrap(),
            "package/index.js\n"
        );
        assert_eq!(
            canonical_tree_digest(&dest).unwrap(),
            "sha256-011c8f7a278748278e741f3bb6d54af8cdaa14ee84b263bcd68f555fc41e2393"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn extraction_rejects_executable_paths_with_line_breaks() {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("package/tool\nname").unwrap();
            header.set_size(4);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, &b"tool"[..]).unwrap();
            builder.finish().unwrap();
        }
        let root = std::env::temp_dir().join(format!(
            "tapid-archive-line-break-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let destination = root.join("tree");
        fs::create_dir(&root).unwrap();

        let error = extract_to(
            &bytes,
            ArchiveFormat::Tar,
            &destination,
            ArchiveLimits::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("line break"), "{error}");
        assert!(!destination.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn extraction_strips_privilege_bits_and_normalizes_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("package/tool").unwrap();
            header.set_size(4);
            header.set_mode(0o7700);
            header.set_cksum();
            builder.append(&header, &b"tool"[..]).unwrap();
            builder.finish().unwrap();
        }
        let root = std::env::temp_dir().join(format!(
            "tapid-archive-privilege-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let dest = root.join("tree");

        extract_to(&bytes, ArchiveFormat::Tar, &dest, ArchiveLimits::default()).unwrap();

        let mode = fs::metadata(dest.join("package/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o755);
        fs::remove_dir_all(root).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn canonical_tree_digest_binds_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "tapid-archive-mode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let executable = root.join("tool");
        fs::write(&executable, b"tool").unwrap();
        let plain = canonical_tree_digest(&root).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let executable_digest = canonical_tree_digest(&root).unwrap();

        assert_ne!(plain, executable_digest);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonical_tree_digest_ignores_non_executable_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "tapid-archive-portable-mode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let file = root.join("tool");
        fs::write(&file, b"tool").unwrap();

        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let private_plain = canonical_tree_digest(&root).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o664)).unwrap();
        let shared_plain = canonical_tree_digest(&root).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        let private_executable = canonical_tree_digest(&root).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let shared_executable = canonical_tree_digest(&root).unwrap();

        assert_eq!(private_plain, shared_plain);
        assert_eq!(private_executable, shared_executable);
        assert_ne!(private_plain, private_executable);
        fs::remove_dir_all(root).unwrap();
    }
}
