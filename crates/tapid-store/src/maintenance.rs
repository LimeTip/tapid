//! Conservative eviction of published data. Staging and recovery belong to
//! their existing owners and are never swept by cache maintenance.
use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheUsage {
    pub entries: u64,
    /// Logical file bytes, not filesystem allocated space. Links are not followed.
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheSummary {
    pub artifacts: CacheUsage,
    pub trees: CacheUsage,
    /// Unrecognized entries within artifacts/ and trees/, preserved by cleaning.
    pub preserved_entries: u64,
}

impl Store {
    /// Inspect published cache data without creating files or recovering state.
    pub fn cache_info(&self) -> Result<CacheSummary, IngestError> {
        self.maintain_cache(false)
    }

    /// Remove recognized published artifacts and marked trees under an exclusive
    /// store lock. Never remove the root, lock, lifecycle key, staging, or journal.
    /// Errors can occur after some entries have been removed; retry is safe.
    pub fn clean_cache(&self) -> Result<CacheSummary, IngestError> {
        self.maintain_cache(true)
    }

    fn maintain_cache(&self, remove: bool) -> Result<CacheSummary, IngestError> {
        if self.root.as_os_str().is_empty() {
            return Err(IngestError::InvalidRoot);
        }
        if !directory_exists(&self.root)? {
            return Ok(CacheSummary::default());
        }
        // Do not create a lock during inspection. A populated legacy store with
        // no lock cannot be inspected or evicted with a concurrency guarantee.
        let file = match open_store_lock(&self.root, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !directory_exists(&self.root.join("artifacts"))?
                    && !directory_exists(&self.root.join("trees"))?
                    && fs::symlink_metadata(self.root.join(STORE_JOURNAL))
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                {
                    return Ok(CacheSummary::default());
                }
                return Err(invalid_cache(
                    "cache has no store lock; run an install before maintenance",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let locked = if remove {
            FileExt::try_lock(&file)
        } else {
            FileExt::try_lock_shared(&file)
        };
        locked.map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "cache is in use; retry after the active Tapid operation finishes",
            ),
            TryLockError::Error(error) => error,
        })?;
        match fs::symlink_metadata(self.root.join(STORE_JOURNAL)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(invalid_cache(
                    "cache has a pending transaction; run an install to recover it before maintenance",
                ));
            }
        }
        let mut summary = CacheSummary::default();
        let mut candidates = Vec::new();
        // Preflight both namespaces before deleting any bytes.
        for (namespace, usage) in [
            ("artifacts", &mut summary.artifacts),
            ("trees", &mut summary.trees),
        ] {
            let directory = self.root.join(namespace);
            if !directory_exists(&directory)? {
                continue;
            }
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)?;
                let digest = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<ArtifactDigest>().ok());
                let recognized = match (namespace, digest) {
                    ("artifacts", Some(_)) => metadata.file_type().is_file(),
                    ("trees", Some(digest)) if metadata.file_type().is_dir() => {
                        match self.marked_tree_path(&digest) {
                            Ok(_) => true,
                            Err(IngestError::Io(error))
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                                ) =>
                            {
                                false
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    _ => false,
                };
                if !recognized {
                    summary.preserved_entries += 1;
                    continue;
                }
                usage.entries += 1;
                usage.bytes = usage
                    .bytes
                    .checked_add(logical_bytes(&path)?)
                    .ok_or_else(|| invalid_cache("cache byte count overflow"))?;
                candidates.push((path, metadata.is_dir()));
            }
        }
        if remove {
            for (path, directory) in candidates {
                if directory {
                    fs::remove_dir_all(&path).map_err(IngestError::CacheCleanup)?;
                } else {
                    fs::remove_file(&path).map_err(IngestError::CacheCleanup)?;
                }
                sync_directory(path.parent().expect("cache entry has a parent"))
                    .map_err(IngestError::CacheCleanup)?;
            }
        }
        Ok(summary)
    }
}

fn invalid_cache(message: &str) -> IngestError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

fn directory_exists(path: &Path) -> Result<bool, IngestError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => Err(invalid_cache(
            "cache path must be a directory, not a symlink or file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn logical_bytes(root: &Path) -> Result<u64, IngestError> {
    let mut pending = vec![root.to_path_buf()];
    let mut bytes = 0u64;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if metadata.file_type().is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| invalid_cache("cache byte count overflow"))?;
        } else if !metadata.file_type().is_symlink() {
            return Err(invalid_cache("cache contains a special file"));
        }
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
