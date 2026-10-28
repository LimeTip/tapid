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

/// Only reasons whose failure stage is unambiguous are representable.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OmissionReason {
    MetadataAbsent,
    NoUsableCandidate,
    IncompatiblePlatform,
}

impl LockedSelection {
    pub(crate) fn validate(&self, lock: &Lockfile) -> Result<(), LockfileError> {
        let invalid = || LockfileError::InvalidSelection;
        let token = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        if !token(&self.target.os)
            || !token(&self.target.cpu)
            || self.target.libc.as_ref().is_some_and(|s| !token(s))
            || self.omissions.len() > 100_000
            || self.omissions.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(invalid());
        }
        let mut declarations = std::collections::BTreeSet::new();
        for omission in &self.omissions {
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
