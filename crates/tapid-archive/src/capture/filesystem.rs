use super::platform::{
    File, Metadata, executable, names, open_child, open_root, same_path, unchanged,
};
use super::*;
use std::io::Write;

#[cfg(test)]
thread_local! {
    static BEFORE_ENCODING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn before_encoding(action: impl FnOnce() + 'static) {
    BEFORE_ENCODING.with(|hook| *hook.borrow_mut() = Some(Box::new(action)));
}

fn invalid(message: &str) -> ExtractError {
    ExtractError::InvalidArchive(message.into())
}

fn portable(path: &str) -> Result<(), ExtractError> {
    if path.contains(['\\', ':', '|', '#', '<', '>', '"', '?', '*'])
        || path.chars().any(char::is_control)
        || normalized_path(path).map_err(ExtractError::Invalid)? != path
    {
        return Err(invalid(
            "directory capture requires canonical portable paths",
        ));
    }
    for part in path.split('/') {
        let base = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let numbered_device = base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9');
        if part.ends_with(['.', ' '])
            || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || numbered_device
        {
            return Err(invalid("directory source contains a nonportable name"));
        }
    }
    Ok(())
}

fn open_path(root: &File, path: &str) -> Result<File, ExtractError> {
    let mut current = root.try_clone()?;
    for part in path.split('/') {
        current = open_child(&current, part)?;
    }
    Ok(current)
}

struct Entry {
    path: String,
    metadata: Metadata,
}

struct BoundedArchive {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedArchive {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory archive exceeds byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Capture uses platform-held directories and no-follow opens.
/// Metadata comparisons detect observable changes, not an atomic filesystem view.
pub fn capture_directory(
    root: &Path,
    source: &str,
    forbidden: &[&str],
    limits: ArchiveLimits,
) -> Result<Vec<u8>, ExtractError> {
    portable(source)?;
    if source.len() > limits.validation.max_path_bytes {
        return Err(ExtractError::Invalid(ValidationError::PathTooLong));
    }
    for path in forbidden {
        portable(path)?;
        for end in source
            .match_indices('/')
            .map(|(index, _)| index)
            .chain(std::iter::once(source.len()))
        {
            if same_path(&source[..end], path)? {
                return Err(invalid("directory source is inside a managed tree"));
            }
        }
    }
    let root = open_root(root)?;
    let source_file = open_path(&root, source)?;
    let source_metadata = source_file.metadata()?;
    if !source_metadata.is_dir() {
        return Err(invalid("directory source must be a directory"));
    }
    let mut validator = EntryValidator::new(limits.validation);
    let mut entries = Vec::new();
    let mut pending = vec![String::new()];
    let mut visited = 0usize;
    while let Some(path) = pending.pop() {
        let directory = if path.is_empty() {
            source_file.try_clone()?
        } else {
            open_path(&source_file, &path)?
        };
        for child in names(&directory)? {
            let child = child?;
            if child == "." || child == ".." {
                continue;
            }
            visited = visited
                .checked_add(1)
                .ok_or(ExtractError::Invalid(ValidationError::TooManyEntries))?;
            if visited > limits.validation.max_entries {
                return Err(ExtractError::Invalid(ValidationError::TooManyEntries));
            }
            let name = child
                .to_str()
                .ok_or_else(|| invalid("directory entry is not UTF-8"))?;
            portable(name)?;
            let relative = if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}/{name}")
            };
            let child = open_child(&directory, name)?;
            let metadata = child.metadata()?;
            if metadata.is_dir() && (same_path(name, ".git")? || same_path(name, "node_modules")?) {
                continue;
            }
            if same_path(name, ".tapid-tree")? || same_path(name, ".tapid-executable-modes")? {
                return Err(invalid("directory source contains reserved store metadata"));
            }
            let project_path = format!("{source}/{relative}");
            for managed in forbidden {
                if same_path(&project_path, managed)? {
                    return Err(invalid("directory source contains a managed tree"));
                }
            }
            let kind = if metadata.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            validator
                .push(ArchiveEntry {
                    path: relative.clone(),
                    kind,
                    size: if metadata.is_file() {
                        metadata.len()
                    } else {
                        0
                    },
                    link_target: None,
                })
                .map_err(ExtractError::Invalid)?;
            if metadata.is_dir() {
                pending.push(relative.clone());
            }
            entries.push(Entry {
                path: relative,
                metadata,
            });
        }
    }
    if !entries.iter().any(|entry| {
        entry.path == "package.json"
            && entry.metadata.is_file()
            && entry.metadata.len() <= 1024 * 1024
    }) {
        return Err(invalid(
            "directory source requires a regular package.json no larger than 1 MiB",
        ));
    }
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    #[cfg(test)]
    BEFORE_ENCODING.with(|hook| {
        if let Some(action) = hook.borrow_mut().take() {
            action();
        }
    });
    let mut archive = tar::Builder::new(BoundedArchive {
        bytes: Vec::new(),
        limit: limits.max_archive_bytes,
    });
    for entry in &entries {
        let file = open_path(&source_file, &entry.path)?;
        unchanged(&entry.metadata, &file.metadata()?)?;
        let mut header = tar::Header::new_gnu();
        let is_directory = entry.metadata.is_dir();
        header.set_entry_type(if is_directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_size(if is_directory {
            0
        } else {
            entry.metadata.len()
        });
        header.set_mode(if is_directory || executable(&entry.metadata) {
            0o755
        } else {
            0o644
        });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        if is_directory {
            archive.append_data(&mut header, &entry.path, io::empty())?;
        } else {
            archive.append_data(&mut header, &entry.path, (&file).take(entry.metadata.len()))?;
        }
        unchanged(&entry.metadata, &file.metadata()?)?;
    }
    archive.finish()?;
    // Reopen from the held project root as well as checking the held objects.
    // Parent substitutions and observable mutations abort before bytes escape.
    unchanged(&source_metadata, &source_file.metadata()?)?;
    unchanged(&source_metadata, &open_path(&root, source)?.metadata()?)?;
    for entry in &entries {
        unchanged(
            &entry.metadata,
            &open_path(&source_file, &entry.path)?.metadata()?,
        )?;
    }
    Ok(archive.into_inner()?.bytes)
}
