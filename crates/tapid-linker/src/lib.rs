//! Pure, deterministic planning for project dependency materialization.
//!
//! The planner describes filesystem work; it deliberately does not create
//! links, mutate directories, execute processes, or claim to provide an OS
//! sandbox. Policy and runner crates consume the plan through the small types
//! in this crate's contract seam.

#![deny(unsafe_code)]

use std::{
    fmt,
    path::{Component, Path, PathBuf},
};
use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
            let file_type = std::fs::symlink_metadata(&source)
                .map_err(|_| PlanError::BinTargetMissing(source.clone()))?
                .file_type();
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedTreeReference {
    pub digest: String,
    pub root: PathBuf,
}

impl VerifiedTreeReference {
    pub fn new(digest: &str, root: impl Into<PathBuf>) -> Result<Self, PlanError> {
        if !digest.starts_with("sha256-")
            || digest.len() != 71
            || !digest[7..].chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(PlanError::InvalidTreeDigest(digest.to_owned()));
        }
        let root = root.into();
        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err(PlanError::TreeRootMustBeAbsolute(root));
        }
        Ok(Self {
            digest: digest.to_ascii_lowercase(),
            root,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedRoot {
    pub path: PathBuf,
    pub ownership_marker: PathBuf,
}

impl ManagedRoot {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, PlanError> {
        let path = path.into();
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(PlanError::InvalidManagedRoot(path));
        }
        Ok(Self {
            ownership_marker: path.join(".tapid-managed"),
            path,
        })
    }

    pub fn contains(&self, candidate: &Path) -> bool {
        candidate.is_absolute()
            && !candidate
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            && candidate.strip_prefix(&self.path).is_ok()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageInstance {
    pub id: PackageInstanceId,
    pub peer_context: PeerContext,
    pub platform_context: PlatformContext,
    pub tree: VerifiedTreeReference,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstanceKey {
    pub id: PackageInstanceId,
    pub peer_context: PeerContext,
    pub platform_context: PlatformContext,
}

impl From<&PackageInstance> for InstanceKey {
    fn from(instance: &PackageInstance) -> Self {
        Self {
            id: instance.id.clone(),
            peer_context: instance.peer_context.clone(),
            platform_context: instance.platform_context.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyEdge {
    pub parent: InstanceKey,
    pub child: InstanceKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutInput {
    pub instances: Vec<PackageInstance>,
    pub root_dependencies: Vec<InstanceKey>,
    pub dependency_edges: Vec<DependencyEdge>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationInput {
    pub instances: Vec<PackageInstance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationEntry {
    pub instance: PackageInstanceId,
    pub peer_context: String,
    pub platform_context: String,
    pub source: PathBuf,
    pub target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkKind {
    Symlink,
    Junction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationStep {
    pub kind: LinkKind,
    pub source: PathBuf,
    pub target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct StagedActivationPlan {
    pub steps: Vec<ActivationStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationPlan {
    pub managed_root: ManagedRoot,
    pub ownership_marker: PathBuf,
    pub entries: Vec<MaterializationEntry>,
    pub activation: StagedActivationPlan,
}

pub fn plan_materialization(
    root: ManagedRoot,
    input: MaterializationInput,
) -> Result<MaterializationPlan, PlanError> {
    let mut instances = input.instances;
    instances.sort_by(|a, b| {
        (
            &a.id,
            a.peer_context.to_string(),
            a.platform_context.to_string(),
        )
            .cmp(&(
                &b.id,
                b.peer_context.to_string(),
                b.platform_context.to_string(),
            ))
    });
    for pair in instances.windows(2) {
        if pair[0].id == pair[1].id
            && pair[0].peer_context == pair[1].peer_context
            && pair[0].platform_context == pair[1].platform_context
        {
            return Err(PlanError::DuplicateInstance(pair[0].id.clone()));
        }
    }
    let mut entries = Vec::with_capacity(instances.len());
    let mut activation = Vec::with_capacity(instances.len());
    for instance in instances {
        let suffix = context_suffix(&instance.peer_context, &instance.platform_context);
        let name = safe_component(instance.id.name.as_str());
        let target = root
            .path
            .join(".tapid")
            .join("instances")
            .join(&name)
            .join(instance.id.version.to_string())
            .join(suffix);
        if !root.contains(&target) {
            return Err(PlanError::PathOutsideManagedRoot(target));
        }
        entries.push(MaterializationEntry {
            instance: instance.id.clone(),
            peer_context: instance.peer_context.to_string(),
            platform_context: instance.platform_context.to_string(),
            source: instance.tree.root.clone(),
            target: target.clone(),
        });
        activation.push(ActivationStep {
            kind: LinkKind::Symlink,
            source: target.clone(),
            target: root.path.join("node_modules").join(name),
        });
    }
    Ok(MaterializationPlan {
        ownership_marker: root.ownership_marker.clone(),
        managed_root: root,
        entries,
        activation: StagedActivationPlan { steps: activation },
    })
}

pub fn plan_layout(
    root: ManagedRoot,
    input: LayoutInput,
    platform: Platform,
) -> Result<MaterializationPlan, PlanError> {
    let kind = match platform {
        Platform::Unix => LinkKind::Symlink,
        Platform::Windows => LinkKind::Junction,
        Platform::Other => return Err(PlanError::UnsupportedPlatform(platform)),
    };
    let mut instances = input.instances;
    instances.sort_by_key(|instance| InstanceKey::from(instance));
    let mut by_key = std::collections::BTreeMap::new();
    for instance in &instances {
        let key = InstanceKey::from(instance);
        if by_key.insert(key.clone(), instance).is_some() {
            return Err(PlanError::DuplicateInstance(key.id));
        }
    }
    let mut entries = Vec::with_capacity(instances.len());
    let mut storage = std::collections::BTreeMap::new();
    for instance in &instances {
        let key = InstanceKey::from(instance);
        let path = root
            .path
            .join(".tapid")
            .join("instances")
            .join(safe_component(instance.id.name.as_str()))
            .join(instance.id.version.to_string())
            .join(context_suffix(
                &instance.peer_context,
                &instance.platform_context,
            ));
        if !root.contains(&path) {
            return Err(PlanError::PathOutsideManagedRoot(path));
        }
        storage.insert(key.clone(), path.clone());
        entries.push(MaterializationEntry {
            instance: key.id,
            peer_context: key.peer_context.to_string(),
            platform_context: key.platform_context.to_string(),
            source: instance.tree.root.clone(),
            target: path,
        });
    }
    let mut locations: std::collections::BTreeMap<InstanceKey, PathBuf> =
        std::collections::BTreeMap::new();
    let mut requests = Vec::new();
    let mut roots = input.root_dependencies;
    roots.sort();
    for child in roots {
        if !by_key.contains_key(&child) {
            return Err(PlanError::UnknownInstance(child.id));
        }
        requests.push((None, child));
    }
    let mut edges = input.dependency_edges;
    edges.sort_by(|a, b| {
        (a.parent.clone(), a.child.clone()).cmp(&(b.parent.clone(), b.child.clone()))
    });
    for edge in edges {
        if !by_key.contains_key(&edge.parent) {
            return Err(PlanError::UnknownInstance(edge.parent.id));
        }
        if !by_key.contains_key(&edge.child) {
            return Err(PlanError::UnknownInstance(edge.child.id));
        }
        requests.push((Some(edge.parent), edge.child));
    }
    let mut activation = Vec::new();
    for (parent, child) in requests {
        let base = match parent {
            None => root.path.join("node_modules"),
            Some(parent) => locations
                .get(&parent)
                .cloned()
                .ok_or(PlanError::UnknownParent(parent.id))?
                .join("node_modules"),
        };
        let target = base.join(child.id.name.as_str().split('/').collect::<PathBuf>());
        if !root.contains(&target) {
            return Err(PlanError::PathOutsideManagedRoot(target));
        }
        if let Some(existing) = activation
            .iter()
            .find(|step: &&ActivationStep| step.target == target)
        {
            if existing.source != storage[&child] {
                return Err(PlanError::ConflictingTarget(target));
            }
            continue;
        }
        locations.insert(child.clone(), target.clone());
        activation.push(ActivationStep {
            kind: kind.clone(),
            source: storage[&child].clone(),
            target,
        });
    }
    activation.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(MaterializationPlan {
        ownership_marker: root.ownership_marker.clone(),
        managed_root: root,
        entries,
        activation: StagedActivationPlan { steps: activation },
    })
}

fn context_suffix(peer: &PeerContext, platform: &PlatformContext) -> String {
    let peer = if peer.to_string().is_empty() {
        "no-peer".to_owned()
    } else {
        safe_component(&peer.to_string())
    };
    let platform = if platform.to_string().is_empty() {
        "no-platform".to_owned()
    } else {
        safe_component(&platform.to_string())
    };
    format!("peer={peer}__platform={platform}")
}
fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Unix,
    Windows,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformCapabilities {
    pub platform: Platform,
    pub symlink: Capability,
    pub junction: Capability,
    pub process_sandbox: Capability,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Capability {
    Supported,
    Unsupported,
}

impl PlatformCapabilities {
    pub fn for_platform(platform: Platform) -> Self {
        match platform {
            Platform::Unix => Self {
                platform,
                symlink: Capability::Supported,
                junction: Capability::Unsupported,
                process_sandbox: Capability::Unsupported,
                limitations: vec![
                    "This crate plans links only; it does not enforce process sandboxing.".into(),
                ],
            },
            Platform::Windows => Self {
                platform,
                symlink: Capability::Supported,
                junction: Capability::Supported,
                process_sandbox: Capability::Unsupported,
                limitations: vec![
                    "Link mutation and process sandboxing are outside this planning crate.".into(),
                ],
            },
            Platform::Other => Self {
                platform,
                symlink: Capability::Unsupported,
                junction: Capability::Unsupported,
                process_sandbox: Capability::Unsupported,
                limitations: vec![
                    "This platform has no supported link strategy in this release.".into(),
                    "Process sandboxing is not provided.".into(),
                ],
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    InvalidTreeDigest(String),
    TreeRootMustBeAbsolute(PathBuf),
    InvalidManagedRoot(PathBuf),
    PathOutsideManagedRoot(PathBuf),
    DuplicateInstance(PackageInstanceId),
    UnknownInstance(PackageInstanceId),
    UnknownParent(PackageInstanceId),
    ConflictingTarget(PathBuf),
    UnsupportedPlatform(Platform),
    InvalidPackageMetadata(String),
    BinTargetMissing(PathBuf),
    BinTargetNotRegular(PathBuf),
    ShimCollision(PathBuf),
}
impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTreeDigest(v) => write!(f, "invalid verified tree digest: {v}"),
            Self::TreeRootMustBeAbsolute(v) => {
                write!(f, "verified tree root must be absolute: {}", v.display())
            }
            Self::InvalidManagedRoot(v) => write!(f, "invalid managed root: {}", v.display()),
            Self::PathOutsideManagedRoot(v) => {
                write!(f, "planned path escapes managed root: {}", v.display())
            }
            Self::DuplicateInstance(v) => write!(f, "duplicate package instance: {v:?}"),
            Self::UnknownInstance(v) => write!(f, "unknown package instance: {v:?}"),
            Self::UnknownParent(v) => write!(f, "dependency parent was not placed: {v:?}"),
            Self::ConflictingTarget(v) => {
                write!(f, "conflicting activation target: {}", v.display())
            }
            Self::UnsupportedPlatform(v) => write!(f, "unsupported linker platform: {v:?}"),
            Self::InvalidPackageMetadata(v) => write!(f, "invalid package metadata: {v}"),
            Self::BinTargetMissing(v) => {
                write!(f, "package bin target is missing: {}", v.display())
            }
            Self::BinTargetNotRegular(v) => write!(
                f,
                "package bin target is not a regular file: {}",
                v.display()
            ),
            Self::ShimCollision(v) => write!(f, "executable shim collision: {}", v.display()),
        }
    }
}
impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    fn instance(name: &str, version: &str, peer: PeerContext) -> PackageInstance {
        PackageInstance {
            id: PackageInstanceId::new(
                "https://registry.example".parse().unwrap(),
                name.parse().unwrap(),
                version.parse().unwrap(),
            ),
            peer_context: peer,
            platform_context: PlatformContext::new(Some("linux"), Some("x86_64"), Some("gnu"))
                .unwrap(),
            tree: VerifiedTreeReference::new(
                &format!("sha256-{}", "a".repeat(64)),
                "/tmp/store/tree",
            )
            .unwrap(),
        }
    }
    #[test]
    fn duplicate_versions_remain_distinct_instances() {
        let root = ManagedRoot::new("/tmp/project").unwrap();
        let plan = plan_materialization(
            root,
            MaterializationInput {
                instances: vec![
                    instance("dep", "1.0.0", PeerContext::default()),
                    instance("dep", "2.0.0", PeerContext::default()),
                ],
            },
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert_ne!(plan.entries[0].target, plan.entries[1].target);
    }
    #[test]
    fn peer_context_is_part_of_instance_target() {
        let mut peer = PeerContext::default();
        peer = peer.with("react".parse().unwrap(), "18.2.0".parse().unwrap());
        let plan = plan_materialization(
            ManagedRoot::new("/tmp/project").unwrap(),
            MaterializationInput {
                instances: vec![
                    instance("plugin", "1.0.0", PeerContext::default()),
                    instance("plugin", "1.0.0", peer),
                ],
            },
        )
        .unwrap();
        assert_ne!(plan.entries[0].target, plan.entries[1].target);
    }
    #[test]
    fn ordering_is_deterministic() {
        let a = instance("z", "1.0.0", PeerContext::default());
        let b = instance("a", "1.0.0", PeerContext::default());
        let p1 = plan_materialization(
            ManagedRoot::new("/tmp/project").unwrap(),
            MaterializationInput {
                instances: vec![a.clone(), b.clone()],
            },
        )
        .unwrap();
        let p2 = plan_materialization(
            ManagedRoot::new("/tmp/project").unwrap(),
            MaterializationInput {
                instances: vec![b, a],
            },
        )
        .unwrap();
        assert_eq!(p1, p2);
    }
    #[test]
    fn managed_paths_cannot_escape_root() {
        assert!(ManagedRoot::new("relative").is_err());
        let root = ManagedRoot::new("/tmp/project").unwrap();
        assert!(root.contains(Path::new("/tmp/project/.tapid/instances")));
        assert!(!root.contains(Path::new("/tmp/project/../else")));
    }
    #[test]
    fn unsupported_platform_reports_limitations() {
        let caps = PlatformCapabilities::for_platform(Platform::Other);
        assert_eq!(caps.symlink, Capability::Unsupported);
        assert_eq!(caps.process_sandbox, Capability::Unsupported);
        assert!(!caps.limitations.is_empty());
    }

    #[test]
    fn root_edges_and_nested_edges_get_distinct_node_modules_targets() {
        let app = instance("app", "1.0.0", PeerContext::default());
        let dep_v1 = instance("dep", "1.0.0", PeerContext::default());
        let dep_v2 = instance("dep", "2.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![app.clone(), dep_v1.clone(), dep_v2.clone()],
            root_dependencies: vec![key(&app), key(&dep_v1)],
            dependency_edges: vec![DependencyEdge {
                parent: key(&app),
                child: key(&dep_v2),
            }],
        };
        let plan = plan_layout(
            ManagedRoot::new("/tmp/project").unwrap(),
            input,
            Platform::Unix,
        )
        .unwrap();
        let targets: Vec<_> = plan
            .activation
            .steps
            .iter()
            .map(|step| step.target.clone())
            .collect();
        assert!(
            targets
                .iter()
                .any(|p| p == Path::new("/tmp/project/node_modules/dep"))
        );
        assert!(
            targets
                .iter()
                .any(|p| p == Path::new("/tmp/project/node_modules/app/node_modules/dep"))
        );
    }

    #[test]
    fn conflicting_edges_are_rejected_instead_of_overwriting_a_target() {
        let app = instance("app", "1.0.0", PeerContext::default());
        let first = instance("dep", "1.0.0", PeerContext::default());
        let second = instance("dep", "2.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![app.clone(), first.clone(), second.clone()],
            root_dependencies: vec![key(&app), key(&first), key(&second)],
            dependency_edges: vec![],
        };
        assert!(matches!(
            plan_layout(
                ManagedRoot::new("/tmp/project").unwrap(),
                input,
                Platform::Unix
            ),
            Err(PlanError::ConflictingTarget(_))
        ));
    }

    #[test]
    fn platform_strategy_is_explicit_and_other_platforms_are_unsupported() {
        let app = instance("app", "1.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![app.clone()],
            root_dependencies: vec![key(&app)],
            dependency_edges: vec![],
        };
        let windows = plan_layout(
            ManagedRoot::new("/tmp/project").unwrap(),
            input.clone(),
            Platform::Windows,
        )
        .unwrap();
        assert_eq!(windows.activation.steps[0].kind, LinkKind::Junction);
        assert!(matches!(
            plan_layout(
                ManagedRoot::new("/tmp/project").unwrap(),
                input,
                Platform::Other
            ),
            Err(PlanError::UnsupportedPlatform(Platform::Other))
        ));
    }

    fn key(package: &PackageInstance) -> InstanceKey {
        InstanceKey {
            id: package.id.clone(),
            peer_context: package.peer_context.clone(),
            platform_context: package.platform_context.clone(),
        }
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
    fn missing_and_non_regular_bin_targets_are_rejected() {
        let root = std::env::temp_dir().join(format!("tapid-shims-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let missing = shim_package(&root, "missing", r#""missing.js""#, "missing");
        std::fs::remove_file(missing.tree_root.join("cli.js")).unwrap();
        assert!(matches!(
            plan_shims(
                ManagedRoot::new(&root).unwrap(),
                vec![missing],
                Platform::Unix
            ),
            Err(PlanError::BinTargetMissing(_))
        ));
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
