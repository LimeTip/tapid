use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};
use tapid_linker::{LayoutInput, ManagedRoot};

pub(crate) fn materialize_stage(
    stage: &Path,
    plan: &tapid_linker::MaterializationPlan,
    input: &LayoutInput,
    trees: &BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    fs::create_dir_all(stage.join("node_modules")).map_err(|e| e.to_string())?;
    let mut by_source = BTreeMap::new();
    for entry in &plan.entries {
        let tree = trees
            .values()
            .find(|path| **path == entry.source)
            .ok_or_else(|| "tree replay mapping lost".to_owned())?;
        let expected = input
            .instances
            .iter()
            .find(|instance| instance.tree.root == *tree)
            .map(|instance| instance.tree.digest.as_str())
            .ok_or_else(|| "tree replay digest mapping lost".to_owned())?;
        let actual = tapid_archive::canonical_tree_digest(tree).map_err(|e| e.to_string())?;
        if actual != expected {
            return Err(format!(
                "tree changed during replay: expected {expected}, got {actual}"
            ));
        }
        let target = stage.join(
            entry
                .target
                .strip_prefix(&plan.managed_root.path)
                .map_err(|_| "invalid stage target")?,
        );
        copy_tree(tree, &target)?;
        let after = tapid_archive::canonical_tree_digest(tree).map_err(|e| e.to_string())?;
        if after != expected {
            return Err(format!(
                "tree changed while replaying: expected {expected}, got {after}"
            ));
        }
        by_source.insert(entry.target.clone(), target);
    }
    for step in &plan.activation.steps {
        let source = by_source
            .get(&step.source)
            .ok_or_else(|| "activation source missing".to_owned())?;
        let target = stage.join(
            step.target
                .strip_prefix(&plan.managed_root.path)
                .map_err(|_| "invalid activation target")?,
        );
        copy_tree(source, &target)?;
    }
    materialize_package_shims(stage, plan)?;
    Ok(())
}

fn package_root_for_shims(package_dir: &Path) -> PathBuf {
    let nested = package_dir.join("package");
    if !package_dir.join("package.json").is_file() && nested.join("package.json").is_file() {
        nested
    } else {
        package_dir.to_path_buf()
    }
}

pub(crate) fn powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "\'\'")
}

pub(crate) fn cmd_batch_path(value: &str) -> String {
    value.replace('%', "%%")
}

pub(crate) fn cmd_shim_contents(parent: &Path, source: &Path) -> String {
    format!(
        "@echo off\r\n@setlocal DisableDelayedExpansion\r\n\"%~dp0{}\" %*\r\n",
        cmd_batch_path(&relative_path(parent, source).display().to_string())
    )
}

pub(crate) fn powershell_shim_contents(parent: &Path, source: &Path) -> String {
    format!(
        "& (Join-Path $PSScriptRoot '{}') $args\r\n",
        powershell_single_quoted(&relative_path(parent, source).display().to_string())
    )
}

fn materialize_package_shims(
    stage: &Path,
    plan: &tapid_linker::MaterializationPlan,
) -> Result<(), String> {
    let managed = ManagedRoot::new(stage.join("node_modules")).map_err(|e| e.to_string())?;
    let mut packages = Vec::new();
    for step in &plan.activation.steps {
        let package_dir = stage.join("node_modules").join(
            step.target
                .strip_prefix(plan.managed_root.path.join("node_modules"))
                .map_err(|_| "invalid package activation target")?,
        );
        let package_root = package_root_for_shims(&package_dir);
        let package_json = fs::read_to_string(package_root.join("package.json"))
            .map_err(|e| format!("cannot read installed package manifest: {e}"))?;
        packages.push(tapid_linker::ShimPackage {
            tree_root: package_root,
            package_json,
            bin_dir: package_dir
                .parent()
                .ok_or_else(|| "installed package has no node_modules parent".to_owned())?
                .to_path_buf(),
        });
    }
    let shims = tapid_linker::plan_shims(
        managed,
        packages,
        crate::application::replay::current_platform(),
    )
    .map_err(|e| e.to_string())?;
    for entry in shims.entries {
        let parent = entry
            .target
            .parent()
            .ok_or_else(|| "shim target has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        match entry.strategy {
            tapid_linker::ShimStrategy::UnixSymlink => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(relative_path(parent, &entry.source), &entry.target)
                    .map_err(|e| format!("cannot materialize package bin shim: {e}"))?;
                #[cfg(not(unix))]
                return Err("package bin shims are unsupported on this platform".into());
            }
            tapid_linker::ShimStrategy::WindowsCmdAndPowerShell => {
                let cmd = entry.target.with_extension("cmd");
                let ps1 = entry.target.with_extension("ps1");
                fs::write(cmd, cmd_shim_contents(parent, &entry.source))
                    .map_err(|e| e.to_string())?;
                fs::write(ps1, powershell_shim_contents(parent, &entry.source))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common..] {
        result.push(component.as_os_str());
    }
    result
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    let source_meta = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if !source_meta.is_dir() {
        return Err("store tree root is not a directory".into());
    }
    let package = source.join("package");
    let root = match fs::symlink_metadata(&package) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "symlink in store tree is not replayable: {}",
                package.display()
            ));
        }
        Ok(meta) if meta.is_dir() => &package,
        Ok(_) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => source,
        Err(error) => return Err(error.to_string()),
    };
    copy_tree_contents(root, target)
}

fn copy_tree_contents(source: &Path, target: &Path) -> Result<(), String> {
    copy_tree_contents_with(source, target, clone_file)
}

fn copy_tree_contents_with(
    source: &Path,
    target: &Path,
    clone: fn(&Path, &Path) -> Result<(), io::Error>,
) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(target).map_err(|e| e.to_string())?;
    for item in fs::read_dir(source).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let src = item.path();
        let dst = target.join(item.file_name());
        let meta = fs::symlink_metadata(&src).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "symlink in store tree is not replayable: {}",
                src.display()
            ));
        }
        if meta.is_dir() {
            copy_tree_contents_with(&src, &dst, clone)?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else if meta.is_file() {
            copy_file_isolated(&src, &dst, clone)?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("unsupported store tree entry: {}", src.display()));
        }
    }
    Ok(())
}

fn copy_file_isolated(
    source: &Path,
    target: &Path,
    clone: fn(&Path, &Path) -> Result<(), io::Error>,
) -> Result<(), String> {
    COPY_FILE_ISOLATED_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if clone(source, target).is_ok() {
        return Ok(());
    }
    COPY_FILE_BYTE_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| e.to_string())?;
    io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn clone_file(source: &Path, target: &Path) -> Result<(), io::Error> {
    use std::os::fd::AsRawFd;
    let input = fs::File::open(source)?;
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let result = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        drop(output);
        let _ = fs::remove_file(target);
        Err(error)
    }
}

#[cfg(target_os = "macos")]
fn clone_file(source: &Path, target: &Path) -> Result<(), io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target path contains NUL"))?;
    let result = unsafe { libc::clonefile(source.as_ptr(), target.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn clone_file(_source: &Path, _target: &Path) -> Result<(), io::Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file cloning unavailable",
    ))
}

static COPY_FILE_ISOLATED_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static COPY_FILE_BYTE_FALLBACKS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
mod copy_tests {
    static COPY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn copy_tree_clone_success_bypasses_byte_copy_fallback() {
        let _guard = COPY_TEST_LOCK.lock().unwrap();
        use super::{
            COPY_FILE_BYTE_FALLBACKS, COPY_FILE_ISOLATED_ATTEMPTS, copy_tree_contents_with,
        };
        use std::{fs, io, path::Path};

        fn clone_success(source: &Path, target: &Path) -> Result<(), io::Error> {
            fs::copy(source, target).map(|_| ())
        }

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-clone-success-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("data"), b"cloned").unwrap();

        COPY_FILE_ISOLATED_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);
        COPY_FILE_BYTE_FALLBACKS.store(0, std::sync::atomic::Ordering::Relaxed);
        copy_tree_contents_with(&source, &target, clone_success).unwrap();

        assert_eq!(
            COPY_FILE_ISOLATED_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            COPY_FILE_BYTE_FALLBACKS.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(fs::read(target.join("data")).unwrap(), b"cloned");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_clone_failure_uses_byte_copy_fallback() {
        let _guard = COPY_TEST_LOCK.lock().unwrap();
        use super::{
            COPY_FILE_BYTE_FALLBACKS, COPY_FILE_ISOLATED_ATTEMPTS, copy_tree_contents_with,
        };
        use std::{fs, io, path::Path};

        fn clone_failure(_source: &Path, _target: &Path) -> Result<(), io::Error> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "forced test failure",
            ))
        }

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-clone-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("data"), b"fallback").unwrap();

        COPY_FILE_ISOLATED_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);
        COPY_FILE_BYTE_FALLBACKS.store(0, std::sync::atomic::Ordering::Relaxed);
        copy_tree_contents_with(&source, &target, clone_failure).unwrap();

        assert_eq!(
            COPY_FILE_ISOLATED_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            COPY_FILE_BYTE_FALLBACKS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(fs::read(target.join("data")).unwrap(), b"fallback");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_handles_nested_directories_and_preserves_executable_mode() {
        let _guard = COPY_TEST_LOCK.lock().unwrap();
        use super::{COPY_FILE_ISOLATED_ATTEMPTS, copy_tree};
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested").join("data"), b"nested").unwrap();
        let bin = source.join("bin");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }

        COPY_FILE_ISOLATED_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);
        copy_tree(&source, &target).unwrap();
        assert_eq!(
            COPY_FILE_ISOLATED_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        assert_eq!(
            fs::read(target.join("nested").join("data")).unwrap(),
            b"nested"
        );
        assert!(target.join("bin").is_file());
        fs::write(target.join("nested").join("data"), b"changed").unwrap();
        assert_eq!(
            fs::read(source.join("nested").join("data")).unwrap(),
            b"nested"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(target.join("nested"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
            assert_eq!(
                fs::metadata(target.join("bin"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        let _ = fs::remove_dir_all(root);
    }
}
