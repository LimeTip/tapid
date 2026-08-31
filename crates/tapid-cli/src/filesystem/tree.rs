use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};
use tapid_linker::{LayoutInput, ManagedRoot};

#[cfg(test)]
thread_local! {
    static BYTE_COPY_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CLONE_CALL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn materialization_progress_checkpoint(completed: usize, total: usize) -> bool {
    total > 0 && completed < total && (completed == 1 || completed.is_multiple_of(50))
}

fn complete_materialization_step<T, E>(
    completed: usize,
    total: usize,
    action: impl FnOnce() -> Result<T, E>,
    report: impl FnOnce(usize, usize),
) -> Result<T, E> {
    let value = action()?;
    if materialization_progress_checkpoint(completed, total) {
        report(completed, total);
    }
    Ok(value)
}

fn report_materialization_completion(total: usize, report: impl FnOnce(usize, usize)) {
    if total > 0 {
        report(total, total);
    }
}

pub(crate) fn materialize_stage(
    stage: &Path,
    plan: &tapid_linker::MaterializationPlan,
    input: &LayoutInput,
    trees: &BTreeMap<String, PathBuf>,
    sources_are_verified_snapshots: bool,
) -> Result<(), String> {
    fs::create_dir_all(stage.join("node_modules")).map_err(|e| e.to_string())?;
    let mut by_source = BTreeMap::new();
    let mut verified_sources = Vec::with_capacity(plan.entries.len());
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
        if !sources_are_verified_snapshots {
            let actual = tapid_archive::canonical_tree_digest(tree).map_err(|e| e.to_string())?;
            if actual != expected {
                return Err(format!(
                    "tree changed during replay: expected {expected}, got {actual}"
                ));
            }
        }
        by_source.insert(entry.target.clone(), tree.clone());
        verified_sources.push((tree.clone(), expected.to_owned()));
    }
    let materialization_total = plan.activation.steps.len();
    for (index, step) in plan.activation.steps.iter().enumerate() {
        let source = by_source
            .get(&step.source)
            .ok_or_else(|| "activation source missing".to_owned())?;
        let target = stage.join(
            step.target
                .strip_prefix(&plan.managed_root.path)
                .map_err(|_| "invalid activation target")?,
        );
        complete_materialization_step(
            index + 1,
            materialization_total,
            || copy_tree(source, &target),
            |completed, total| {
                eprintln!("Materialization progress: {completed}/{total}");
            },
        )?;
    }
    for (tree, expected) in verified_sources {
        let after = tapid_archive::canonical_tree_digest(&tree).map_err(|e| e.to_string())?;
        if after != expected {
            return Err(format!(
                "tree changed while replaying: expected {expected}, got {after}"
            ));
        }
    }
    materialize_package_shims(stage, plan)?;
    report_materialization_completion(materialization_total, |completed, total| {
        eprintln!("Materialization progress: {completed}/{total}");
    });
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
                {
                    make_unix_bin_executable(&entry.source)?;
                    std::os::unix::fs::symlink(relative_path(parent, &entry.source), &entry.target)
                        .map_err(|e| format!("cannot materialize package bin shim: {e}"))?;
                }
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

#[cfg(unix)]
fn make_unix_bin_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("cannot open package bin target: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect package bin target: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("package bin target must be a regular file".into());
    }
    let mode = metadata.permissions().mode();
    file.set_permissions(fs::Permissions::from_mode(mode | 0o111))
        .map_err(|error| format!("cannot make package bin target executable: {error}"))
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
    let root = package_content_root(source)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if clone_directory_tree(&root, target)? {
        let metadata = target.join(".tapid-executable-modes");
        match fs::symlink_metadata(&metadata) {
            Ok(file) if file.file_type().is_file() => {
                fs::remove_file(metadata).map_err(|error| error.to_string())?;
            }
            Ok(_) => return Err("internal executable metadata is not a regular file".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        return Ok(());
    }
    copy_tree_contents(&root, target)
}

/// Selects the package content root without trusting an npm wrapper name.
///
/// Direct manifests take precedence. Wrapped archives must expose exactly one
/// top-level directory with a regular `package.json`, keeping ambiguous layouts
/// fail-closed before project activation.
fn package_content_root(source: &Path) -> Result<PathBuf, String> {
    let direct_manifest = source.join("package.json");
    match fs::symlink_metadata(&direct_manifest) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            return Ok(source.to_path_buf());
        }
        Ok(_) => {
            return Err(format!(
                "installed package manifest is not a regular file: {}",
                direct_manifest.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    let mut candidates = Vec::new();
    for item in fs::read_dir(source).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let path = item.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let manifest = path.join("package.json");
        match fs::symlink_metadata(&manifest) {
            Ok(manifest_meta)
                if manifest_meta.is_file() && !manifest_meta.file_type().is_symlink() =>
            {
                candidates.push(path);
            }
            Ok(_) => {
                return Err(format!(
                    "installed package manifest is not a regular file: {}",
                    manifest.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    match candidates.as_slice() {
        [root] => Ok(root.clone()),
        [] => Err(format!(
            "installed package manifest is missing from store tree: {}",
            source.display()
        )),
        _ => Err(format!(
            "installed package tree has multiple manifest roots: {}",
            source.display()
        )),
    }
}

fn copy_tree_contents(source: &Path, target: &Path) -> Result<(), String> {
    copy_tree_contents_inner(source, target, true)
}

fn copy_tree_contents_inner(
    source: &Path,
    target: &Path,
    skip_internal_metadata: bool,
) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(target).map_err(|e| e.to_string())?;
    for item in fs::read_dir(source).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        if skip_internal_metadata && item.file_name() == ".tapid-executable-modes" {
            continue;
        }
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
            copy_tree_contents_inner(&src, &dst, false)?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else if meta.is_file() {
            copy_regular_file(&src, &dst)?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("unsupported store tree entry: {}", src.display()));
        }
    }
    Ok(())
}

fn copy_regular_file(source: &Path, target: &Path) -> Result<(), String> {
    copy_regular_file_with(source, target, clone_regular_file)
}

fn copy_regular_file_with(
    source: &Path,
    target: &Path,
    clone: impl FnOnce(&Path, &Path) -> Result<bool, String>,
) -> Result<(), String> {
    if clone(source, target)? {
        return Ok(());
    }
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| e.to_string())?;
    #[cfg(test)]
    BYTE_COPY_COUNT.set(BYTE_COPY_COUNT.get() + 1);
    io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn clone_directory_tree(source: &Path, target: &Path) -> Result<bool, String> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    const CLONE_NOFOLLOW_ANY: u32 = 0x0008;
    let canonical_source = fs::canonicalize(source).map_err(|e| e.to_string())?;
    let target_parent = target
        .parent()
        .ok_or_else(|| "package clone target has no parent".to_owned())?;
    let canonical_target = fs::canonicalize(target_parent)
        .map_err(|e| e.to_string())?
        .join(
            target
                .file_name()
                .ok_or_else(|| "package clone target has no file name".to_owned())?,
        );
    let source_path = CString::new(canonical_source.as_os_str().as_bytes())
        .map_err(|_| "source path contains an interior NUL byte".to_owned())?;
    let target_path = CString::new(canonical_target.as_os_str().as_bytes())
        .map_err(|_| "target path contains an interior NUL byte".to_owned())?;
    #[cfg(test)]
    CLONE_CALL_COUNT.set(CLONE_CALL_COUNT.get() + 1);
    // SAFETY: Both pointers remain valid NUL-terminated path strings for the
    // call. NOFOLLOW_ANY rejects symlinks anywhere in source or target paths,
    // and private staging provides a fresh destination.
    if unsafe {
        libc::clonefile(
            source_path.as_ptr(),
            target_path.as_ptr(),
            CLONE_NOOWNERCOPY | CLONE_NOFOLLOW_ANY,
        )
    } == 0
    {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    let unsupported = error.raw_os_error().is_some_and(|code| {
        [libc::EXDEV, libc::ENOTSUP, libc::ENOSYS, libc::EINVAL].contains(&code)
    });
    if unsupported && !target.exists() {
        return Ok(false);
    }
    Err(format!("cannot clone package directory: {error}"))
}

#[cfg(not(target_os = "macos"))]
fn clone_directory_tree(_source: &Path, _target: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(target_os = "macos")]
fn clone_regular_file(source: &Path, target: &Path) -> Result<bool, String> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let source_path = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| "source path contains an interior NUL byte".to_owned())?;
    let target_path = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| "target path contains an interior NUL byte".to_owned())?;
    #[cfg(test)]
    CLONE_CALL_COUNT.set(CLONE_CALL_COUNT.get() + 1);
    clone_regular_file_with(source, target, |_, _| {
        // SAFETY: Both pointers remain valid NUL-terminated path strings for the
        // duration of the call, and private staging provides a fresh destination.
        if unsafe { libc::clonefile(source_path.as_ptr(), target_path.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

#[cfg(target_os = "macos")]
fn clone_regular_file_with(
    source: &Path,
    target: &Path,
    clone: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<bool, String> {
    let Err(error) = clone(source, target) else {
        return Ok(true);
    };
    let unsupported = error.raw_os_error().is_some_and(|code| {
        [libc::EXDEV, libc::ENOTSUP, libc::ENOSYS, libc::EINVAL].contains(&code)
    });
    if !unsupported {
        return Err(format!("cannot clone package file: {error}"));
    }
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(target).map_err(|cleanup| {
                format!("cannot clean up failed package clone after {error}: {cleanup}")
            })?;
        }
        Ok(_) => {
            return Err(format!(
                "failed package clone left a non-regular destination: {}",
                target.display()
            ));
        }
        Err(inspect) if inspect.kind() == io::ErrorKind::NotFound => {}
        Err(inspect) => {
            return Err(format!(
                "cannot inspect failed package clone destination after {error}: {inspect}"
            ));
        }
    }
    Ok(false)
}

#[cfg(not(target_os = "macos"))]
fn clone_regular_file(_source: &Path, _target: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(test)]
mod copy_tests {
    #[cfg(unix)]
    #[test]
    fn unix_package_bin_target_becomes_executable() {
        use super::make_unix_bin_executable;
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        let root = std::env::temp_dir().join(format!(
            "tapid-bin-mode-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("tool");
        fs::write(&target, b"#!/bin/sh\nprintf executable").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        make_unix_bin_executable(&target).unwrap();

        assert_ne!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o111,
            0
        );
        let output = Command::new(&target).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"executable");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_handles_nested_directories_and_preserves_executable_mode() {
        use super::copy_tree;
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
        fs::write(source.join("package.json"), br#"{"name":"example"}"#).unwrap();
        fs::write(source.join("nested").join("data"), b"nested").unwrap();
        let bin = source.join("bin");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }

        copy_tree(&source, &target).unwrap();
        assert_eq!(
            fs::read(target.join("nested").join("data")).unwrap(),
            b"nested"
        );
        assert!(target.join("bin").is_file());
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

    #[test]
    fn copy_tree_normalizes_a_single_named_npm_wrapper() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-wrapper-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(source.join("mdx")).unwrap();
        fs::write(source.join("mdx/package.json"), br#"{"name":"@types/mdx"}"#).unwrap();
        fs::write(source.join("mdx/index.d.ts"), b"export {};\n").unwrap();

        copy_tree(&source, &target).unwrap();

        assert!(target.join("package.json").is_file());
        assert!(target.join("index.d.ts").is_file());
        assert!(!target.join("mdx").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_rejects_ambiguous_named_npm_wrappers() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-ambiguous-wrapper-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        for wrapper in ["mdx", "other"] {
            fs::create_dir_all(source.join(wrapper)).unwrap();
            fs::write(
                source.join(wrapper).join("package.json"),
                br#"{"name":"ambiguous"}"#,
            )
            .unwrap();
        }

        let error = copy_tree(&source, &root.join("target")).unwrap_err();

        assert!(error.contains("multiple manifest roots"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_rejects_a_store_tree_without_a_package_manifest() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-missing-manifest-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("README.md"), b"missing manifest\n").unwrap();

        let error = copy_tree(&source, &root.join("target")).unwrap_err();

        assert!(error.contains("manifest is missing"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn materialization_progress_is_truthful_and_bounded() {
        use super::{complete_materialization_step, report_materialization_completion};

        let events = std::cell::RefCell::new(Vec::new());
        for completed in 1..=612 {
            complete_materialization_step(
                completed,
                612,
                || {
                    events.borrow_mut().push(("copy", completed));
                    Ok::<_, ()>(())
                },
                |completed, _| events.borrow_mut().push(("progress", completed)),
            )
            .unwrap();
        }
        events.borrow_mut().push(("finish", 612));
        report_materialization_completion(612, |completed, _| {
            events.borrow_mut().push(("progress", completed));
        });
        let events = events.into_inner();
        let reports = events
            .iter()
            .filter(|(event, _)| *event == "progress")
            .collect::<Vec<_>>();

        assert_eq!(events.first(), Some(&("copy", 1)));
        assert_eq!(events.get(1), Some(&("progress", 1)));
        assert_eq!(events.get(events.len() - 2), Some(&("finish", 612)));
        assert_eq!(events.last(), Some(&("progress", 612)));
        assert!(reports.len() <= 14);
    }

    #[test]
    fn materialize_stage_does_not_copy_instances_through_an_intermediate_tree() {
        use super::materialize_stage;
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::{collections::BTreeMap, fs};
        use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};
        use tapid_linker::{
            InstanceKey, LayoutInput, ManagedRoot, PackageInstance, Platform,
            VerifiedTreeReference, plan_layout,
        };

        let root = std::env::temp_dir().join(format!(
            "tapid-single-copy-stage-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("store-tree");
        let project = root.join("project");
        let stage = root.join("stage");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            source.join("package.json"),
            br#"{"name":"example","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(source.join("index.js"), b"module.exports = 1;\n").unwrap();
        let digest = tapid_archive::canonical_tree_digest(&source).unwrap();
        let instance = PackageInstance {
            id: PackageInstanceId::new(
                "https://registry.example".parse().unwrap(),
                "example".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            ),
            peer_context: PeerContext::default(),
            platform_context: PlatformContext::new(None, None, None).unwrap(),
            tree: VerifiedTreeReference::new(&digest, &source).unwrap(),
        };
        let input = LayoutInput {
            root_dependencies: vec![InstanceKey::from(&instance)],
            instances: vec![instance],
            dependency_edges: Vec::new(),
        };
        let plan = plan_layout(
            ManagedRoot::new(&project).unwrap(),
            input.clone(),
            Platform::Unix,
        )
        .unwrap();

        materialize_stage(
            &stage,
            &plan,
            &input,
            &BTreeMap::from([("example".to_owned(), source)]),
            false,
        )
        .unwrap();

        assert!(stage.join("node_modules/example/package.json").is_file());
        assert!(stage.join("node_modules/example/index.js").is_file());
        assert!(
            !stage.join(".tapid/instances").exists(),
            "materialization must not duplicate package bytes through a discarded intermediate tree"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn generated_unix_package_bin_executes_from_an_initially_non_executable_target() {
        use super::materialize_stage;
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, process::Command};
        use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};
        use tapid_linker::{
            InstanceKey, LayoutInput, ManagedRoot, PackageInstance, Platform,
            VerifiedTreeReference, plan_layout,
        };

        let root = std::env::temp_dir().join(format!(
            "tapid-generated-bin-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("store-tree");
        let project = root.join("project");
        let stage = root.join("stage");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            source.join("package.json"),
            br#"{"name":"example","version":"1.0.0","bin":{"example-tool":"bin/tool"}}"#,
        )
        .unwrap();
        let bin = source.join("bin/tool");
        fs::write(&bin, b"#!/bin/sh\nprintf generated-bin").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(fs::metadata(&bin).unwrap().permissions().mode() & 0o111, 0);
        let digest = tapid_archive::canonical_tree_digest(&source).unwrap();
        let instance = PackageInstance {
            id: PackageInstanceId::new(
                "https://registry.example".parse().unwrap(),
                "example".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            ),
            peer_context: PeerContext::default(),
            platform_context: PlatformContext::new(None, None, None).unwrap(),
            tree: VerifiedTreeReference::new(&digest, &source).unwrap(),
        };
        let input = LayoutInput {
            root_dependencies: vec![InstanceKey::from(&instance)],
            instances: vec![instance],
            dependency_edges: Vec::new(),
        };
        let plan = plan_layout(
            ManagedRoot::new(&project).unwrap(),
            input.clone(),
            Platform::Unix,
        )
        .unwrap();

        materialize_stage(
            &stage,
            &plan,
            &input,
            &BTreeMap::from([("example".to_owned(), source)]),
            false,
        )
        .unwrap();

        let shim = stage.join("node_modules/.bin/example-tool");
        assert!(
            fs::symlink_metadata(&shim)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let output = Command::new(&shim).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"generated-bin");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn regular_file_clone_einval_falls_back_to_byte_copy() {
        use super::{clone_regular_file_with, copy_regular_file_with};
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::{fs, io};

        let root = std::env::temp_dir().join(format!(
            "tapid-clonefile-einval-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let target = root.join("target");
        fs::write(&source, b"clone fallback").unwrap();

        copy_regular_file_with(&source, &target, |source, target| {
            clone_regular_file_with(source, target, |_, _| {
                Err(io::Error::from_raw_os_error(libc::EINVAL))
            })
        })
        .unwrap();

        assert_eq!(fs::read(target).unwrap(), b"clone fallback");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn copy_tree_uses_copy_on_write_clones_on_macos() {
        use super::{BYTE_COPY_COUNT, CLONE_CALL_COUNT, copy_tree};
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-clonefile-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), br#"{"name":"clone-test"}"#).unwrap();
        fs::write(source.join("data.bin"), vec![7_u8; 1024 * 1024]).unwrap();
        fs::write(source.join(".tapid-executable-modes"), b"data.bin\n").unwrap();
        BYTE_COPY_COUNT.set(0);
        CLONE_CALL_COUNT.set(0);

        copy_tree(&source, &target).unwrap();

        assert_eq!(
            BYTE_COPY_COUNT.get(),
            0,
            "APFS-capable materialization should not stream package bytes"
        );
        assert_eq!(
            CLONE_CALL_COUNT.get(),
            1,
            "the package hierarchy should be cloned atomically in one filesystem operation"
        );
        assert!(!target.join(".tapid-executable-modes").exists());
        fs::write(target.join("data.bin"), b"project mutation").unwrap();
        assert_eq!(
            fs::read(source.join("data.bin")).unwrap(),
            vec![7_u8; 1024 * 1024],
            "copy-on-write project mutation must not alter verified store bytes"
        );
        let _ = fs::remove_dir_all(root);
    }
}
