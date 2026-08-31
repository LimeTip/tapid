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
    let root = archive_package_root(source)?;
    validate_tree(&root)?;
    copy_tree_contents(&root, target)
}

fn validate_tree(root: &Path) -> Result<(), String> {
    let root_meta = fs::symlink_metadata(root).map_err(|e| e.to_string())?;
    if !root_meta.is_dir() {
        return Err(format!(
            "store tree root is not a directory: {}",
            root.display()
        ));
    }
    for item in fs::read_dir(root).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let path = item.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "symlink in store tree is not replayable: {}",
                path.display()
            ));
        }
        if meta.is_dir() {
            validate_tree(&path)?;
        } else if !meta.is_file() {
            return Err(format!("unsupported store tree entry: {}", path.display()));
        }
    }
    Ok(())
}

/// Finds the package root in an extracted npm archive.
///
/// npm archives conventionally use `package/`, but valid archives also use a
/// package-specific wrapper directory. Only a direct manifest or one
/// unambiguous wrapper manifest is accepted.
fn archive_package_root(source: &Path) -> Result<PathBuf, String> {
    let direct_manifest = source.join("package.json");
    match fs::symlink_metadata(&direct_manifest) {
        Ok(meta) if meta.is_file() => return Ok(source.to_path_buf()),
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "symlink package manifest is not replayable: {}",
                direct_manifest.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "symlink in store tree is not replayable: {}",
                path.display()
            ));
        }
        if !meta.is_dir() {
            if !meta.is_file() {
                return Err(format!("unsupported store tree entry: {}", path.display()));
            }
            continue;
        }
        let manifest = path.join("package.json");
        match fs::symlink_metadata(&manifest) {
            Ok(manifest_meta) if manifest_meta.is_file() => candidates.push(path),
            Ok(manifest_meta) if manifest_meta.file_type().is_symlink() => {
                return Err(format!(
                    "symlink package manifest is not replayable: {}",
                    manifest.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }

    match candidates.as_slice() {
        [root] => Ok(root.clone()),
        [] => Err(format!(
            "cannot find unambiguous package manifest in {}",
            source.display()
        )),
        _ => Err(format!(
            "multiple package manifests in archive root {}",
            source.display()
        )),
    }
}

fn copy_tree_contents(source: &Path, target: &Path) -> Result<(), String> {
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
            copy_tree_contents(&src, &dst)?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else if meta.is_file() {
            let mut input = open_regular_file_without_following_symlinks(&src)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dst)
                .map_err(|e| e.to_string())?;
            io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
            fs::set_permissions(&dst, meta.permissions()).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("unsupported store tree entry: {}", src.display()));
        }
    }
    Ok(())
}

fn open_regular_file_without_following_symlinks(path: &Path) -> Result<fs::File, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    fs::File::open(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod copy_tests {
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
        fs::write(source.join("package.json"), b"{}\n").unwrap();
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
    fn copy_tree_accepts_a_non_package_wrapper_directory() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-wrapper-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(source.join("mdx")).unwrap();
        fs::write(source.join("mdx/package.json"), b"{\"name\":\"mdx\"}\n").unwrap();
        fs::write(source.join("mdx/index.js"), b"module.exports = 1;\n").unwrap();

        copy_tree(&source, &target).unwrap();
        assert!(target.join("package.json").is_file());
        assert!(target.join("index.js").is_file());
        assert!(!target.join("mdx").exists());

        let legacy_source = root.join("legacy-source");
        let legacy_target = root.join("legacy-target");
        fs::create_dir_all(legacy_source.join("package")).unwrap();
        fs::write(legacy_source.join("package/package.json"), b"{}\n").unwrap();
        copy_tree(&legacy_source, &legacy_target).unwrap();
        assert!(legacy_target.join("package.json").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_rejects_ambiguous_and_missing_manifests() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-wrapper-invalid-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let missing = root.join("missing");
        fs::create_dir_all(&missing).unwrap();
        assert!(copy_tree(&missing, &root.join("missing-target")).is_err());

        let ambiguous = root.join("ambiguous");
        fs::create_dir_all(ambiguous.join("one")).unwrap();
        fs::create_dir_all(ambiguous.join("two")).unwrap();
        fs::write(ambiguous.join("one/package.json"), b"{}\n").unwrap();
        fs::write(ambiguous.join("two/package.json"), b"{}\n").unwrap();
        assert!(copy_tree(&ambiguous, &root.join("ambiguous-target")).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let symlinked = root.join("symlinked");
            fs::create_dir_all(&symlinked).unwrap();
            symlink("missing-package", symlinked.join("package")).unwrap();
            assert!(copy_tree(&symlinked, &root.join("symlinked-target")).is_err());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_rejects_special_file_before_materialization() {
        use super::copy_tree;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-copy-special-file-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{}\n").unwrap();
        fs::write(source.join("valid.js"), b"module.exports = true;\n").unwrap();

        let status = std::process::Command::new("mkfifo")
            .arg(source.join("pipe.js"))
            .status()
            .unwrap();
        assert!(status.success());

        let error = copy_tree(&source, &target).unwrap_err();

        assert!(error.contains("unsupported store tree entry"));
        assert!(!target.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_tree_validates_direct_root_before_materialization() {
        use super::copy_tree;
        use std::fs;
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "tapid-direct-root-validation-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), b"{}\n").unwrap();
        fs::write(source.join("valid.js"), b"module.exports = 1;\n").unwrap();
        symlink("missing.js", source.join("link.js")).unwrap();

        let error = copy_tree(&source, &target).unwrap_err();
        assert!(error.contains("symlink in store tree is not replayable"));
        assert!(!target.exists());
        let _ = fs::remove_dir_all(root);
    }
}
