use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};

use crate::Platform;

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
            platform_sort_key(&a.platform_context),
        )
            .cmp(&(
                &b.id,
                b.peer_context.to_string(),
                platform_sort_key(&b.platform_context),
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
        let name = package_name_path(instance.id.name.as_str());
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
        let activation_target = root.path.join("node_modules").join(name);
        if !root.contains(&activation_target) {
            return Err(PlanError::PathOutsideManagedRoot(activation_target));
        }
        if activation
            .iter()
            .any(|step: &ActivationStep| step.target == activation_target)
        {
            return Err(PlanError::ConflictingTarget(activation_target));
        }
        activation.push(ActivationStep {
            kind: LinkKind::Symlink,
            source: target.clone(),
            target: activation_target,
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
            .join(package_name_path(instance.id.name.as_str()))
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
    let mut children: std::collections::BTreeMap<InstanceKey, Vec<InstanceKey>> =
        std::collections::BTreeMap::new();
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
        children.entry(edge.parent).or_default().push(edge.child);
    }
    for selected in children.values_mut() {
        selected.sort();
        selected.dedup();
    }

    let root_base = root.path.join("node_modules");
    let mut roots = input.root_dependencies;
    roots.sort();
    roots.dedup();
    let mut pending = std::collections::VecDeque::new();
    let mut activation = Vec::new();
    let mut placements = std::collections::BTreeMap::new();
    let mut reachable = std::collections::BTreeSet::new();
    for child in roots {
        if !by_key.contains_key(&child) {
            return Err(PlanError::UnknownInstance(child.id));
        }
        let target = root_base.join(package_name_path(child.id.name.as_str()));
        if let Some(existing) = placements.insert(target.clone(), child.clone()) {
            if existing != child {
                return Err(PlanError::ConflictingTarget(target));
            }
            continue;
        }
        activation.push(ActivationStep {
            kind: kind.clone(),
            source: storage[&child].clone(),
            target: target.clone(),
        });
        reachable.insert(child.clone());
        let mut lineage = std::collections::BTreeSet::new();
        lineage.insert(child.clone());
        if let Some(selected) = children.get(&child) {
            for grandchild in selected {
                pending.push_back((
                    grandchild.clone(),
                    vec![root_base.clone(), target.join("node_modules")],
                    lineage.clone(),
                ));
            }
        }
    }

    while let Some((child, bases, lineage)) = pending.pop_front() {
        let closes_cycle = lineage.contains(&child);
        let mut already_resolves = false;
        let mut shadowed_at = None;
        for (index, base) in bases.iter().enumerate().rev() {
            let target = base.join(package_name_path(child.id.name.as_str()));
            if !root.contains(&target) {
                return Err(PlanError::PathOutsideManagedRoot(target));
            }
            if let Some(existing) = placements.get(&target) {
                already_resolves = existing == &child;
                if !already_resolves {
                    shadowed_at = Some(index);
                }
                break;
            }
        }
        if already_resolves {
            reachable.insert(child);
            continue;
        }
        let first_candidate = shadowed_at.map_or(0, |index| index + 1);
        let mut selected = None;
        for (index, base) in bases.iter().enumerate().skip(first_candidate) {
            let target = base.join(package_name_path(child.id.name.as_str()));
            if !placements.contains_key(&target) {
                selected = Some((index, target));
                break;
            }
        }
        let Some((index, target)) = selected else {
            return Err(PlanError::ConflictingTarget(
                bases
                    .last()
                    .expect("dependency search always has a direct parent base")
                    .join(package_name_path(child.id.name.as_str())),
            ));
        };
        placements.insert(target.clone(), child.clone());
        activation.push(ActivationStep {
            kind: kind.clone(),
            source: storage[&child].clone(),
            target: target.clone(),
        });
        reachable.insert(child.clone());
        if closes_cycle {
            continue;
        }
        let mut descendants = lineage;
        descendants.insert(child.clone());
        let mut descendant_bases = bases[..=index].to_vec();
        descendant_bases.push(target.join("node_modules"));
        if let Some(selected) = children.get(&child) {
            for grandchild in selected {
                pending.push_back((
                    grandchild.clone(),
                    descendant_bases.clone(),
                    descendants.clone(),
                ));
            }
        }
    }
    if let Some(parent) = children.keys().find(|parent| !reachable.contains(*parent)) {
        return Err(PlanError::UnknownParent(parent.id.clone()));
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
        platform_component_key(platform)
    };
    format!("peer={peer}__platform={platform}")
}

fn encode_path_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'@' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn platform_component_key(platform: &PlatformContext) -> String {
    format!(
        "os-{}--cpu-{}--libc-{}",
        encode_path_component(platform.os.as_deref().unwrap_or_default()),
        encode_path_component(platform.cpu.as_deref().unwrap_or_default()),
        encode_path_component(platform.libc.as_deref().unwrap_or_default()),
    )
}

fn platform_sort_key(platform: &PlatformContext) -> String {
    platform_component_key(platform)
}

fn package_name_path(value: &str) -> PathBuf {
    value.split('/').collect()
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

    fn test_project_root() -> PathBuf {
        std::env::temp_dir().join("tapid-linker-test-project")
    }

    fn test_tree_root() -> PathBuf {
        test_project_root().join("store").join("tree")
    }

    fn test_managed_root() -> ManagedRoot {
        ManagedRoot::new(test_project_root()).unwrap()
    }

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
                test_tree_root(),
            )
            .unwrap(),
        }
    }
    #[test]
    fn duplicate_activation_targets_are_rejected() {
        let root = test_managed_root();
        let plan = plan_materialization(
            root,
            MaterializationInput {
                instances: vec![
                    instance("dep", "1.0.0", PeerContext::default()),
                    instance("dep", "2.0.0", PeerContext::default()),
                ],
            },
        );
        assert!(matches!(plan, Err(PlanError::ConflictingTarget(_))));
    }
    #[test]
    fn peer_context_is_part_of_instance_target() {
        let mut peer = PeerContext::default();
        peer = peer.with("react".parse().unwrap(), "18.2.0".parse().unwrap());
        let default_plan = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![instance("plugin", "1.0.0", PeerContext::default())],
            },
        )
        .unwrap();
        let peer_plan = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![instance("plugin", "1.0.0", peer)],
            },
        )
        .unwrap();
        assert_ne!(default_plan.entries[0].target, peer_plan.entries[0].target);
    }
    #[test]
    fn hyphenated_platform_contexts_have_distinct_targets_and_lossless_order_keys() {
        let first_platform =
            PlatformContext::new(Some("linux-x"), Some("x86_64"), Some("gnu")).unwrap();
        let second_platform =
            PlatformContext::new(Some("linux"), Some("x-x86_64"), Some("gnu")).unwrap();
        assert_ne!(
            platform_sort_key(&first_platform),
            platform_sort_key(&second_platform)
        );
        let delimiter_platform =
            PlatformContext::new(Some("a;cpu=b"), Some("c"), Some("d")).unwrap();
        let escaped_delimiter_platform =
            PlatformContext::new(Some("a"), Some("b"), Some("c;cpu=d")).unwrap();
        assert_ne!(
            platform_sort_key(&delimiter_platform),
            platform_sort_key(&escaped_delimiter_platform)
        );

        let mut contexts = [second_platform.clone(), first_platform.clone()];
        contexts.sort_by_key(platform_sort_key);
        assert_eq!(contexts[0], first_platform);
        assert_eq!(contexts[1], second_platform);

        let mut first = instance("plugin-a", "1.0.0", PeerContext::default());
        first.platform_context = first_platform;
        let mut second = instance("plugin-b", "1.0.0", PeerContext::default());
        second.platform_context = second_platform;
        let p1 = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![first.clone(), second.clone()],
            },
        )
        .unwrap();
        let p2 = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![second, first],
            },
        )
        .unwrap();
        assert_ne!(p1.entries[0].target, p1.entries[1].target);
        assert_eq!(p1.entries, p2.entries);
        assert_eq!(p1.activation, p2.activation);
    }

    #[test]
    fn platform_field_boundaries_are_unambiguous_for_storage_and_sorting() {
        let first_platform = PlatformContext::new(Some("a;cpu=b"), Some("c"), Some("d")).unwrap();
        let second_platform = PlatformContext::new(Some("a"), Some("b;cpu=c"), Some("d")).unwrap();
        assert_ne!(
            platform_sort_key(&first_platform),
            platform_sort_key(&second_platform)
        );

        let mut first = instance("plugin", "1.0.0", PeerContext::default());
        first.platform_context = first_platform;
        let mut second = instance("plugin", "1.0.0", PeerContext::default());
        second.platform_context = second_platform;
        let plan = plan_layout(
            test_managed_root(),
            LayoutInput {
                instances: vec![first, second],
                root_dependencies: vec![],
                dependency_edges: vec![],
            },
            Platform::Unix,
        )
        .unwrap();
        assert_ne!(plan.entries[0].target, plan.entries[1].target);
    }

    #[test]
    fn ordering_is_deterministic() {
        let a = instance("z", "1.0.0", PeerContext::default());
        let b = instance("a", "1.0.0", PeerContext::default());
        let p1 = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![a.clone(), b.clone()],
            },
        )
        .unwrap();
        let p2 = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![b, a],
            },
        )
        .unwrap();
        assert_eq!(p1, p2);
    }
    #[test]
    fn scoped_package_materialization_preserves_name_segments() {
        let first = instance("@a/b_c", "1.0.0", PeerContext::default());
        let second = instance("@a_b/c", "1.0.0", PeerContext::default());
        let plan = plan_materialization(
            test_managed_root(),
            MaterializationInput {
                instances: vec![first, second],
            },
        )
        .unwrap();

        assert_ne!(plan.entries[0].target, plan.entries[1].target);
        let targets: Vec<_> = plan
            .activation
            .steps
            .iter()
            .map(|step| step.target.clone())
            .collect();
        assert!(
            targets.contains(
                &test_project_root()
                    .join("node_modules")
                    .join("@a")
                    .join("b_c")
            )
        );
        assert!(
            targets.contains(
                &test_project_root()
                    .join("node_modules")
                    .join("@a_b")
                    .join("c")
            )
        );
    }

    #[test]
    fn managed_paths_cannot_escape_root() {
        assert!(ManagedRoot::new("relative").is_err());
        let root = test_managed_root();
        assert!(root.contains(&test_project_root().join(".tapid").join("instances")));
        assert!(!root.contains(&test_project_root().join("..").join("else")));
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
        let plan = plan_layout(test_managed_root(), input, Platform::Unix).unwrap();
        let targets: Vec<_> = plan
            .activation
            .steps
            .iter()
            .map(|step| step.target.clone())
            .collect();
        assert!(targets.iter().any(|p| {
            p == test_project_root()
                .join("node_modules")
                .join("dep")
                .as_path()
        }));
        assert!(targets.iter().any(|p| {
            p == test_project_root()
                .join("node_modules")
                .join("app")
                .join("node_modules")
                .join("dep")
                .as_path()
        }));
    }

    #[test]
    fn transitive_dependency_hoists_to_outermost_unshadowed_node_modules() {
        let root_package = instance("app", "1.0.0", PeerContext::default());
        let parent = instance("engine", "1.0.0", PeerContext::default());
        let native = instance("engine-darwin-arm64", "1.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![root_package.clone(), parent.clone(), native.clone()],
            root_dependencies: vec![key(&root_package)],
            dependency_edges: vec![
                DependencyEdge {
                    parent: key(&root_package),
                    child: key(&parent),
                },
                DependencyEdge {
                    parent: key(&parent),
                    child: key(&native),
                },
            ],
        };

        let plan = plan_layout(test_managed_root(), input, Platform::Unix).unwrap();

        assert!(plan.activation.steps.iter().any(|step| {
            step.target
                == test_project_root()
                    .join("node_modules")
                    .join("engine-darwin-arm64")
        }));
    }

    #[test]
    fn multi_level_dependency_edges_are_processed_after_their_parents() {
        let root_package = instance("x", "1.0.0", PeerContext::default());
        let middle = instance("m", "1.0.0", PeerContext::default());
        let leaf = instance("n", "1.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![root_package.clone(), middle.clone(), leaf.clone()],
            root_dependencies: vec![key(&root_package)],
            dependency_edges: vec![
                DependencyEdge {
                    parent: key(&middle),
                    child: key(&leaf),
                },
                DependencyEdge {
                    parent: key(&root_package),
                    child: key(&middle),
                },
            ],
        };

        let plan = plan_layout(test_managed_root(), input, Platform::Unix).unwrap();

        assert!(
            plan.activation
                .steps
                .iter()
                .any(|step| { step.target == test_project_root().join("node_modules").join("n") })
        );
    }

    #[test]
    fn reused_instance_materializes_transitive_children_at_every_parent_location() {
        let first_parent = instance("first", "1.0.0", PeerContext::default());
        let second_parent = instance("second", "1.0.0", PeerContext::default());
        let shared = instance("debug", "4.4.3", PeerContext::default());
        let leaf = instance("ms", "2.1.3", PeerContext::default());
        let input = LayoutInput {
            instances: vec![
                first_parent.clone(),
                second_parent.clone(),
                shared.clone(),
                leaf.clone(),
            ],
            root_dependencies: vec![key(&first_parent), key(&second_parent)],
            dependency_edges: vec![
                DependencyEdge {
                    parent: key(&first_parent),
                    child: key(&shared),
                },
                DependencyEdge {
                    parent: key(&second_parent),
                    child: key(&shared),
                },
                DependencyEdge {
                    parent: key(&shared),
                    child: key(&leaf),
                },
            ],
        };

        let plan = plan_layout(test_managed_root(), input, Platform::Unix).unwrap();
        let targets: Vec<_> = plan
            .activation
            .steps
            .iter()
            .map(|step| step.target.clone())
            .collect();

        assert!(targets.contains(&test_project_root().join("node_modules").join("debug")));
        assert!(targets.contains(&test_project_root().join("node_modules").join("ms")));
    }

    #[test]
    fn nearest_same_name_shadow_blocks_outer_resolution() {
        let app = instance("app", "1.0.0", PeerContext::default());
        let bridge = instance("bridge", "1.0.0", PeerContext::default());
        let root_bridge = instance("bridge", "2.0.0", PeerContext::default());
        let outer_dep = instance("dep", "1.0.0", PeerContext::default());
        let shadowing_dep = instance("dep", "2.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![
                app.clone(),
                bridge.clone(),
                root_bridge.clone(),
                outer_dep.clone(),
                shadowing_dep.clone(),
            ],
            root_dependencies: vec![key(&app), key(&outer_dep), key(&root_bridge)],
            dependency_edges: vec![
                DependencyEdge {
                    parent: key(&app),
                    child: key(&bridge),
                },
                DependencyEdge {
                    parent: key(&app),
                    child: key(&shadowing_dep),
                },
                DependencyEdge {
                    parent: key(&bridge),
                    child: key(&outer_dep),
                },
            ],
        };

        let plan = plan_layout(test_managed_root(), input, Platform::Unix).unwrap();
        let nested_target = test_project_root()
            .join("node_modules")
            .join("app")
            .join("node_modules")
            .join("bridge")
            .join("node_modules")
            .join("dep");

        let outer_source = plan
            .entries
            .iter()
            .find(|entry| entry.instance == outer_dep.id)
            .expect("outer dependency storage")
            .target
            .clone();
        assert!(
            plan.activation
                .steps
                .iter()
                .any(|step| { step.target == nested_target && step.source == outer_source })
        );
    }

    #[test]
    fn same_name_version_cycle_places_shadowed_ancestor() {
        let first = instance("cycle", "1.0.0", PeerContext::default());
        let shadow = instance("cycle", "2.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![first.clone(), shadow.clone()],
            root_dependencies: vec![key(&first)],
            dependency_edges: vec![
                DependencyEdge {
                    parent: key(&first),
                    child: key(&shadow),
                },
                DependencyEdge {
                    parent: key(&shadow),
                    child: key(&first),
                },
            ],
        };

        let plan = plan_layout(test_managed_root(), input, Platform::Unix).unwrap();
        let nested_target = test_project_root()
            .join("node_modules")
            .join("cycle")
            .join("node_modules")
            .join("cycle")
            .join("node_modules")
            .join("cycle");

        assert!(
            plan.activation.steps.iter().any(|step| {
                step.target == nested_target && step.source == plan.entries[0].target
            })
        );
    }

    #[test]
    fn permuted_layout_inputs_are_deterministic_under_same_name_shadowing() {
        let app = instance("app", "1.0.0", PeerContext::default());
        let bridge = instance("bridge", "1.0.0", PeerContext::default());
        let root_bridge = instance("bridge", "2.0.0", PeerContext::default());
        let outer_dep = instance("dep", "1.0.0", PeerContext::default());
        let shadowing_dep = instance("dep", "2.0.0", PeerContext::default());
        let instances = vec![
            app.clone(),
            bridge.clone(),
            root_bridge.clone(),
            outer_dep.clone(),
            shadowing_dep.clone(),
        ];
        let roots = vec![key(&app), key(&outer_dep), key(&root_bridge)];
        let edges = vec![
            DependencyEdge {
                parent: key(&app),
                child: key(&bridge),
            },
            DependencyEdge {
                parent: key(&app),
                child: key(&shadowing_dep),
            },
            DependencyEdge {
                parent: key(&bridge),
                child: key(&outer_dep),
            },
        ];
        let mut permuted_instances = instances.clone();
        permuted_instances.reverse();
        let mut permuted_roots = roots.clone();
        permuted_roots.reverse();
        let mut permuted_edges = edges.clone();
        permuted_edges.reverse();

        let expected = plan_layout(
            test_managed_root(),
            LayoutInput {
                instances,
                root_dependencies: roots,
                dependency_edges: edges,
            },
            Platform::Unix,
        )
        .unwrap();
        let actual = plan_layout(
            test_managed_root(),
            LayoutInput {
                instances: permuted_instances,
                root_dependencies: permuted_roots,
                dependency_edges: permuted_edges,
            },
            Platform::Unix,
        )
        .unwrap();
        let nested_target = test_project_root()
            .join("node_modules")
            .join("app")
            .join("node_modules")
            .join("bridge")
            .join("node_modules")
            .join("dep");

        assert_eq!(actual, expected);
        assert!(
            actual
                .activation
                .steps
                .iter()
                .any(|step| step.target == nested_target)
        );
    }

    #[test]
    fn unplaceable_dependency_cycle_uses_the_existing_unknown_parent_error() {
        let first = instance("x", "1.0.0", PeerContext::default());
        let second = instance("m", "1.0.0", PeerContext::default());
        let input = LayoutInput {
            instances: vec![first.clone(), second.clone()],
            root_dependencies: vec![],
            dependency_edges: vec![
                DependencyEdge {
                    parent: key(&first),
                    child: key(&second),
                },
                DependencyEdge {
                    parent: key(&second),
                    child: key(&first),
                },
            ],
        };

        assert!(matches!(
            plan_layout(test_managed_root(), input, Platform::Unix),
            Err(PlanError::UnknownParent(_))
        ));
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
            plan_layout(test_managed_root(), input, Platform::Unix),
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
        let windows = plan_layout(test_managed_root(), input.clone(), Platform::Windows).unwrap();
        assert_eq!(windows.activation.steps[0].kind, LinkKind::Junction);
        assert!(matches!(
            plan_layout(test_managed_root(), input, Platform::Other),
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
}
