use std::path::{Component, PathBuf};

use crate::{ManagedRoot, PlanError, Platform};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimPackage {
    pub tree_root: PathBuf,
    pub package_json: String,
    pub bin_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShimStrategy {
    UnixSymlink,
    WindowsCmdAndPowerShell,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimEntry {
    pub command: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub strategy: ShimStrategy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimPlan {
    pub managed_root: ManagedRoot,
    pub entries: Vec<ShimEntry>,
}

pub fn plan_shims(
    managed_root: ManagedRoot,
    packages: Vec<ShimPackage>,
    platform: Platform,
) -> Result<ShimPlan, PlanError> {
    let strategy = match platform {
        Platform::Unix => ShimStrategy::UnixSymlink,
        Platform::Windows => ShimStrategy::WindowsCmdAndPowerShell,
        Platform::Other => return Err(PlanError::UnsupportedPlatform(platform)),
    };
    let mut entries = Vec::new();
    for package in packages {
        if !package.tree_root.is_absolute()
            || package
                .tree_root
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(PlanError::PathOutsideManagedRoot(package.tree_root));
        }
        if !managed_root.contains(&package.bin_dir) {
            return Err(PlanError::PathOutsideManagedRoot(package.bin_dir));
        }
        let manifest = tapid_manifest::PackageManifest::parse(&package.package_json)
            .map_err(|error| PlanError::InvalidPackageMetadata(error.to_string()))?;
        let Some(bin) = manifest.bin() else { continue };
        let bin_dir = package.bin_dir.join(".bin");
        for target in bin.targets() {
            let source = package.tree_root.join(&target.target);
            if !managed_root.contains(&source) {
                return Err(PlanError::PathOutsideManagedRoot(source));
            }
            let file_type = match std::fs::symlink_metadata(&source) {
                Ok(metadata) => metadata.file_type(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(PlanError::BinTargetMissing(source.clone())),
            };
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(PlanError::BinTargetNotRegular(source));
            }
            let target_path = bin_dir.join(&target.command);
            if !managed_root.contains(&target_path) {
                return Err(PlanError::PathOutsideManagedRoot(target_path));
            }
            if entries
                .iter()
                .any(|entry: &ShimEntry| entry.target == target_path)
            {
                return Err(PlanError::ShimCollision(target_path));
            }
            entries.push(ShimEntry {
                command: target.command.clone(),
                source,
                target: target_path,
                strategy,
            });
        }
    }
    entries.sort_by(|a, b| a.target.cmp(&b.target).then(a.source.cmp(&b.source)));
    Ok(ShimPlan {
        managed_root,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn shim_package(root: &Path, name: &str, bin: &str, dir: &str) -> ShimPackage {
        let tree = root.join(dir);
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("cli.js"), "#!/usr/bin/env node\n").unwrap();
        ShimPackage {
            tree_root: tree,
            package_json: format!(r#"{{"name":"{name}","version":"1.0.0","bin":{bin}}}"#),
            bin_dir: root.join("node_modules"),
        }
    }

    #[test]
    fn shims_are_deterministic_and_platform_strategy_is_intent_only() {
        let root = std::env::temp_dir().join(format!("tapid-shims-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let a = shim_package(
            &root,
            "@scope/tool",
            r#"{"z":"./cli.js","tool":"cli.js"}"#,
            "a",
        );
        let b = shim_package(&root, "other", r#""./cli.js""#, "b");
        let p1 = plan_shims(
            ManagedRoot::new(&root).unwrap(),
            vec![b.clone(), a.clone()],
            Platform::Windows,
        )
        .unwrap();
        let p2 = plan_shims(
            ManagedRoot::new(&root).unwrap(),
            vec![a, b],
            Platform::Windows,
        )
        .unwrap();
        assert_eq!(p1, p2);
        assert_eq!(
            p1.entries[0].strategy,
            ShimStrategy::WindowsCmdAndPowerShell
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nested_package_bin_directories_are_distinct_and_collisions_rejected() {
        let root = std::env::temp_dir().join(format!("tapid-shims-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut a = shim_package(&root, "a", r#"{"cli":"cli.js"}"#, "a");
        let mut b = shim_package(&root, "b", r#"{"cli":"cli.js"}"#, "b");
        b.bin_dir = root.join("node_modules").join("a");
        let plan = plan_shims(
            ManagedRoot::new(&root).unwrap(),
            vec![a.clone(), b],
            Platform::Unix,
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 2);
        a.bin_dir = root.join("node_modules");
        assert!(matches!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![a.clone(), a],
                Platform::Unix
            ),
            Err(PlanError::ShimCollision(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_bin_targets_are_skipped_and_non_regular_targets_are_rejected() {
        let root = std::env::temp_dir().join(format!("tapid-shims-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let missing = shim_package(&root, "missing", r#""missing.js""#, "missing");
        std::fs::remove_file(missing.tree_root.join("cli.js")).unwrap();
        let missing_plan = plan_shims(
            ManagedRoot::new(&root).unwrap(),
            vec![missing],
            Platform::Unix,
        )
        .unwrap();
        assert!(missing_plan.entries.is_empty());
        let directory = shim_package(&root, "directory", r#""cli.js""#, "directory");
        std::fs::remove_file(directory.tree_root.join("cli.js")).unwrap();
        std::fs::create_dir(directory.tree_root.join("cli.js")).unwrap();
        assert!(matches!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![directory],
                Platform::Unix
            ),
            Err(PlanError::BinTargetNotRegular(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_bin_targets_are_rejected_without_following_them() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("tapid-shims-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let package = shim_package(&root, "linked", r#""cli.js""#, "linked");
        std::fs::remove_file(package.tree_root.join("cli.js")).unwrap();
        symlink("/etc/passwd", package.tree_root.join("cli.js")).unwrap();
        assert!(matches!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![package],
                Platform::Unix
            ),
            Err(PlanError::BinTargetNotRegular(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
