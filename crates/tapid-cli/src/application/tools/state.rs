use super::{Tool, error};
use crate::application::outcome::OperationalError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
pub(super) const LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct CatalogLock {
    file: fs::File,
}

impl Drop for CatalogLock {
    fn drop(&mut self) {
        // Like ActivationLock, release explicitly: a duplicated or briefly
        // inherited file handle must not extend the critical section.
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// Protect catalog recovery/publication and lease acquisition, never downloads
/// or command execution. Dropping the guard explicitly releases the lock.
pub(super) fn lock_catalog(root: &Path) -> Result<CatalogLock, OperationalError> {
    lock_catalog_for(root, std::time::Duration::from_secs(30))
}

fn lock_catalog_for(
    root: &Path,
    timeout: std::time::Duration,
) -> Result<CatalogLock, OperationalError> {
    use crate::application::outcome::ErrorKind;
    use fs4::{FileExt, TryLockError};
    use std::time::{Duration, Instant};
    let path = root.join(".tapid-tool-catalog.lock");
    check_path(&path)?;
    let file = crate::filesystem::activation::open_lock_file(&path).map_err(error)?;
    let started = Instant::now();
    loop {
        match FileExt::try_lock(&file) {
            Ok(()) => return Ok(CatalogLock { file }),
            Err(TryLockError::WouldBlock) if started.elapsed() < timeout => {
                std::thread::sleep(
                    Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
                );
            }
            Err(TryLockError::WouldBlock) => {
                return Err(OperationalError::new(
                    ErrorKind::ProjectBusy,
                    "tool catalog is busy; timed out waiting for publication",
                ));
            }
            Err(TryLockError::Error(e)) => return Err(error(e)),
        }
    }
}
const MARKER: &[u8] = b"tapid-tools-v1\n";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    pub schema: u32,
    #[serde(default)]
    pub settled_transaction: Option<String>,
    pub tools: BTreeMap<String, Tool>,
    pub shims: BTreeMap<String, String>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema: 1,
            settled_transaction: None,
            tools: BTreeMap::new(),
            shims: BTreeMap::new(),
        }
    }
}
pub(super) fn root_path(explicit: Option<&Path>) -> Result<PathBuf, OperationalError> {
    let mut path = if let Some(path) = explicit {
        path.to_owned()
    } else {
        let variable = if cfg!(windows) {
            "LOCALAPPDATA"
        } else {
            "HOME"
        };
        let home = || {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .ok_or_else(|| error(format!("{variable} is required to locate tools")))
        };
        if cfg!(windows) {
            home()?.join("tapid/tools")
        } else if cfg!(target_os = "macos") {
            home()?.join("Library/Application Support/tapid/tools")
        } else if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            let data = PathBuf::from(data);
            if !data.is_absolute() {
                return Err(error("XDG_DATA_HOME must be absolute"));
            }
            data.join("tapid/tools")
        } else {
            home()?.join(".local/share/tapid/tools")
        }
    };
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(error(
            "tools directory must be an absolute path without parent components",
        ));
    }
    if fs::symlink_metadata(&path).is_ok_and(|metadata| is_link(&metadata)) {
        return Err(error("tools root must not be a symlink or reparse point"));
    }
    // Resolve parent aliases (not the managed root itself), including macOS's
    // system /var alias, before choosing the managed filesystem boundary.
    if let Some(existing) = path.ancestors().find(|p| fs::symlink_metadata(p).is_ok())
        && existing
            .ancestors()
            .any(|p| fs::symlink_metadata(p).is_ok_and(|m| is_link(&m)))
    {
        let suffix = path.strip_prefix(existing).map_err(error)?;
        path = fs::canonicalize(existing).map_err(error)?.join(suffix);
    }
    check_path(&path)?;
    Ok(path)
}
pub(super) fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}
pub(super) fn check_path(path: &Path) -> Result<(), OperationalError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(error("managed path contains parent traversal"));
        }
        current.push(component);
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_link(&metadata) => {
                return Err(error(format!(
                    "managed path must not contain a symlink or reparse point: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}
pub(super) fn open_regular(path: &Path) -> Result<fs::File, OperationalError> {
    check_path(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(path).map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(error("managed state must be a regular non-link file"));
    }
    Ok(file)
}

pub(super) fn read(path: &Path) -> Result<Vec<u8>, OperationalError> {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > LIMIT {
        return Err(error("managed tool state exceeds 4 MiB"));
    }
    Ok(bytes)
}
pub(super) fn write_new(
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), OperationalError> {
    check_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(error("refusing to overwrite an existing managed file")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    let parent = path
        .parent()
        .ok_or_else(|| error("managed file has no parent"))?;
    let temporary = parent.join(format!(
        ".tapid-tool-write-{:x}",
        crate::filesystem::atomic::unique_nonce()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o755 } else { 0o600 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut file = options.open(&temporary).map_err(error)?;
    let result = (|| {
        let crash = match path.file_name().and_then(|name| name.to_str()) {
            Some("pending.json") => "tool_writing_journal",
            Some("committed") => "tool_writing_decision",
            _ if parent.file_name().is_some_and(|name| name == "bin") => "tool_writing_launcher",
            _ => "",
        };
        crate::filesystem::activation::test_crash_at(crash);
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(error)?;
        drop(file);
        // The catalog lock serializes Tapid publishers. Concurrent replacement
        // by a host process with write access is outside the ownership contract.
        check_path(path)?;
        if path.try_exists().map_err(error)? {
            return Err(error("managed file appeared before publication"));
        }
        fs::rename(&temporary, path).map_err(error)?;
        crate::filesystem::activation::sync_directory(parent).map_err(error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
pub(super) fn initialize(root: &Path) -> Result<(), OperationalError> {
    check_path(root)?;
    if root.exists()
        && !root.join(".tapid-tools").exists()
        && fs::read_dir(root).map_err(error)?.next().is_some()
    {
        return Err(error(
            "refusing a nonempty tools directory without Tapid ownership",
        ));
    }
    fs::create_dir_all(root).map_err(error)?;
    if !root.join(".tapid-tools").exists() {
        write_new(&root.join(".tapid-tools"), MARKER, false)?;
    }
    verify(root)?;
    for directory in ["bin", "environments"] {
        let path = root.join(directory);
        check_path(&path)?;
        fs::create_dir_all(path).map_err(error)?;
    }
    Ok(())
}
pub(super) fn verify(root: &Path) -> Result<(), OperationalError> {
    if read(&root.join(".tapid-tools"))? != MARKER {
        return Err(error("invalid Tapid tools ownership marker"));
    }
    Ok(())
}
pub(super) fn validate(catalog: &Catalog) -> Result<(), OperationalError> {
    if catalog.schema != 1 {
        return Err(error("unsupported tools catalog schema"));
    }
    for (name, tool) in &catalog.tools {
        name.parse::<tapid_core::PackageName>().map_err(error)?;
        tool.version
            .parse::<tapid_core::PackageVersion>()
            .map_err(error)?;
        super::validate_requirement(&tool.requirement)?;
        if tool.generation.is_empty() || !tool.generation.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error("invalid tool generation"));
        }
        for (command, bin) in &tool.commands {
            super::launchers::validate_command(command)?;
            if bin.target.is_absolute()
                || bin
                    .target
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(error("tool bin path escapes its environment"));
            }
        }
    }
    for name in catalog.shims.keys() {
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(error("invalid launcher path"));
        }
    }
    Ok(())
}
pub(super) fn load(root: &Path) -> Result<Catalog, OperationalError> {
    verify(root)?;
    let path = root.join("catalog.json");
    check_path(&path)?;
    if !path.try_exists().map_err(error)? {
        return Ok(Catalog::default());
    }
    let catalog: Catalog = serde_json::from_slice(&read(&path)?).map_err(error)?;
    validate(&catalog)?;
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::outcome::ErrorKind;
    use std::time::{Duration, Instant};
    use tapid_test_support::TempHome;

    #[test]
    fn catalog_lock_contention_is_bounded_and_retryable() {
        let home = TempHome::new("tool-catalog-timeout").unwrap();
        let root = root_path(Some(home.path())).unwrap();
        let lock = lock_catalog(&root).unwrap();
        let started = Instant::now();
        let failure = lock_catalog_for(&root, Duration::from_millis(30)).unwrap_err();
        assert_eq!(failure.kind, ErrorKind::ProjectBusy);
        assert!(started.elapsed() >= Duration::from_millis(30));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(lock);
        lock_catalog_for(&root, Duration::ZERO).unwrap();
    }

    #[test]
    fn catalog_lock_releases_even_with_a_duplicated_handle() {
        let home = TempHome::new("tool-catalog-release").unwrap();
        let root = root_path(Some(home.path())).unwrap();
        let lock = lock_catalog(&root).unwrap();
        let retained = lock.file.try_clone().unwrap();
        drop(lock);
        let next = lock_catalog_for(&root, Duration::ZERO).unwrap();
        drop(next);
        drop(retained);
    }

    #[test]
    fn catalog_lock_rejects_non_regular_files() {
        let home = TempHome::new("tool-catalog-directory").unwrap();
        let root = root_path(Some(home.path())).unwrap();
        fs::create_dir(root.join(".tapid-tool-catalog.lock")).unwrap();
        assert!(lock_catalog_for(&root, Duration::ZERO).is_err());
    }
}
