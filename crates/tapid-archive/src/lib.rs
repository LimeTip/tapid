//! Bounded validation for hostile package archive manifests.

#![deny(unsafe_code)]

use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path};

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
}
