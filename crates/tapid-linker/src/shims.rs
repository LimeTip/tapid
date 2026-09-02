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
            let output_keys = shim_output_keys(&target_path, strategy);
            if entries.iter().any(|entry: &ShimEntry| {
                shim_output_keys(&entry.target, strategy)
                    .iter()
                    .any(|key| output_keys.iter().any(|output_key| output_key == key))
            }) {
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

fn shim_output_keys(path: &std::path::Path, strategy: ShimStrategy) -> Vec<String> {
    match strategy {
        ShimStrategy::WindowsCmdAndPowerShell => ["cmd", "ps1"]
            .into_iter()
            .map(|extension| shim_target_key(&path.with_extension(extension), strategy))
            .collect(),
        ShimStrategy::UnixSymlink => vec![shim_target_key(path, strategy)],
    }
}

fn shim_target_key(path: &std::path::Path, strategy: ShimStrategy) -> String {
    let value = path.to_string_lossy();
    match strategy {
        ShimStrategy::WindowsCmdAndPowerShell => value
            .chars()
            .flat_map(|character| {
                let mut uppercase = character.to_uppercase();
                match (uppercase.next(), uppercase.next()) {
                    (Some(mapped), None) => std::iter::once(mapped).collect::<Vec<_>>(),
                    _ => std::iter::once(character).collect::<Vec<_>>(),
                }
            })
            .collect(),
        ShimStrategy::UnixSymlink => value.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);

    fn test_root(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "tapid-shims-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

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
        let root = test_root("basic");
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
        let root = test_root("nested");
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
    fn windows_shims_reject_command_names_that_differ_only_by_case() {
        let root = test_root("case");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let upper = shim_package(&root, "upper", r#"{"Tool":"cli.js"}"#, "upper");
        let lower = shim_package(&root, "lower", r#"{"tool":"cli.js"}"#, "lower");
        let managed_root = ManagedRoot::new(&root).unwrap();
        let windows = plan_shims(
            managed_root.clone(),
            vec![upper.clone(), lower.clone()],
            Platform::Windows,
        );
        println!("windows: {windows:?}");
        assert!(matches!(windows, Err(PlanError::ShimCollision(_))));
        assert_eq!(
            plan_shims(managed_root, vec![upper, lower], Platform::Unix)
                .unwrap()
                .entries
                .len(),
            2
        );
        assert_eq!(
            shim_target_key(Path::new("Tool"), ShimStrategy::WindowsCmdAndPowerShell),
            shim_target_key(Path::new("tool"), ShimStrategy::WindowsCmdAndPowerShell)
        );
        assert_eq!(
            shim_target_key(Path::new("Ä"), ShimStrategy::WindowsCmdAndPowerShell),
            shim_target_key(Path::new("ä"), ShimStrategy::WindowsCmdAndPowerShell)
        );
        assert_eq!(
            shim_target_key(Path::new("ΟΣ"), ShimStrategy::WindowsCmdAndPowerShell),
            shim_target_key(Path::new("οσ"), ShimStrategy::WindowsCmdAndPowerShell)
        );
        assert_eq!(
            shim_target_key(Path::new("ς"), ShimStrategy::WindowsCmdAndPowerShell),
            shim_target_key(Path::new("σ"), ShimStrategy::WindowsCmdAndPowerShell)
        );
        assert_ne!(
            shim_target_key(Path::new("ß"), ShimStrategy::WindowsCmdAndPowerShell),
            shim_target_key(Path::new("SS"), ShimStrategy::WindowsCmdAndPowerShell)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn windows_shims_reject_names_that_replace_the_output_extension() {
        let root = test_root("extension");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plain = shim_package(&root, "plain", r#"{"tool":"cli.js"}"#, "plain");
        let suffixed = shim_package(&root, "suffixed", r#"{"tool.cmd":"cli.js"}"#, "suffixed");
        let managed_root = ManagedRoot::new(&root).unwrap();
        assert!(matches!(
            plan_shims(
                managed_root.clone(),
                vec![plain.clone(), suffixed.clone()],
                Platform::Windows,
            ),
            Err(PlanError::ShimCollision(_))
        ));
        assert_eq!(
            plan_shims(managed_root, vec![plain, suffixed], Platform::Unix)
                .unwrap()
                .entries
                .len(),
            2
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_bin_targets_are_skipped_and_non_regular_targets_are_rejected() {
        let root = test_root("files");
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
        let root = test_root("link");
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
