//! Normalized registry metadata and injected transport boundary.
#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use tapid_core::{PackageIntegrity, PackageName, PackageVersion, RegistryOrigin};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataError {
    InvalidRegistry(String),
    InvalidPackageName(String),
    InvalidVersion(String),
    InvalidIntegrity(String),
    DuplicateVersion(String),
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for MetadataError {}

impl RegistrySnapshot {
    pub fn normalize(raw: RawRegistrySnapshot) -> Result<Self, MetadataError> {
        let registry: RegistryOrigin = raw
            .registry
            .parse()
            .map_err(|_| MetadataError::InvalidRegistry(raw.registry.clone()))?;
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
                .map(|value| {
                    value
                        .parse()
                        .map_err(|_| MetadataError::InvalidIntegrity(value))
                })
                .transpose()?;
            let id = RegistryPackageId::new(registry.clone(), name.clone(), version);
            let candidates = packages.entry(name).or_insert_with(Vec::new);
            if candidates
                .iter()
                .any(|package: &PackageMetadata| package.identity.version == version)
            {
                return Err(MetadataError::DuplicateVersion(id.to_string()));
            }
            candidates.push(PackageMetadata {
                identity: id,
                integrity,
                artifact: entry
                    .artifact
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty()),
            });
        }

        for candidates in packages.values_mut() {
            candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.identity.version));
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

pub trait RegistryTransport {
    type Error: std::error::Error + Send + Sync + 'static;

    fn fetch(&self, registry: &RegistryOrigin) -> Result<RawRegistrySnapshot, Self::Error>;
}

pub struct RegistryClient<T> {
    transport: T,
}

impl<T> RegistryClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T: RegistryTransport> RegistryClient<T> {
    pub fn snapshot(
        &self,
        registry: &RegistryOrigin,
        mode: FetchMode,
    ) -> Result<RegistrySnapshot, ClientError<T::Error>> {
        if mode != FetchMode::Online {
            return Err(ClientError::NetworkDisabled(mode));
        }
        RegistrySnapshot::normalize(
            self.transport
                .fetch(registry)
                .map_err(ClientError::Transport)?,
        )
        .map_err(ClientError::Metadata)
    }
}

#[derive(Debug)]
pub enum ClientError<E> {
    Transport(E),
    Metadata(MetadataError),
    NetworkDisabled(FetchMode),
}

impl<E: fmt::Display> fmt::Display for ClientError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "registry transport failed: {error}"),
            Self::Metadata(error) => write!(f, "invalid registry metadata: {error}"),
            Self::NetworkDisabled(mode) => write!(f, "network disabled in {mode:?} mode"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ClientError<E> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(version: &str) -> RawPackageMetadata {
        RawPackageMetadata {
            name: "foo".into(),
            version: version.into(),
            integrity: None,
            artifact: None,
        }
    }

    #[test]
    fn normalization_sorts_versions() {
        let snapshot = RegistrySnapshot::normalize(RawRegistrySnapshot {
            registry: "https://x".into(),
            packages: vec![package("1.0.0"), package("2.0.0")],
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
    fn malformed_metadata_is_rejected() {
        assert!(
            RegistrySnapshot::normalize(RawRegistrySnapshot {
                registry: "https://x".into(),
                packages: vec![package("bad")],
            })
            .is_err()
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
