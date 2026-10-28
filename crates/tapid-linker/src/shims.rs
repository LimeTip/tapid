use std::{
    collections::BTreeSet,
    path::{Component, PathBuf},
};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use crate::{ManagedRoot, PlanError, Platform};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimPackage {
    pub tree_root: PathBuf,
    pub package_json: String,
    pub bin_dir: PathBuf,
}

/// A workspace package whose validated bin target is exposed through its staged local link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceShimPackage {
    /// Canonical package root beneath the project workspace root.
    pub tree_root: PathBuf,
    /// Manifest bytes parsed during planning.
    pub package_json: String,
    /// Staged `node_modules/<package>` path used as the shim's stable relative source.
    pub link_root: PathBuf,
    /// Managed `.bin` placement, normally the root `node_modules` directory.
    pub bin_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShimStrategy {
    UnixSymlink,
    WindowsCmdAndPowerShell,
}

impl ShimStrategy {
    /// Validate generated output identities without reading bin targets or
    /// selecting interpreters. Uses the same collision rules as shim planning.
    pub fn validate_targets<'a>(
        self,
        targets: impl IntoIterator<Item = &'a std::path::Path>,
    ) -> Result<(), PlanError> {
        let mut outputs = ShimOutputs::default();
        for target in targets {
            reserve_shim_outputs(&mut outputs, target, self)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShimInterpreter {
    Direct,
    Node,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimEntry {
    pub command: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub strategy: ShimStrategy,
    pub interpreter: ShimInterpreter,
}

fn shim_interpreter(
    source: &std::path::Path,
    platform: Platform,
) -> Result<ShimInterpreter, PlanError> {
    use std::io::Read;
    if platform != Platform::Windows {
        return Ok(ShimInterpreter::Direct);
    }
    let mut prefix = Vec::new();
    std::fs::File::open(source)
        .and_then(|file| file.take(512).read_to_end(&mut prefix))
        .map_err(|_| PlanError::BinTargetMissing(source.to_owned()))?;
    if !prefix.starts_with(b"#!") {
        return Ok(ShimInterpreter::Direct);
    }
    if prefix.len() == 512 && !prefix.contains(&b'\n') {
        return Err(PlanError::InvalidPackageMetadata(format!(
            "Windows bin shebang exceeds the inspection limit: {}",
            source.display()
        )));
    }
    let line = prefix
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let words = std::str::from_utf8(&line[2..])
        .unwrap_or_default()
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    match words.as_slice() {
        ["node" | "node.exe" | "/usr/bin/node" | "/usr/bin/node.exe"]
        | ["/usr/bin/env", "node" | "node.exe"] => Ok(ShimInterpreter::Node),
        _ => Err(PlanError::InvalidPackageMetadata(format!(
            "unsupported Windows bin interpreter: {}",
            source.display()
        ))),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShimPlan {
    pub managed_root: ManagedRoot,
    pub entries: Vec<ShimEntry>,
}

/// Plan all package-bin outputs before materialization writes any shim.
///
/// Windows identity uses the generated `.cmd`/`.ps1` paths, with non-expanding
/// BMP uppercase mapping over UTF-16 units. This is a deterministic collision
/// guard, not an exact model of every filesystem's case table or alias rules.
/// Unix targets retain exact `Path` equality, including non-UTF-8 names.
pub fn plan_shims(
    managed_root: ManagedRoot,
    packages: Vec<ShimPackage>,
    platform: Platform,
) -> Result<ShimPlan, PlanError> {
    let mut accumulator = ShimAccumulator::new(platform)?;
    populate_registry(&managed_root, packages, platform, &mut accumulator)?;
    Ok(accumulator.finish(managed_root))
}

struct ShimAccumulator {
    strategy: ShimStrategy,
    entries: Vec<ShimEntry>,
    outputs: ShimOutputs,
}

impl ShimAccumulator {
    fn new(platform: Platform) -> Result<Self, PlanError> {
        let strategy = match platform {
            Platform::Unix => ShimStrategy::UnixSymlink,
            Platform::Windows => ShimStrategy::WindowsCmdAndPowerShell,
            Platform::Other => return Err(PlanError::UnsupportedPlatform(platform)),
        };
        Ok(Self {
            strategy,
            entries: Vec::new(),
            outputs: ShimOutputs::default(),
        })
    }

    fn insert(
        &mut self,
        command: &str,
        source: PathBuf,
        target: PathBuf,
        inspection_source: &std::path::Path,
        platform: Platform,
    ) -> Result<(), PlanError> {
        reserve_shim_outputs(&mut self.outputs, &target, self.strategy)?;
        self.entries.push(ShimEntry {
            interpreter: shim_interpreter(inspection_source, platform)?,
            command: command.to_owned(),
            source,
            target,
            strategy: self.strategy,
        });
        Ok(())
    }

    fn finish(mut self, managed_root: ManagedRoot) -> ShimPlan {
        self.entries
            .sort_by(|a, b| a.target.cmp(&b.target).then(a.source.cmp(&b.source)));
        ShimPlan {
            managed_root,
            entries: self.entries,
        }
    }
}

fn populate_registry(
    managed_root: &ManagedRoot,
    packages: Vec<ShimPackage>,
    platform: Platform,
    accumulator: &mut ShimAccumulator,
) -> Result<(), PlanError> {
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
        let manifest =
            tapid_manifest::PackageManifest::parse(&package.package_json).map_err(|error| {
                PlanError::InvalidPackageMetadata(format!(
                    "{}: {error}",
                    package.tree_root.display()
                ))
            })?;
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
            accumulator.insert(
                &target.command,
                source.clone(),
                target_path,
                &source,
                platform,
            )?;
        }
    }
    Ok(())
}

/// Plans registry shims and workspace shims together so command collisions are
/// rejected before materialization. Workspace shim sources point through their
/// staged package links, so relative targets remain valid after activation.
pub fn plan_shims_with_workspace_packages(
    managed_root: ManagedRoot,
    packages: Vec<ShimPackage>,
    workspace_root: &std::path::Path,
    workspace_packages: Vec<WorkspaceShimPackage>,
    platform: Platform,
) -> Result<ShimPlan, PlanError> {
    let mut accumulator = ShimAccumulator::new(platform)?;
    populate_registry(&managed_root, packages, platform, &mut accumulator)?;
    populate_workspace(
        &managed_root,
        workspace_root,
        workspace_packages,
        platform,
        &mut accumulator,
    )?;
    Ok(accumulator.finish(managed_root))
}

fn populate_workspace(
    managed_root: &ManagedRoot,
    workspace_root: &std::path::Path,
    workspace_packages: Vec<WorkspaceShimPackage>,
    platform: Platform,
    accumulator: &mut ShimAccumulator,
) -> Result<(), PlanError> {
    if !workspace_root.is_absolute()
        || workspace_root
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PlanError::PathOutsideManagedRoot(
            workspace_root.to_path_buf(),
        ));
    }
    for package in workspace_packages {
        if !package.tree_root.is_absolute()
            || package
                .tree_root
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            || package.tree_root == workspace_root
            || !package.tree_root.starts_with(workspace_root)
        {
            return Err(PlanError::PathOutsideManagedRoot(package.tree_root));
        }
        if !managed_root.contains(&package.link_root) || !managed_root.contains(&package.bin_dir) {
            return Err(PlanError::PathOutsideManagedRoot(package.link_root));
        }
        let manifest =
            tapid_manifest::PackageManifest::parse(&package.package_json).map_err(|error| {
                PlanError::InvalidPackageMetadata(format!(
                    "{}: {error}",
                    package.tree_root.display()
                ))
            })?;
        let Some(bin) = manifest.bin() else { continue };
        for target in bin.targets() {
            let validated_source = package.tree_root.join(&target.target);
            if !validated_source.starts_with(&package.tree_root)
                || validated_source
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
                || !validated_source.starts_with(workspace_root)
            {
                return Err(PlanError::PathOutsideManagedRoot(validated_source));
            }
            let metadata = match std::fs::symlink_metadata(&validated_source) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(PlanError::BinTargetMissing(validated_source.clone())),
            };
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(PlanError::BinTargetNotRegular(validated_source));
            }
            let canonical_source = std::fs::canonicalize(&validated_source)
                .map_err(|_| PlanError::BinTargetMissing(validated_source.clone()))?;
            if !canonical_source.starts_with(&package.tree_root)
                || !canonical_source.starts_with(workspace_root)
            {
                return Err(PlanError::PathOutsideManagedRoot(canonical_source));
            }
            #[cfg(unix)]
            if platform == Platform::Unix {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    return Err(PlanError::InvalidPackageMetadata(format!(
                        "workspace bin target must already be executable: {}",
                        validated_source.display()
                    )));
                }
            }
            let source = package.link_root.join(&target.target);
            let target_path = package.bin_dir.join(".bin").join(&target.command);
            if !managed_root.contains(&source) || !managed_root.contains(&target_path) {
                return Err(PlanError::PathOutsideManagedRoot(source));
            }
            accumulator.insert(
                &target.command,
                source,
                target_path,
                &validated_source,
                platform,
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ShimKey {
    Bytes(Vec<u8>),
    Utf16(Vec<u16>),
}

#[derive(Default)]
struct ShimOutputs {
    keys: BTreeSet<ShimKey>,
    #[cfg(test)]
    generated: usize,
    #[cfg(test)]
    attempts: usize,
}

fn reserve_shim_outputs(
    outputs: &mut ShimOutputs,
    target: &std::path::Path,
    strategy: ShimStrategy,
) -> Result<(), PlanError> {
    let keys = shim_output_keys(target, strategy);
    #[cfg(test)]
    {
        outputs.generated += keys.len();
    }
    if keys.into_iter().any(|key| {
        #[cfg(test)]
        {
            outputs.attempts += 1;
        }
        !outputs.keys.insert(key)
    }) {
        return Err(PlanError::ShimCollision(target.to_owned()));
    }
    Ok(())
}

fn shim_output_keys(path: &std::path::Path, strategy: ShimStrategy) -> Vec<ShimKey> {
    match strategy {
        ShimStrategy::WindowsCmdAndPowerShell => ["cmd", "ps1"]
            .into_iter()
            .map(|extension| shim_target_key(&path.with_extension(extension), strategy))
            .collect(),
        ShimStrategy::UnixSymlink => vec![shim_target_key(path, strategy)],
    }
}

fn shim_target_key_from_utf16(units: Vec<u16>) -> ShimKey {
    ShimKey::Utf16(
        units
            .into_iter()
            .map(|unit| {
                let Some(character) = char::from_u32(unit as u32) else {
                    return unit;
                };
                // Preserve raw surrogate units; only one-to-one BMP uppercase
                // mappings participate. Rust's Unicode table is not a queried
                // Windows volume upcase table, so this is not universal NTFS identity.
                let mut uppercase = character.to_uppercase();
                match (uppercase.next(), uppercase.next()) {
                    (Some(mapped), None) if (mapped as u32) <= u16::MAX as u32 => mapped as u16,
                    _ => unit,
                }
            })
            .collect(),
    )
}

fn shim_target_key(path: &std::path::Path, strategy: ShimStrategy) -> ShimKey {
    match strategy {
        ShimStrategy::WindowsCmdAndPowerShell => {
            #[cfg(windows)]
            let units: Vec<u16> = path.as_os_str().encode_wide().collect();
            #[cfg(not(windows))]
            let units: Vec<u16> = path.to_string_lossy().encode_utf16().collect();

            shim_target_key_from_utf16(units)
        }
        ShimStrategy::UnixSymlink => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                ShimKey::Bytes(path.as_os_str().as_bytes().to_vec())
            }
            #[cfg(not(unix))]
            {
                ShimKey::Bytes(path.to_string_lossy().as_bytes().to_vec())
            }
        }
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

    #[cfg(unix)]
    #[test]
    fn unix_planner_retains_non_utf8_placement_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let fixture = tapid_test_support::TempProject::new("native-shim-paths").unwrap();
        let root = fixture.path();
        let mut first = shim_package(root, "first", r#""cli.js""#, "first");
        let mut second = first.clone();
        first.bin_dir = root.join(OsString::from_vec(b"node-\xff".to_vec()));
        second.bin_dir = root.join(OsString::from_vec(b"node-\xfe".to_vec()));
        let plan = plan_shims(
            ManagedRoot::new(root).unwrap(),
            vec![first.clone(), second.clone()],
            Platform::Unix,
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert_ne!(plan.entries[0].target, plan.entries[1].target);
        assert!(
            plan.entries
                .iter()
                .any(|entry| entry.target == first.bin_dir.join(".bin/first"))
        );
        assert!(
            plan.entries
                .iter()
                .any(|entry| entry.target == second.bin_dir.join(".bin/first"))
        );
        assert!(matches!(
            plan_shims(
                ManagedRoot::new(root).unwrap(),
                vec![first.clone(), first],
                Platform::Unix
            ),
            Err(PlanError::ShimCollision(_))
        ));
    }

    #[test]
    fn collision_precedes_workspace_interpreter_but_follows_source_validation() {
        let fixture = tapid_test_support::TempProject::new("shim-error-order").unwrap();
        let root = std::fs::canonicalize(fixture.path()).unwrap();
        let registry = shim_package(&root, "registry", r#"{"tool":"cli.js"}"#, "registry");
        let local = shim_package(&root, "local", r#"{"tool":"cli.js"}"#, "workspace/local");
        std::fs::write(local.tree_root.join("cli.js"), "#!/bin/sh\n").unwrap();
        let workspace = WorkspaceShimPackage {
            tree_root: local.tree_root,
            package_json: local.package_json,
            link_root: root.join("node_modules/local"),
            bin_dir: registry.bin_dir.clone(),
        };
        let plan = |registry_packages| {
            plan_shims_with_workspace_packages(
                ManagedRoot::new(&root).unwrap(),
                registry_packages,
                &root.join("workspace"),
                vec![workspace.clone()],
                Platform::Windows,
            )
        };
        assert!(matches!(
            plan(vec![registry.clone()]),
            Err(PlanError::ShimCollision(_))
        ));
        assert!(matches!(
            plan(vec![]),
            Err(PlanError::InvalidPackageMetadata(_))
        ));
        std::fs::remove_file(workspace.tree_root.join("cli.js")).unwrap();
        std::fs::create_dir(workspace.tree_root.join("cli.js")).unwrap();
        assert!(matches!(
            plan(vec![registry]),
            Err(PlanError::BinTargetNotRegular(_))
        ));
    }

    #[test]
    fn large_mixed_fixture_measures_indexed_work() {
        let fixture = tapid_test_support::TempProject::new("large-shims").unwrap();
        let root = std::fs::canonicalize(fixture.path()).unwrap();
        let bins = |prefix: &str| {
            let mappings = (0..5000)
                .map(|i| format!(r#""{prefix}{i:04}":"cli.js""#))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{mappings}}}")
        };
        let registry = shim_package(&root, "registry", &bins("r"), "registry");
        let local = shim_package(&root, "local", &bins("w"), "workspace/local");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                local.tree_root.join("cli.js"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let workspace = WorkspaceShimPackage {
            tree_root: local.tree_root,
            package_json: local.package_json,
            link_root: root.join("node_modules/local"),
            bin_dir: registry.bin_dir.clone(),
        };
        for platform in [Platform::Unix, Platform::Windows] {
            let managed = ManagedRoot::new(&root).unwrap();
            let mut accumulator = ShimAccumulator::new(platform).unwrap();
            populate_registry(&managed, vec![registry.clone()], platform, &mut accumulator)
                .unwrap();
            let width = if platform == Platform::Unix { 1 } else { 2 };
            assert_eq!(accumulator.outputs.generated, 5000 * width);
            populate_workspace(
                &managed,
                &root.join("workspace"),
                vec![workspace.clone()],
                platform,
                &mut accumulator,
            )
            .unwrap();
            assert_eq!(accumulator.outputs.generated, 10000 * width);
            assert_eq!(accumulator.outputs.attempts, 10000 * width);
            let keys = accumulator
                .entries
                .iter()
                .map(|entry| shim_output_keys(&entry.target, accumulator.strategy))
                .collect::<Vec<_>>();
            let mut comparisons = 0usize;
            for (i, current) in keys.iter().enumerate() {
                for previous in &keys[..i] {
                    comparisons += 1;
                    assert!(!current.iter().any(|key| previous.contains(key)));
                }
            }
            assert_eq!(comparisons, 49_995_000);
            let mut duplicate = workspace.clone();
            duplicate.package_json =
                r#"{"name":"duplicate","version":"1.0.0","bin":{"r0000":"cli.js"}}"#.to_owned();
            assert!(matches!(
                populate_workspace(
                    &managed,
                    &root.join("workspace"),
                    vec![duplicate],
                    platform,
                    &mut accumulator
                ),
                Err(PlanError::ShimCollision(_))
            ));
            assert_eq!(accumulator.outputs.generated, 10001 * width);
            assert_eq!(accumulator.outputs.attempts, 10000 * width + 1);
            let duplicate_keys =
                shim_output_keys(&accumulator.entries[0].target, accumulator.strategy);
            let mut late_comparisons = 0;
            let collision = keys.iter().any(|previous| {
                late_comparisons += 1;
                duplicate_keys.iter().any(|key| previous.contains(key))
            });
            assert!(collision);
            assert_eq!(late_comparisons, 1);
            eprintln!(
                "{platform:?}: keys={}, reservations={}, baseline={}, late_baseline={}",
                accumulator.outputs.generated,
                accumulator.outputs.attempts,
                comparisons,
                comparisons + late_comparisons
            );
        }
    }

    #[test]
    fn mixed_source_collision_identity_and_ordering() {
        let fixture = tapid_test_support::TempProject::new("mixed-shims").unwrap();
        let root = std::fs::canonicalize(fixture.path()).unwrap();
        for (left, right, windows_collision) in [
            ("tool", "tool", true),
            ("Tool", "tool", true),
            ("Ä", "ä", true),
            ("ς", "σ", true),
            ("tool", "tool.cmd", true),
            ("tool.cmd", "tool.ps1", true),
            ("ß", "SS", false),
            ("𐐀", "𐐨", false),
        ] {
            let registry = shim_package(
                &root,
                "registry",
                &format!(r#"{{"{left}":"cli.js"}}"#),
                "registry",
            );
            let local = shim_package(
                &root,
                "local",
                &format!(r#"{{"{right}":"cli.js"}}"#),
                "workspace/local",
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    local.tree_root.join("cli.js"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
            let workspace = WorkspaceShimPackage {
                tree_root: local.tree_root,
                package_json: local.package_json,
                link_root: root.join("node_modules/local"),
                bin_dir: registry.bin_dir.clone(),
            };
            for platform in [Platform::Unix, Platform::Windows] {
                let result = plan_shims_with_workspace_packages(
                    ManagedRoot::new(&root).unwrap(),
                    vec![registry.clone()],
                    &root.join("workspace"),
                    vec![workspace.clone()],
                    platform,
                );
                let collision =
                    left == right || (platform == Platform::Windows && windows_collision);
                if collision {
                    assert!(
                        matches!(result, Err(PlanError::ShimCollision(_))),
                        "{left}/{right}: {result:?}"
                    );
                } else {
                    assert_eq!(result.unwrap().entries.len(), 2);
                }
                let mut separate = workspace.clone();
                separate.bin_dir = root.join("nested/node_modules");
                let first = plan_shims_with_workspace_packages(
                    ManagedRoot::new(&root).unwrap(),
                    vec![registry.clone()],
                    &root.join("workspace"),
                    vec![separate.clone()],
                    platform,
                )
                .unwrap();
                assert_eq!(first.entries.len(), 2);
                assert!(
                    first
                        .entries
                        .iter()
                        .any(|entry| entry.source == separate.link_root.join("cli.js"))
                );
                let mut other_registry = registry.clone();
                other_registry.bin_dir = root.join("other/node_modules");
                let mut other_workspace = separate.clone();
                other_workspace.bin_dir = root.join("another/node_modules");
                let plan = |registries, workspaces| {
                    plan_shims_with_workspace_packages(
                        ManagedRoot::new(&root).unwrap(),
                        registries,
                        &root.join("workspace"),
                        workspaces,
                        platform,
                    )
                    .unwrap()
                };
                assert_eq!(
                    plan(
                        vec![registry.clone(), other_registry.clone()],
                        vec![separate.clone(), other_workspace.clone()]
                    ),
                    plan(
                        vec![other_registry, registry.clone()],
                        vec![other_workspace, separate]
                    )
                );
            }
        }
    }

    #[test]
    fn windows_bin_interpreters_are_bounded_and_do_not_execute_shebang_arguments() {
        let root = test_root("interpreters");
        std::fs::create_dir_all(&root).unwrap();
        let package = shim_package(&root, "tool", r#""cli.js""#, "package");
        for (prefix, expected) in [
            (
                b"#!/usr/bin/env node\n".as_slice(),
                Some(ShimInterpreter::Node),
            ),
            (
                b"#! /usr/bin/node\r\n".as_slice(),
                Some(ShimInterpreter::Node),
            ),
            (
                b"MZ native executable".as_slice(),
                Some(ShimInterpreter::Direct),
            ),
            (b"#!/usr/bin/env node --inspect\n".as_slice(), None),
            (b"#!/usr/bin/env node&evil\n".as_slice(), None),
            (b"#!/bin/sh\n".as_slice(), None),
        ] {
            std::fs::write(package.tree_root.join("cli.js"), prefix).unwrap();
            let result = plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![package.clone()],
                Platform::Windows,
            );
            match expected {
                Some(interpreter) => {
                    assert_eq!(result.unwrap().entries[0].interpreter, interpreter)
                }
                None => assert!(matches!(result, Err(PlanError::InvalidPackageMetadata(_)))),
            }
            assert!(!package.bin_dir.join(".bin").exists());
        }
        std::fs::write(
            package.tree_root.join("cli.js"),
            [b"#!".as_slice(), &vec![b' '; 1024], b"node\n"].concat(),
        )
        .unwrap();
        assert!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![package.clone()],
                Platform::Windows
            )
            .is_err()
        );
        std::fs::write(
            package.tree_root.join("cli.js"),
            [b"#!node".as_slice(), &vec![b' '; 1024], b"--inspect\n"].concat(),
        )
        .unwrap();
        assert!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![package.clone()],
                Platform::Windows
            )
            .is_err(),
            "arguments beyond the bounded prefix must not be accepted"
        );
        assert_eq!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![package],
                Platform::Unix
            )
            .unwrap()
            .entries[0]
                .interpreter,
            ShimInterpreter::Direct
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_package_metadata_error_identifies_package_directory() {
        let root = test_root("invalid-metadata");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let invalid = ShimPackage {
            tree_root: root.join("bad-package"),
            package_json: r#"{"name":"typescript@*","version":"1.0.0"}"#.to_owned(),
            bin_dir: root.join("node_modules"),
        };

        let error = plan_shims(
            ManagedRoot::new(&root).unwrap(),
            vec![invalid],
            Platform::Unix,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("bad-package"), "{error}");
        assert!(error.contains("typescript@*"), "{error}");
        let _ = std::fs::remove_dir_all(root);
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
            shim_target_key(Path::new("𐐀"), ShimStrategy::WindowsCmdAndPowerShell),
            shim_target_key(Path::new("𐐨"), ShimStrategy::WindowsCmdAndPowerShell)
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

    #[test]
    fn windows_shim_collisions_preserve_unpaired_utf16_units() {
        let first = shim_target_key_from_utf16(vec![110, 0xd800, 97]);
        let second = shim_target_key_from_utf16(vec![110, 0xd801, 97]);
        assert_ne!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn unix_shim_collisions_preserve_non_utf8_path_identity() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let first = PathBuf::from(OsString::from_vec(b"node-\xff/.bin/tool".to_vec()));
        let second = PathBuf::from(OsString::from_vec(b"node-\xfe/.bin/tool".to_vec()));
        assert_ne!(
            shim_output_keys(&first, ShimStrategy::UnixSymlink),
            shim_output_keys(&second, ShimStrategy::UnixSymlink),
        );
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
