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
}
