//! Conservative eviction of published data. Staging and recovery belong to
//! their existing owners and are never swept by cache maintenance.
use super::*;
mod directory;
use directory::{CacheDirectory, Kind};

#[cfg(test)]
thread_local! {
    static BEFORE_CACHE_REMOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

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
        let root = match CacheDirectory::open_root(&self.root) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CacheSummary::default());
            }
            Err(error)
                if error.kind() == io::ErrorKind::InvalidInput
                    || error.kind() == io::ErrorKind::NotADirectory =>
            {
                return Err(IngestError::CachePath(error));
            }
            Err(error) => return Err(error.into()),
        };
        // Inspection neither creates a lock nor recovers a journal. The lock
        // and all subsequent children are opened through the pinned root.
        let file = match root.open_file(std::ffi::OsStr::new(".store.lock")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if optional_directory(&root, "artifacts")?.is_none()
                    && optional_directory(&root, "trees")?.is_none()
                    && root
                        .metadata(std::ffi::OsStr::new(STORE_JOURNAL))
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
        match root.metadata(std::ffi::OsStr::new(STORE_JOURNAL)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(invalid_cache(
                    "cache has a pending transaction; run an install to recover it before maintenance",
                ));
            }
        }
        let mut summary = CacheSummary::default();
        let mut namespaces = Vec::new();
        // Pin both namespaces and preflight every candidate before eviction.
        for (namespace, usage) in [
            ("artifacts", &mut summary.artifacts),
            ("trees", &mut summary.trees),
        ] {
            let Some(directory) = optional_directory(&root, namespace)? else {
                continue;
            };
            let mut candidates = Vec::new();
            for name in directory.entries()? {
                let metadata = directory.metadata(&name)?;
                let digest = name.to_str().and_then(|s| s.parse::<ArtifactDigest>().ok());
                let bytes = match (namespace, digest, metadata.kind) {
                    ("artifacts", Some(_), Kind::File) => Some(metadata.bytes),
                    ("trees", Some(digest), Kind::Directory) => {
                        let child = directory.directory(&name)?;
                        if child.identity()? != metadata.identity {
                            return Err(invalid_cache("cache tree changed during inspection"));
                        }
                        if marked_tree(&child, &digest)? {
                            Some(child.logical_bytes()?)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let Some(bytes) = bytes else {
                    summary.preserved_entries += 1;
                    continue;
                };
                usage.entries += 1;
                usage.bytes = usage
                    .bytes
                    .checked_add(bytes)
                    .ok_or_else(|| invalid_cache("cache byte count overflow"))?;
                candidates.push((name, metadata));
            }
            namespaces.push((directory, candidates));
        }
        if remove {
            #[cfg(test)]
            BEFORE_CACHE_REMOVE.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            for (directory, candidates) in &namespaces {
                for (name, metadata) in candidates {
                    directory
                        .remove(name, *metadata)
                        .map_err(IngestError::CacheCleanup)?;
                    directory.sync().map_err(IngestError::CacheCleanup)?;
                }
            }
        }
        Ok(summary)
    }
}

fn invalid_cache(message: &str) -> IngestError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

fn optional_directory(
    root: &CacheDirectory,
    name: &str,
) -> Result<Option<CacheDirectory>, IngestError> {
    match root.directory(std::ffi::OsStr::new(name)) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn marked_tree(directory: &CacheDirectory, digest: &ArtifactDigest) -> Result<bool, IngestError> {
    let marker = std::ffi::OsStr::new(".tapid-tree");
    match directory.metadata(marker) {
        Ok(metadata) if metadata.kind == Kind::File => Ok(tree_marker_matches(
            &mut directory.open_file(marker)?,
            digest,
        )?),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
