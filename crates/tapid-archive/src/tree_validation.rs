use super::{
    ArchiveEntry, EntryKind, ExtractError, ValidationError, ValidationLimits, normalized_path,
    validate_entries,
};
use std::{fs, path::Path};

/// Apply portable archive bounds to a generated filesystem tree before hashing
/// or publication. Generated trees cannot retain archive symlinks or special files.
pub fn validate_tree(root: &Path, limits: ValidationLimits) -> Result<(), ExtractError> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ExtractError::InvalidArchive(
            "generated tree root must be a directory".into(),
        ));
    }
    fn collect(
        root: &Path,
        path: &Path,
        limits: ValidationLimits,
        entries: &mut Vec<ArchiveEntry>,
    ) -> Result<(), ExtractError> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let child = entry.path();
            if entries.len() >= limits.max_entries {
                return Err(ExtractError::Invalid(ValidationError::TooManyEntries));
            }
            let relative = child.strip_prefix(root).map_err(|_| {
                ExtractError::InvalidArchive("generated path escaped its tree".into())
            })?;
            let name = relative
                .to_str()
                .ok_or_else(|| ExtractError::InvalidArchive("generated path is not UTF-8".into()))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            if name.len() > limits.max_path_bytes {
                return Err(ExtractError::Invalid(ValidationError::PathTooLong));
            }
            if normalized_path(&name).map_err(ExtractError::Invalid)? != name {
                return Err(ExtractError::Invalid(ValidationError::InvalidPath(name)));
            }
            if name == ".tapid-tree" || name == ".tapid-executable-modes" {
                return Err(ExtractError::InvalidArchive(
                    "generated tree contains reserved store metadata".into(),
                ));
            }
            let metadata = fs::symlink_metadata(&child)?;
            let kind = if metadata.is_file() {
                EntryKind::File
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else {
                return Err(ExtractError::Invalid(ValidationError::SpecialFile(name)));
            };
            let size = if metadata.is_file() {
                metadata.len()
            } else {
                0
            };
            entries.push(ArchiveEntry {
                path: name,
                kind,
                size,
                link_target: None,
            });
            if metadata.is_dir() {
                collect(root, &child, limits, entries)?;
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    collect(root, root, limits, &mut entries)?;
    validate_entries(entries, limits).map_err(ExtractError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_trees_obey_size_bounds_without_reading_sparse_contents() {
        let project = tapid_test_support::TempProject::new("generated-tree-bounds").unwrap();
        project.write("output", b"ok").unwrap();
        validate_tree(project.path(), ValidationLimits::default()).unwrap();
        fs::File::options()
            .write(true)
            .open(project.path().join("output"))
            .unwrap()
            .set_len(513 * 1024 * 1024)
            .unwrap();
        assert!(matches!(
            validate_tree(project.path(), ValidationLimits::default()),
            Err(ExtractError::Invalid(ValidationError::EntryTooLarge { .. }))
        ));
    }
    #[cfg(unix)]
    #[test]
    fn generated_trees_reject_symlinks_and_reserved_metadata() {
        let project = tapid_test_support::TempProject::new("generated-tree-special").unwrap();
        project.write("source", b"ok").unwrap();
        std::os::unix::fs::symlink("source", project.path().join("link")).unwrap();
        assert!(validate_tree(project.path(), ValidationLimits::default()).is_err());
        fs::remove_file(project.path().join("link")).unwrap();
        project
            .write(".tapid-executable-modes", b"forged metadata")
            .unwrap();
        assert!(validate_tree(project.path(), ValidationLimits::default()).is_err());
    }
}
