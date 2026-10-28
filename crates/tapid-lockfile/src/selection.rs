use serde::{Deserialize, Serialize};
use tapid_core::PackageName;

use crate::{Lockfile, LockfileError, LockfilePackageKey};

/// Effective inputs for a recorded ordinary-lock selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedSelection {
    pub target: SelectionTarget,
    pub profile: SelectionProfile,
    pub omissions: Vec<AcceptedOmission>,
    pub declarations: Vec<RetainedDeclaration>,
}

/// A selected dependency declaration, before overlapping sections are collapsed.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedDeclaration {
    pub origin: OmissionOrigin,
    pub section: DeclarationSection,
    pub name: String,
    pub requirement: String,
    /// Original workspace override range or copied root specification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_requirement: Option<String>,
    pub target: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeclarationSection {
    Dependencies,
    DevDependencies,
    OptionalDependencies,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionTarget {
    pub os: String,
    pub cpu: String,
    pub libc: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionProfile {
    Development,
    Production,
}

/// An optional declaration cut, rather than an independently failed descendant.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedOmission {
    pub origin: OmissionOrigin,
    pub name: String,
    pub requirement: String,
    /// Original workspace manifest range when a root override changes selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_requirement: Option<String>,
    pub registry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub reason: OmissionReason,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum OmissionOrigin {
    Root,
    Workspace { key: String },
    Package { key: String },
}

/// Known failure stages, or explicit absence in an imported placement graph.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OmissionReason {
    MetadataAbsent,
    NoUsableCandidate,
    IncompatiblePlatform,
    /// The imported npm lock has no provider; its original failure stage is unknown.
    ImportedPlacementAbsent,
}

impl LockedSelection {
    pub(crate) fn validate(&self, lock: &Lockfile) -> Result<(), LockfileError> {
        let invalid = || LockfileError::InvalidSelection;
        if !self.target.is_valid()
            || self.omissions.len() > 100_000
            || self.omissions.windows(2).any(|w| w[0] >= w[1])
            || self.declarations.len() > 100_000
            || self.declarations.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(invalid());
        }
        let mut bound_roots = lock
            .root_bindings()
            .values()
            .collect::<std::collections::BTreeSet<_>>();
        bound_roots.extend(lock.workspace_packages().keys());
        if lock
            .roots()
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            != bound_roots
        {
            return Err(invalid());
        }
        let mut retained = std::collections::BTreeSet::new();
        let mut covered = std::collections::BTreeSet::new();
        for declaration in &self.declarations {
            let name = declaration
                .name
                .parse::<PackageName>()
                .map_err(|_| invalid())?;
            let target = declaration.target.parse::<LockfilePackageKey>()?;
            let copied_root =
                declaration.origin == OmissionOrigin::Root && target.source.copied().is_some();
            if copied_root {
                let original = declaration
                    .original_requirement
                    .as_deref()
                    .ok_or_else(invalid)?;
                // The CLI owns copied specification parsing and checks it against
                // the immutable source. Here retain the bounded original input;
                // the effective requirement below still validates package identity.
                if original.len() > 4096
                    || original.trim() != original
                    || original.chars().any(char::is_control)
                    || !(original.starts_with("file:") || original.starts_with("git+https://"))
                {
                    return Err(invalid());
                }
            } else {
                validate_original_requirement(
                    &declaration.origin,
                    &declaration.name,
                    &declaration.requirement,
                    declaration.original_requirement.as_deref(),
                )?;
            }
            if (declaration.original_requirement.is_some() && target.source.workspace().is_some())
                || declaration.requirement.len() > 4096
                || declaration.requirement.trim() != declaration.requirement
                || !retained.insert((&declaration.origin, declaration.section, &declaration.name))
            {
                return Err(invalid());
            }
            // These protocol symbols describe the exact local member, and cannot
            // authorize registry fallback or a package-origin local dependency.
            let local_protocol = declaration.requirement.starts_with("workspace:");
            if local_protocol
                && (target.source.workspace().is_none()
                    || matches!(declaration.origin, OmissionOrigin::Package { .. })
                    || !matches!(
                        declaration.requirement.as_str(),
                        "workspace:*" | "workspace:^" | "workspace:~"
                    ))
            {
                return Err(invalid());
            }
            let requirement = if local_protocol {
                "*"
            } else {
                &declaration.requirement
            }
            .parse::<tapid_resolver::Requirement>()
            .map_err(|_| invalid())?;
            if target.source.workspace().is_some()
                && (name != target.name || requirement.is_alias())
            {
                return Err(invalid());
            }
            let dependencies = match &declaration.origin {
                OmissionOrigin::Root => lock.root_bindings(),
                OmissionOrigin::Workspace { key } => lock
                    .workspace_packages()
                    .get(key)
                    .ok_or_else(invalid)?
                    .dependencies(),
                OmissionOrigin::Package { key } => {
                    if declaration.section == DeclarationSection::DevDependencies {
                        return Err(invalid());
                    }
                    lock.packages().get(key).ok_or_else(invalid)?.dependencies()
                }
            };
            let local_root = declaration.origin == OmissionOrigin::Root
                && target.source.workspace().is_some()
                && !dependencies.contains_key(&declaration.name)
                && lock.roots().contains(&declaration.target);
            if (!local_root && dependencies.get(&declaration.name) != Some(&declaration.target))
                || requirement.package_name(&name) != &target.name
                || !requirement.matches_locked_version(&target.version)
            {
                return Err(invalid());
            }
            covered.insert((declaration.origin.clone(), declaration.name.clone()));
        }
        // Every selected edge needs provenance, including required/optional overlap.
        let edges =
            lock.root_bindings()
                .keys()
                .map(|name| (OmissionOrigin::Root, name.clone()))
                .chain(lock.packages().iter().flat_map(|(key, package)| {
                    package.dependencies().keys().map(move |name| {
                        (OmissionOrigin::Package { key: key.clone() }, name.clone())
                    })
                }))
                .chain(lock.workspace_packages().iter().flat_map(|(key, package)| {
                    package.dependencies().keys().map(move |name| {
                        (OmissionOrigin::Workspace { key: key.clone() }, name.clone())
                    })
                }));
        if edges.into_iter().any(|edge| !covered.contains(&edge)) {
            return Err(invalid());
        }
        let mut declarations = std::collections::BTreeSet::new();
        for omission in &self.omissions {
            if omission.reason == OmissionReason::ImportedPlacementAbsent {
                return Err(invalid());
            }
            validate_original_requirement(
                &omission.origin,
                &omission.name,
                &omission.requirement,
                omission.original_requirement.as_deref(),
            )?;
            let registry = omission
                .registry
                .parse::<tapid_core::RegistryOrigin>()
                .map_err(|_| invalid())?;
            if registry.as_str() != omission.registry {
                return Err(invalid());
            }
            let name = omission
                .name
                .parse::<PackageName>()
                .map_err(|_| invalid())?;
            if omission.requirement.len() > 4096
                || omission.requirement.trim() != omission.requirement
                || !declarations.insert((&omission.origin, &omission.name))
            {
                return Err(invalid());
            }
            let requirement = omission
                .requirement
                .parse::<tapid_resolver::Requirement>()
                .map_err(|_| invalid())?;
            let dependencies = match &omission.origin {
                OmissionOrigin::Root => lock.root_bindings(),
                OmissionOrigin::Workspace { key } => {
                    key.parse::<LockfilePackageKey>()?;
                    lock.workspace_packages()
                        .get(key)
                        .ok_or_else(invalid)?
                        .dependencies()
                }
                OmissionOrigin::Package { key } => {
                    key.parse::<LockfilePackageKey>()?;
                    lock.packages().get(key).ok_or_else(invalid)?.dependencies()
                }
            };
            if dependencies.contains_key(&omission.name) {
                return Err(invalid());
            }
            if let Some(target) = &omission.target {
                let identity = target.parse::<LockfilePackageKey>()?;
                if identity.source.registry() != Some(&registry)
                    || requirement.package_name(&name) != &identity.name
                    || !requirement.matches_locked_version(&identity.version)
                    || lock.contains_package_key(target)
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}

impl SelectionTarget {
    pub(crate) fn is_valid(&self) -> bool {
        let token = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        token(&self.os) && token(&self.cpu) && self.libc.as_ref().is_none_or(|s| token(s))
    }
}

fn validate_original_requirement(
    origin: &OmissionOrigin,
    name: &str,
    effective: &str,
    original: Option<&str>,
) -> Result<(), LockfileError> {
    let Some(original) = original else {
        return Ok(());
    };
    let invalid = || LockfileError::InvalidSelection;
    if !matches!(origin, OmissionOrigin::Workspace { .. })
        || original == effective
        || original.len() > 4096
        || original.trim() != original
    {
        return Err(invalid());
    }
    let name = name.parse::<PackageName>().map_err(|_| invalid())?;
    let original = original
        .parse::<tapid_resolver::Requirement>()
        .map_err(|_| invalid())?;
    let effective = effective
        .parse::<tapid_resolver::Requirement>()
        .map_err(|_| invalid())?;
    if original.package_name(&name) != effective.package_name(&name) {
        return Err(invalid());
    }
    Ok(())
}
