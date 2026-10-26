//! Bounded validation for hostile package archive manifests.

#![deny(unsafe_code)]

mod tree_validation;
pub use tree_validation::validate_tree;
mod release;
pub use release::inspect_release_archive;

use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read};
use std::path::{Component, Path};

const EXECUTABLE_MANIFEST: &str = ".tapid-executable-modes";
// Includes headers, padding and parser-consumed GNU/PAX extensions between
// logical entries. Payload bytes receive a budget only after validation.
const PARSER_METADATA_LIMIT: u64 = 64 * 1024;

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
///
/// On validation or extraction failure, attempt to remove the newly created
/// staging directory. Cleanup can fail; return the original processing error
/// even if staging removal fails.
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
    validate_initial_metadata(bytes, format, limits.validation)?;
    validate_archive(bytes, format, limits.validation)?;
    let executable_paths = extract_entries(bytes, format, destination)?;
    write_executable_manifest(destination, &executable_paths)?;
    Ok(())
}

// Inspect only leading extension declarations, without traversing any package
// payload. The complete logical pass owns effective sizes, limits, and errors.
fn validate_initial_metadata(
    bytes: &[u8],
    format: ArchiveFormat,
    limits: ValidationLimits,
) -> Result<(), ExtractError> {
    let reader: Box<dyn Read + '_> = match format {
        ArchiveFormat::Tar => Box::new(Cursor::new(bytes)),
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes))),
    };
    let mut archive = tar::Archive::new(reader.take(PARSER_METADATA_LIMIT));
    for item in archive.entries()?.raw(true) {
        // Defer malformed/truncated metadata and an exhausted preflight budget
        // to the logical pass, preserving its validation order and diagnostics.
        let Ok(entry) = item else { return Ok(()) };
        let kind = entry.header().entry_type();
        let size = entry.size();
        if kind.is_gnu_longname()
            || kind.is_gnu_longlink()
            || kind.is_pax_local_extensions()
            || kind.is_pax_global_extensions()
        {
            let maximum = if kind.is_gnu_longname() || kind.is_gnu_longlink() {
                (limits.max_path_bytes as u64).saturating_add(1)
            } else if kind.is_pax_global_extensions() {
                1024
            } else {
                64 * 1024
            };
            if size > maximum {
                return Err(ExtractError::InvalidArchive(
                    "archive parser metadata limit exceeded".into(),
                ));
            }
        } else {
            return Ok(());
        }
    }
    Ok(())
}

/// Validate each logical header before advancing through its payload, using the
/// parser's effective size (including PAX overrides). Each advance receives only
/// the preceding validated payload size plus the bounded framing/metadata budget.
/// Extraction must wait until this complete validation pass succeeds.
fn validate_archive(
    bytes: &[u8],
    format: ArchiveFormat,
    limits: ValidationLimits,
) -> Result<(), ExtractError> {
    let reader: Box<dyn Read + '_> = match format {
        ArchiveFormat::Tar => Box::new(Cursor::new(bytes)),
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes))),
    };
    let remaining = Cell::new(PARSER_METADATA_LIMIT);
    let mut archive = tar::Archive::new(ParserBudgetReader {
        reader,
        remaining: &remaining,
    });
    let mut validator = EntryValidator::new(limits);
    let mut global_comment_seen = false;
    let mut entries = archive.entries().map_err(ExtractError::Io)?;
    let mut previous_payload = 0u64;
    loop {
        // Advancing can skip the preceding entry's payload before parsing the
        // next header. Only that already-validated payload gets extra budget.
        remaining.set(previous_payload.saturating_add(PARSER_METADATA_LIMIT));
        let Some(item) = entries.next() else { break };
        let mut e = item.map_err(ExtractError::Io)?;
        previous_payload = 0;
        if git_commit_comment(&mut e)? {
            if global_comment_seen {
                return Err(ExtractError::InvalidArchive(
                    "duplicate global commit comment".into(),
                ));
            }
            global_comment_seen = true;
            continue;
        }
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
        let size = e.size();
        let target = if matches!(kind, EntryKind::Symlink) {
            e.link_name()
                .map_err(|x| ExtractError::InvalidArchive(x.to_string()))?
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        validator
            .push(ArchiveEntry {
                path: path.clone(),
                kind,
                size,
                link_target: target,
            })
            .map_err(ExtractError::Invalid)?;
        if kind == EntryKind::Symlink {
            return Err(ExtractError::Invalid(ValidationError::SpecialFile(path)));
        }
        previous_payload = size;
    }
    Ok(())
}

struct ParserBudgetReader<'a, R> {
    reader: R,
    remaining: &'a Cell<u64>,
}

impl<R: Read> Read for ParserBudgetReader<'_, R> {
    /// Charge decompressed bytes against the current parser budget. Nonempty
    /// reads fail when it is exhausted; only the validation loop replenishes it
    /// after accepting a header, so hidden extensions cannot grant payload budget.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self.remaining.get();
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive parser metadata limit exceeded",
            ));
        }
        let limit = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = self.reader.read(&mut buffer[..limit])?;
        self.remaining.set(remaining - read as u64);
        Ok(read)
    }
}

/// Git's commit archive has one inert global PAX comment. Accept only that
/// bounded shape, never global path, link, size, or permission overrides.
fn git_commit_comment(entry: &mut tar::Entry<'_, impl Read>) -> Result<bool, ExtractError> {
    if !entry.header().entry_type().is_pax_global_extensions() {
        return Ok(false);
    }
    if entry.header().size()? > 1024 {
        return Err(ExtractError::InvalidArchive(
            "global PAX metadata is too large".into(),
        ));
    }
    let mut extensions = entry
        .pax_extensions()?
        .ok_or_else(|| ExtractError::InvalidArchive("missing global PAX metadata".into()))?;
    let comment = extensions
        .next()
        .ok_or_else(|| ExtractError::InvalidArchive("empty global PAX metadata".into()))??;
    if comment.key_bytes() != b"comment"
        || !matches!(comment.value_bytes().len(), 40 | 64)
        || !comment
            .value_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        || extensions.next().is_some()
    {
        return Err(ExtractError::InvalidArchive(
            "unsupported global PAX metadata".into(),
        ));
    }
    Ok(true)
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
        if git_commit_comment(&mut entry)? {
            continue;
        }
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
    tree_digest(root, None)
}

/// Hash a tree view that omits exactly one named entry directly below its root.
/// The caller must omit that same entry from the copied/consumed view. Normal
/// archive and store verification must continue using canonical_tree_digest.
pub fn canonical_tree_digest_without_root_entry(root: &Path, entry: &str) -> io::Result<String> {
    if entry.is_empty()
        || matches!(entry, "." | "..")
        || entry.bytes().any(|b| matches!(b, 0 | b'/' | b'\\' | b':'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "excluded tree entry must be one filename",
        ));
    }
    tree_digest(root, Some(entry))
}

/// Incrementally hashes exactly the recorded length using a fixed 65,536-byte buffer.
///
/// Rejects premature EOF and probes one additional byte to reject growth. Reader
/// errors propagate unchanged; on any failure, the supplied hasher may already
/// contain a prefix of the file's bytes.
fn hash_file_contents<R: Read>(reader: &mut R, length: u64, hasher: &mut Sha256) -> io::Result<()> {
    let mut remaining = length;
    let mut buffer = [0u8; 65_536];
    while remaining != 0 {
        let chunk_size = remaining.min(65_536) as usize;
        let count = reader.read(&mut buffer[..chunk_size])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tree file changed during hashing",
            ));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if reader.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tree file grew during hashing",
        ));
    }
    Ok(())
}

fn tree_digest(root: &Path, excluded: Option<&str>) -> io::Result<String> {
    let mut executable_paths = read_executable_manifest(root)?;
    if let (Some(paths), Some(entry)) = (&mut executable_paths, excluded) {
        paths.retain(|path| path != entry && !path.starts_with(&format!("{entry}/")));
    }
    let mut files = Vec::new();
    collect_tree(root, root, executable_paths.as_ref(), excluded, &mut files)?;
    if let Some(paths) = &executable_paths {
        let mut manifest = paths.iter().cloned().collect::<Vec<_>>().join("\n");
        if !manifest.is_empty() {
            manifest.push('\n');
        }
        files.push((
            EXECUTABLE_MANIFEST.to_owned(),
            3,
            TreePayload::Bytes(manifest.into_bytes()),
        ));
    }
    files.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let mut h = Sha256::new();
    for (path, kind, data) in files {
        h.update((path.len() as u64).to_be_bytes());
        h.update(path.as_bytes());
        h.update([kind]);
        match data {
            TreePayload::Bytes(bytes) => {
                h.update((bytes.len() as u64).to_be_bytes());
                h.update(bytes);
            }
            TreePayload::File(path, length) => {
                h.update(length.to_be_bytes());
                let mut file = fs::File::open(path)?;
                hash_file_contents(&mut file, length, &mut h)?;
            }
        }
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

enum TreePayload {
    Bytes(Vec<u8>),
    File(std::path::PathBuf, u64),
}

fn collect_tree(
    root: &Path,
    current: &Path,
    executable_paths: Option<&BTreeSet<String>>,
    excluded: Option<&str>,
    out: &mut Vec<(String, u8, TreePayload)>,
) -> io::Result<()> {
    for item in fs::read_dir(current)? {
        let item = item?;
        let p = item.path();
        let rel = p
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == ".tapid-tree" || rel == EXECUTABLE_MANIFEST || excluded == Some(rel.as_str()) {
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
            out.push((rel.clone(), 1, TreePayload::Bytes(Vec::new())));
            collect_tree(root, &p, executable_paths, excluded, out)?;
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
            out.push((rel, kind, TreePayload::File(p, m.len())));
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
    let mut validator = EntryValidator::new(limits);
    for entry in entries {
        validator.push(entry)?;
    }
    Ok(())
}

struct EntryValidator {
    limits: ValidationLimits,
    count: usize,
    total: u64,
    exact: HashSet<String>,
    folded: HashSet<String>,
}

impl EntryValidator {
    /// Start an archive with no accepted paths, entries, or file payload bytes.
    fn new(limits: ValidationLimits) -> Self {
        Self {
            limits,
            count: 0,
            total: 0,
            exact: HashSet::new(),
            folded: HashSet::new(),
        }
    }

    /// Check portable paths, collisions, entry kinds, and cumulative limits using
    /// metadata alone. Callers must supply the effective payload size. An error
    /// may leave counters or path sets advanced; discard the validator on failure.
    /// Safe symlink targets pass this metadata check; extraction rejects all links.
    fn push(&mut self, entry: ArchiveEntry) -> Result<(), ValidationError> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or(ValidationError::TooManyEntries)?;
        if self.count > self.limits.max_entries {
            return Err(ValidationError::TooManyEntries);
        }
        if entry.path.len() > self.limits.max_path_bytes {
            return Err(ValidationError::PathTooLong);
        }
        let normalized = normalized_path(&entry.path)?;
        if !self.exact.insert(normalized.clone()) {
            return Err(ValidationError::DuplicatePath(entry.path));
        }
        if !self.folded.insert(normalized.to_ascii_lowercase()) {
            return Err(ValidationError::CaseCollision(entry.path));
        }
        match entry.kind {
            EntryKind::File => {
                if entry.size > self.limits.max_entry_size {
                    return Err(ValidationError::EntryTooLarge {
                        path: entry.path,
                        size: entry.size,
                    });
                }
                self.total = self
                    .total
                    .checked_add(entry.size)
                    .ok_or(ValidationError::TotalSizeExceeded)?;
                if self.total > self.limits.max_total_size {
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
                if symlink_target_escapes(&normalized, target) {
                    return Err(ValidationError::SymlinkEscape(entry.path));
                }
            }
            EntryKind::Hardlink | EntryKind::Other => {
                return Err(ValidationError::SpecialFile(entry.path));
            }
        }
        Ok(())
    }
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
    /// Hidden GNU/PAX metadata must exhaust the same bound for tar and tar.gz,
    /// and failed validation cleans staging under normal filesystem conditions.
    #[test]
    fn extraction_bounds_hidden_tar_metadata_before_parsing_the_next_file() {
        let project = tapid_test_support::TempProject::new("gnu-metadata-limit").unwrap();
        let destination = project.path().join("tree");
        for (kind, after_file) in b"LKx"
            .iter()
            .flat_map(|&kind| [(kind, false), (kind, true)])
        {
            let mut header = tar::Header::new_gnu();
            header.set_path("././@LongLink").unwrap();
            header.set_entry_type(tar::EntryType::new(kind));
            header.set_size(128 * 1024);
            header.set_cksum();
            let mut bytes = Vec::new();
            if after_file {
                let mut file = tar::Header::new_ustar();
                file.set_path("package/first").unwrap();
                file.set_size(0);
                file.set_cksum();
                bytes.extend_from_slice(file.as_bytes());
            }
            bytes.extend_from_slice(header.as_bytes());
            bytes.resize(bytes.len() + 128 * 1024, b'x');
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut encoder, &bytes).unwrap();
            let compressed = encoder.finish().unwrap();
            for (input, format) in [
                (bytes.as_slice(), ArchiveFormat::Tar),
                (compressed.as_slice(), ArchiveFormat::TarGz),
            ] {
                let error =
                    extract_to(input, format, &destination, ArchiveLimits::default()).unwrap_err();
                assert!(
                    error.to_string().contains("parser metadata limit"),
                    "{error}"
                );
                assert!(!destination.exists());
            }
        }
    }

    /// Validated payloads may exceed the metadata budget without rejecting
    /// legitimate files or bounded GNU long-name extensions.
    #[test]
    fn extraction_preserves_large_payloads_and_bounded_long_names() {
        let project = tapid_test_support::TempProject::new("large-valid-archive").unwrap();
        let destination = project.path().join("tree");
        let body = vec![b'x'; 128 * 1024];
        let long_path = format!("package/{}", "a".repeat(160));
        let mut builder = tar::Builder::new(Vec::new());
        for path in ["package/first", long_path.as_str()] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, path, body.as_slice())
                .unwrap();
        }
        let bytes = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        for (input, format) in [
            (bytes.as_slice(), ArchiveFormat::Tar),
            (compressed.as_slice(), ArchiveFormat::TarGz),
        ] {
            extract_to(input, format, &destination, ArchiveLimits::default()).unwrap();
            assert_eq!(fs::read(destination.join("package/first")).unwrap(), body);
            assert_eq!(fs::read(destination.join(&long_path)).unwrap(), body);
            fs::remove_dir_all(&destination).unwrap();
        }
    }

    /// Header violations take precedence over malformed bytes later in the tar,
    /// proving rejection happens before the parser traverses their payloads.
    #[test]
    fn extraction_rejects_header_limits_before_later_invalid_bytes() {
        for (label, headers, validation, expected) in [
            (
                "count",
                vec![("first", 0), ("second", 0)],
                ValidationLimits {
                    max_entries: 1,
                    ..Default::default()
                },
                ValidationError::TooManyEntries,
            ),
            (
                "total",
                vec![("first", 6), ("second", 6)],
                ValidationLimits {
                    max_total_size: 10,
                    ..Default::default()
                },
                ValidationError::TotalSizeExceeded,
            ),
            (
                "path",
                vec![("long-path", 0)],
                ValidationLimits {
                    max_path_bytes: 4,
                    ..Default::default()
                },
                ValidationError::PathTooLong,
            ),
        ] {
            let project = tapid_test_support::TempProject::new(label).unwrap();
            let destination = project.path().join("tree");
            let mut bytes = Vec::new();
            for (path, size) in headers {
                let mut header = tar::Header::new_ustar();
                header.set_path(path).unwrap();
                header.set_size(size);
                header.set_mode(0o644);
                header.set_cksum();
                bytes.extend_from_slice(header.as_bytes());
                // Supply data for preceding entries. The invalid checksum after
                // the violating header detects traversal beyond that header.
                if size > 0 {
                    bytes.resize(bytes.len() + 512, 0);
                }
            }
            bytes.extend_from_slice(&[b'!'; 512]);
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut encoder, &bytes).unwrap();
            let compressed = encoder.finish().unwrap();
            for (input, format) in [
                (bytes.as_slice(), ArchiveFormat::Tar),
                (compressed.as_slice(), ArchiveFormat::TarGz),
            ] {
                let error = extract_to(
                    input,
                    format,
                    &destination,
                    ArchiveLimits {
                        validation,
                        ..Default::default()
                    },
                )
                .unwrap_err();
                assert!(
                    matches!(error, ExtractError::Invalid(ref actual) if actual == &expected),
                    "{label}: {error}"
                );
                assert!(!destination.exists());
            }
        }
    }

    /// PAX size overrides, rather than raw header sizes, govern early rejection.
    #[test]
    fn extraction_limits_use_pax_effective_size_before_reading_payload() {
        let project = tapid_test_support::TempProject::new("pax-size-limit").unwrap();
        let destination = project.path().join("tree");
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_pax_extensions([("size", b"11".as_slice())])
            .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_path("package/large").unwrap();
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, io::empty()).unwrap();
        let bytes = builder.into_inner().unwrap();
        let error = extract_to(
            &bytes,
            ArchiveFormat::Tar,
            &destination,
            ArchiveLimits {
                validation: ValidationLimits {
                    max_entry_size: 10,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                ExtractError::Invalid(ValidationError::EntryTooLarge { size: 11, .. })
            ),
            "{error}"
        );
        assert!(!destination.exists());
    }

    /// An oversized declaration fails with a size diagnostic even when its
    /// payload is absent, and failure cleans staging under normal filesystem conditions.
    #[test]
    fn extraction_rejects_oversized_entry_before_reading_its_missing_payload() {
        let project = tapid_test_support::TempProject::new("early-entry-limit").unwrap();
        let destination = project.path().join("tree");
        let mut header = tar::Header::new_ustar();
        header.set_path("package/large").unwrap();
        header.set_mode(0o644);
        header.set_size(11);
        header.set_cksum();
        let error = extract_to(
            header.as_bytes(),
            ArchiveFormat::Tar,
            &destination,
            ArchiveLimits {
                validation: ValidationLimits {
                    max_entry_size: 10,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                ExtractError::Invalid(ValidationError::EntryTooLarge { size: 11, .. })
            ),
            "{error}"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn entry_count_is_checked_before_reading_more_archive_bodies() {
        let project = tapid_test_support::TempProject::new("archive-entry-limit").unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        for name in ["a", "b"] {
            let mut header = tar::Header::new_ustar();
            header.set_mode(0o644);
            header.set_size(0);
            builder.append_data(&mut header, name, io::empty()).unwrap();
        }
        let mut bytes = builder.into_inner().unwrap();
        bytes.truncate(1024);
        bytes.extend_from_slice(&[b'!'; 512]);
        let mut limits = ArchiveLimits::default();
        limits.validation.max_entries = 1;
        let destination = project.path().join("tree");
        assert!(matches!(
            extract_to(&bytes, ArchiveFormat::Tar, &destination, limits),
            Err(ExtractError::Invalid(ValidationError::TooManyEntries))
        ));
        assert!(!destination.exists());
    }

    #[test]
    fn bounded_pax_paths_and_gnu_longnames_still_extract() {
        for pax in [false, true] {
            let project = tapid_test_support::TempProject::new("bounded-archive-path").unwrap();
            let path = format!("package/{}/file", "a".repeat(120));
            let mut builder = tar::Builder::new(Vec::new());
            if pax {
                builder
                    .append_pax_extensions([("path", path.as_bytes())])
                    .unwrap();
            }
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(2);
            builder
                .append_data(
                    &mut header,
                    if pax { "placeholder" } else { &path },
                    b"ok".as_slice(),
                )
                .unwrap();
            let bytes = builder.into_inner().unwrap();
            let destination = project.path().join("tree");
            extract_to(
                &bytes,
                ArchiveFormat::Tar,
                &destination,
                ArchiveLimits::default(),
            )
            .unwrap();
            assert_eq!(fs::read(destination.join(path)).unwrap(), b"ok");
        }
    }

    #[test]
    fn oversized_extension_is_rejected_before_its_body_is_read() {
        let project = tapid_test_support::TempProject::new("extension-limit").unwrap();
        for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
            let mut header = tar::Header::new_gnu();
            header.set_path("metadata").unwrap();
            header.set_entry_type(kind);
            header.set_size(32 * 1024 * 1024);
            header.set_cksum();
            let destination = project.path().join("tree");
            // The missing body makes this distinguish an early bound from a
            // parser that attempts to materialize all of the extension first.
            assert!(matches!(extract_to(header.as_bytes(), ArchiveFormat::Tar,
                &destination, ArchiveLimits::default()),
                Err(ExtractError::InvalidArchive(message)) if message.contains("metadata")));
            assert!(!destination.exists());
        }
    }

    #[test]
    fn pax_size_cannot_bypass_extraction_limits() {
        use std::io::Write;
        let project = tapid_test_support::TempProject::new("pax-size-limit").unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_pax_extensions([("size", b"4096".as_slice())])
            .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_path("package/file").unwrap();
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        let mut bytes = builder.into_inner().unwrap();
        // Replace the builder's end markers with a raw zero-sized header whose
        // effective PAX body is larger than both configured extraction limits.
        bytes.truncate(1024);
        bytes.extend_from_slice(header.as_bytes());
        bytes.write_all(&[b'x'; 4096]).unwrap();
        bytes.extend_from_slice(&[0; 1024]);
        let mut limits = ArchiveLimits::default();
        limits.validation.max_entry_size = 1024;
        limits.validation.max_total_size = 1024;
        let destination = project.path().join("tree");
        let error = extract_to(&bytes, ArchiveFormat::Tar, &destination, limits).unwrap_err();
        assert!(
            matches!(
                error,
                ExtractError::Invalid(ValidationError::EntryTooLarge { size: 4096, .. })
            ),
            "{error}"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn bounded_pax_size_overrides_preserve_effective_payload_boundaries() {
        let project = tapid_test_support::TempProject::new("pax-payload-boundaries").unwrap();
        let body = vec![b'x'; 128 * 1024];
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_pax_extensions([("size", body.len().to_string().as_bytes())])
            .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_path("package/file").unwrap();
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        let mut bytes = builder.into_inner().unwrap();
        bytes.truncate(1024);
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&[0; 1024]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), Default::default());
        std::io::Write::write_all(&mut encoder, &bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        for (input, format) in [
            (bytes.as_slice(), ArchiveFormat::Tar),
            (compressed.as_slice(), ArchiveFormat::TarGz),
        ] {
            let destination = project.path().join("tree");
            extract_to(input, format, &destination, ArchiveLimits::default()).unwrap();
            assert_eq!(fs::read(destination.join("package/file")).unwrap(), body);
            fs::remove_dir_all(destination).unwrap();
        }
    }

    #[test]
    fn global_pax_accepts_only_one_bounded_commit_comment() {
        let commit = "a".repeat(40);
        for body in [
            format!("52 comment={commit}\n"),
            "20 path=../escape\n".into(),
            "not valid pax".into(),
            "x".repeat(1025),
        ] {
            let project = tapid_test_support::TempProject::new("git-pax").unwrap();
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(tar::EntryType::new(b'g'));
            header.set_mode(0o644);
            header.set_size(body.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "pax_global_header", body.as_bytes())
                .unwrap();
            let mut file = tar::Header::new_ustar();
            file.set_mode(0o644);
            file.set_size(2);
            file.set_cksum();
            builder
                .append_data(&mut file, "package/file", b"ok".as_slice())
                .unwrap();
            let bytes = builder.into_inner().unwrap();
            let destination = project.path().join("tree");
            let result = extract_to(
                &bytes,
                ArchiveFormat::Tar,
                &destination,
                ArchiveLimits::default(),
            );
            if body.starts_with("52 comment=") {
                result.unwrap();
                assert_eq!(fs::read(destination.join("package/file")).unwrap(), b"ok");
                assert!(!destination.join("pax_global_header").exists());
            } else {
                assert!(result.is_err(), "{body}");
                assert!(!destination.exists());
            }
        }
    }

    fn ok(entry: ArchiveEntry) -> Result<(), ValidationError> {
        validate_entries([entry], ValidationLimits::default())
    }
    /// Exact and empty inputs retain independently known SHA-256 digests.
    #[test]
    fn digest_reader_hashes_exact_and_zero_lengths() {
        for (bytes, expected) in [
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
        ] {
            let mut reader = bytes;
            let mut hasher = Sha256::new();
            hash_file_contents(&mut reader, bytes.len() as u64, &mut hasher).unwrap();
            assert_eq!(hex::encode(hasher.finalize()), expected);
        }
    }

    /// Successful short reads contribute every byte to the same known digest.
    #[test]
    fn digest_reader_hashes_short_successful_reads() {
        /// Limits each read to one byte even when the caller requests more.
        struct ShortReader<'a>(&'a [u8]);
        impl Read for ShortReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = buffer.len().min(1);
                self.0.read(&mut buffer[..count])
            }
        }

        let mut reader = ShortReader(b"abc");
        let mut hasher = Sha256::new();
        hash_file_contents(&mut reader, 3, &mut hasher).unwrap();
        assert_eq!(
            hex::encode(hasher.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Truncation preserves the EOF error kind and existing diagnostic.
    #[test]
    fn digest_reader_rejects_premature_eof() {
        let error = hash_file_contents(&mut b"ab".as_slice(), 3, &mut Sha256::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "tree file changed during hashing");
    }

    /// The growth probe rejects extra bytes after zero or nonzero recorded lengths.
    #[test]
    fn digest_reader_rejects_trailing_data() {
        for length in [0, 2] {
            let error =
                hash_file_contents(&mut b"abc".as_slice(), length, &mut Sha256::new()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "tree file grew during hashing");
        }
    }

    /// Content reads and the growth probe preserve the reader's error and message.
    #[test]
    fn digest_reader_propagates_io_errors() {
        /// Injects a transport failure independently of the requested slice length.
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected read failure",
                ))
            }
        }

        // Exercise both the content read and the final growth probe.
        for length in [1, 0] {
            let error =
                hash_file_contents(&mut FailingReader, length, &mut Sha256::new()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "injected read failure");
        }
    }

    /// Consumes 3 MiB while bounding requested per-file read slices independently
    /// of total input length; this does not measure RSS or total caller memory.
    #[test]
    fn digest_reader_requests_at_most_one_fixed_chunk() {
        /// Synthesizes bytes without allocating the payload and records the largest
        /// requested slice, including the final growth probe.
        struct MeasuringReader {
            remaining: usize,
            max_request: usize,
        }
        impl std::io::Read for MeasuringReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.max_request = self.max_request.max(buffer.len());
                let count = self.remaining.min(buffer.len());
                buffer[..count].fill(b'x');
                self.remaining -= count;
                Ok(count)
            }
        }

        let mut reader = MeasuringReader {
            remaining: 3 * 1024 * 1024,
            max_request: 0,
        };
        let mut hasher = Sha256::new();
        hash_file_contents(&mut reader, 3 * 1024 * 1024, &mut hasher).unwrap();
        assert_eq!(reader.remaining, 0);
        assert!(reader.max_request <= 65_536);
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
        for (path, target) in [
            ("./link", "../outside"),
            ("dir/./link", "../../outside"),
            (r"dir\.\link", r"..\..\outside"),
        ] {
            assert_eq!(
                ok(ArchiveEntry::symlink(path, target)),
                Err(ValidationError::SymlinkEscape(path.to_owned())),
                "{path} -> {target}"
            );
        }
        assert!(ok(ArchiveEntry::symlink("./dir/./link", "../sibling")).is_ok());
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

    #[test]
    fn filtered_tree_digest_excludes_only_the_selected_root_entry() {
        let source = tapid_test_support::TempProject::new("filtered-source").unwrap();
        let view = tapid_test_support::TempProject::new("filtered-view").unwrap();
        source.write(".bin/shim", b"ignored").unwrap();
        for root in [&source, &view] {
            root.write("module/.bin/runtime", b"included").unwrap();
        }
        let digest = canonical_tree_digest_without_root_entry(source.path(), ".bin").unwrap();
        assert_eq!(digest, canonical_tree_digest(view.path()).unwrap());
        source.write("module/.bin/runtime", b"changed").unwrap();
        assert_ne!(
            digest,
            canonical_tree_digest_without_root_entry(source.path(), ".bin").unwrap()
        );
        for invalid in [
            "",
            ".",
            "..",
            "../escape",
            "module/.bin",
            "C:escape",
            "a\\b",
        ] {
            assert_eq!(
                canonical_tree_digest_without_root_entry(source.path(), invalid)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn filtered_tree_digest_keeps_all_other_link_rejections() {
        let source = tapid_test_support::TempProject::new("filtered-links").unwrap();
        source.write("module/index.js", b"runtime").unwrap();
        std::os::unix::fs::symlink("module", source.path().join(".bin")).unwrap();
        assert!(canonical_tree_digest(source.path()).is_err());
        assert!(canonical_tree_digest_without_root_entry(source.path(), ".bin").is_ok());
        std::os::unix::fs::symlink("module", source.path().join("escape")).unwrap();
        assert!(canonical_tree_digest_without_root_entry(source.path(), ".bin").is_err());
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
