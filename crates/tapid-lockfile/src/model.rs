use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tapid_core::{
    ArtifactDigest, PackageIntegrity, PackageName, PackageVersion, PeerContext, PlatformContext,
    RegistryOrigin,
};

use crate::{LOCKFILE_VERSION, LockfileError, validation};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LockfilePackageKey {
    pub registry: RegistryOrigin,
    pub name: PackageName,
    pub version: PackageVersion,
    pub peer_context: String,
    pub platform_context: String,
}

impl LockfilePackageKey {
    pub fn new(
        registry: RegistryOrigin,
        name: PackageName,
        version: PackageVersion,
        peer_context: &PeerContext,
        platform_context: &PlatformContext,
    ) -> Self {
        Self {
            registry,
            name,
            version,
            peer_context: peer_context.to_string(),
            platform_context: platform_context.to_string(),
        }
    }
}

impl std::fmt::Display for LockfilePackageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}|{}@{}|peer={}|platform={}",
            self.registry,
            self.name,
            self.version,
            if self.peer_context.is_empty() {
                "-"
            } else {
                &self.peer_context
            },
            if self.platform_context.is_empty() {
                "-"
            } else {
                &self.platform_context
            }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    lockfile_version: u32,
    root_manifest_digest: String,
    resolver_version: String,
    linker_version: String,
    packages: BTreeMap<String, LockedPackage>,
}

impl Lockfile {
    pub fn new(root_manifest_digest: &str) -> Result<Self, LockfileError> {
        root_manifest_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?;
        Ok(Self {
            lockfile_version: LOCKFILE_VERSION,
            root_manifest_digest: root_manifest_digest.to_owned(),
            resolver_version: "0".to_owned(),
            linker_version: "0".to_owned(),
            packages: BTreeMap::new(),
        })
    }

    pub fn insert_package(&mut self, package: LockedPackage) -> Result<(), LockfileError> {
        let key = package.key();
        if self.packages.insert(key.clone(), package).is_some() {
            return Err(LockfileError::DuplicatePackage(key));
        }
        Ok(())
    }

    pub fn packages(&self) -> &BTreeMap<String, LockedPackage> {
        &self.packages
    }

    pub fn to_json(&self) -> Result<String, LockfileError> {
        serde_json::to_string_pretty(self)
            .map(|json| format!("{json}\n"))
            .map_err(LockfileError::Serialization)
    }

    pub fn from_json(input: &str) -> Result<Self, LockfileError> {
        let lockfile: Self = serde_json::from_str(input).map_err(LockfileError::Serialization)?;
        if lockfile.lockfile_version != LOCKFILE_VERSION {
            return Err(LockfileError::UnsupportedVersion(lockfile.lockfile_version));
        }
        lockfile
            .root_manifest_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?;
        for (key, package) in &lockfile.packages {
            if key != &package.key() {
                return Err(LockfileError::PackageKeyMismatch(key.clone()));
            }
            package.validate()?;
        }
        Ok(lockfile)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedPackage {
    registry: String,
    name: String,
    version: String,
    artifact_integrity: String,
    unpacked_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_url: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    peer_context: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    platform_context: String,
    dependencies: BTreeMap<String, String>,
}

impl LockedPackage {
    pub fn new(
        registry: &str,
        name: &str,
        version: &str,
        artifact_integrity: &str,
        unpacked_digest: &str,
    ) -> Result<Self, LockfileError> {
        Self::new_with_context(
            registry,
            name,
            version,
            artifact_integrity,
            unpacked_digest,
            &PeerContext::default(),
            &PlatformContext::new(None, None, None).unwrap(),
        )
    }

    pub fn new_with_context(
        registry: &str,
        name: &str,
        version: &str,
        artifact_integrity: &str,
        unpacked_digest: &str,
        peer_context: &PeerContext,
        platform_context: &PlatformContext,
    ) -> Result<Self, LockfileError> {
        let package = Self {
            registry: registry
                .parse::<RegistryOrigin>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            name: name
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            version: version
                .parse::<PackageVersion>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            artifact_integrity: artifact_integrity
                .parse::<PackageIntegrity>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            unpacked_digest: unpacked_digest
                .parse::<ArtifactDigest>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            artifact_url: None,
            peer_context: peer_context.to_string(),
            platform_context: platform_context.to_string(),
            dependencies: BTreeMap::new(),
        };
        package.validate()?;
        Ok(package)
    }

    pub fn key(&self) -> String {
        let registry: RegistryOrigin = self.registry.parse().expect("validated registry origin");
        let name: PackageName = self.name.parse().expect("validated package name");
        let version: PackageVersion = self.version.parse().expect("validated package version");
        format!(
            "{}|{}@{}|peer={}|platform={}",
            registry,
            name,
            version,
            if self.peer_context.is_empty() {
                "-"
            } else {
                &self.peer_context
            },
            if self.platform_context.is_empty() {
                "-"
            } else {
                &self.platform_context
            }
        )
    }

    fn validate(&self) -> Result<(), LockfileError> {
        self.registry
            .parse::<RegistryOrigin>()
            .map_err(LockfileError::Domain)?;
        self.name
            .parse::<PackageName>()
            .map_err(LockfileError::Domain)?;
        self.version
            .parse::<PackageVersion>()
            .map_err(LockfileError::Domain)?;
        self.artifact_integrity
            .parse::<PackageIntegrity>()
            .map_err(LockfileError::Domain)?;
        self.unpacked_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?;
        validation::validate_url(&self.registry)?;
        if let Some(url) = &self.artifact_url {
            validation::validate_url(url)?;
        }
        Ok(())
    }

    pub fn set_artifact_url(&mut self, url: &str) -> Result<(), LockfileError> {
        validation::validate_url(url)?;
        self.artifact_url = Some(url.to_owned());
        Ok(())
    }
    pub fn add_dependency(&mut self, name: &str, key: &str) -> Result<(), LockfileError> {
        name.parse::<PackageName>().map_err(LockfileError::Domain)?;
        self.dependencies.insert(name.to_owned(), key.to_owned());
        Ok(())
    }
}
