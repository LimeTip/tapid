use crate::MetadataError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use tapid_core::{PackageIntegrity, PackageName, PackageVersion, RegistryOrigin};
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FetchMode {
    #[default]
    Online,
    Offline,
    Frozen,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawRegistrySnapshot {
    pub registry: String,
    pub packages: Vec<RawPackageMetadata>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawPackageMetadata {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub integrity: Option<String>,
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    registry: RegistryOrigin,
    packages: BTreeMap<PackageName, Vec<PackageMetadata>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageMetadata {
    pub identity: RegistryPackageId,
    pub integrity: Option<PackageIntegrity>,
    pub artifact: Option<String>,
    pub dependencies: BTreeMap<PackageName, String>,
    pub registry_kind: RegistryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryKind {
    Npm,
    Jsr,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegistryPackageId {
    pub registry: RegistryOrigin,
    pub name: PackageName,
    pub version: PackageVersion,
}
impl RegistryPackageId {
    pub fn new(registry: RegistryOrigin, name: PackageName, version: PackageVersion) -> Self {
        Self {
            registry,
            name,
            version,
        }
    }
}
impl fmt::Display for RegistryPackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.registry, self.name, self.version)
    }
}

impl RegistrySnapshot {
    pub fn normalize(raw: RawRegistrySnapshot) -> Result<Self, MetadataError> {
        let registry: RegistryOrigin = raw
            .registry
            .parse()
            .map_err(|_| MetadataError::InvalidRegistry(raw.registry.clone()))?;
        let registry_kind = if registry.to_string() == "https://jsr.io" {
            RegistryKind::Jsr
        } else {
            RegistryKind::Npm
        };
        let mut packages = BTreeMap::new();
        for entry in raw.packages {
            let name: PackageName = entry
                .name
                .trim()
                .parse()
                .map_err(|_| MetadataError::InvalidPackageName(entry.name.clone()))?;
            let version: PackageVersion = entry
                .version
                .trim()
                .parse()
                .map_err(|_| MetadataError::InvalidVersion(entry.version.clone()))?;
            let integrity = entry
                .integrity
                .map(|v| v.parse().map_err(|_| MetadataError::InvalidIntegrity(v)))
                .transpose()?;
            let id = RegistryPackageId::new(registry.clone(), name.clone(), version.clone());
            let candidates = packages.entry(name).or_insert_with(Vec::new);
            if candidates
                .iter()
                .any(|p: &PackageMetadata| p.identity.version == version)
            {
                return Err(MetadataError::DuplicateVersion(id.to_string()));
            }
            let artifact = entry
                .artifact
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty());
            if artifact.as_deref().is_some_and(|v| Url::parse(v).is_err()) {
                return Err(MetadataError::InvalidArtifact(id.to_string()));
            }
            let dependencies = entry
                .dependencies
                .into_iter()
                .map(|(name, requirement)| {
                    let package: PackageName = name
                        .parse()
                        .map_err(|_| MetadataError::InvalidDependency(name.clone()))?;
                    if requirement.trim().is_empty() {
                        return Err(MetadataError::InvalidDependency(name));
                    }
                    Ok((package, requirement))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            candidates.push(PackageMetadata {
                identity: id,
                integrity,
                artifact,
                dependencies,
                registry_kind,
            });
        }
        for candidates in packages.values_mut() {
            candidates.sort_by(|a, b| b.identity.version.cmp(&a.identity.version));
        }
        Ok(Self { registry, packages })
    }
    pub fn registry(&self) -> &RegistryOrigin {
        &self.registry
    }
    pub fn packages(&self) -> &BTreeMap<PackageName, Vec<PackageMetadata>> {
        &self.packages
    }
    pub fn candidates(&self, name: &PackageName) -> &[PackageMetadata] {
        self.packages.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// npm package compatibility constraints used for deterministic platform selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePlatform {
    /// Supported or negated npm operating-system identifiers.
    pub os: Vec<String>,
    /// Supported or negated npm CPU architecture identifiers.
    pub cpu: Vec<String>,
    /// Supported or negated npm libc identifiers.
    pub libc: Vec<String>,
}
impl PackagePlatform {
    /// Returns constraints that permit every supported platform.
    pub fn unrestricted() -> Self {
        Self {
            os: Vec::new(),
            cpu: Vec::new(),
            libc: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryArtifact {
    pub identity: RegistryPackageId,
    pub artifact_url: String,
    pub integrity: Option<PackageIntegrity>,
    pub dependencies: BTreeMap<PackageName, String>,
    pub optional_dependencies: BTreeMap<PackageName, String>,
    pub platform: PackagePlatform,
    pub registry_kind: RegistryKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_sorts_versions() {
        let p = |v: &str| RawPackageMetadata {
            name: "foo".into(),
            version: v.into(),
            integrity: None,
            artifact: None,
            dependencies: BTreeMap::new(),
        };
        let snapshot = RegistrySnapshot::normalize(RawRegistrySnapshot {
            registry: "https://x".into(),
            packages: vec![p("1.0.0"), p("2.0.0")],
        })
        .unwrap();
        assert_eq!(
            snapshot.candidates(&"foo".parse().unwrap())[0]
                .identity
                .version
                .to_string(),
            "2.0.0"
        );
    }

    #[test]
    fn registry_identity_is_part_of_package_identity() {
        let a = RegistryPackageId::new(
            "https://a".parse().unwrap(),
            "foo".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        let b = RegistryPackageId::new(
            "https://b".parse().unwrap(),
            "foo".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        assert_ne!(a, b);
    }
}
