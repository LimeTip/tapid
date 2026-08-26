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

impl std::str::FromStr for LockfilePackageKey {
    type Err = LockfileError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let p: Vec<_> = value.split('|').collect();
        if p.len() != 4 || !p[2].starts_with("peer=") || !p[3].starts_with("platform=") {
            return Err(LockfileError::InvalidPackageKey(value.into()));
        }
        let (name, version) = p[1]
            .rsplit_once('@')
            .ok_or_else(|| LockfileError::InvalidPackageKey(value.into()))?;
        let peer_context = &p[2][5..];
        let platform_context = &p[3][9..];
        let key = Self {
            registry: p[0].parse().map_err(LockfileError::Domain)?,
            name: name.parse().map_err(LockfileError::Domain)?,
            version: version.parse().map_err(LockfileError::Domain)?,
            peer_context: if peer_context == "-" {
                String::new()
            } else {
                peer_context.to_owned()
            },
            platform_context: if platform_context == "-" {
                String::new()
            } else {
                platform_context.to_owned()
            },
        };
        if key.peer_context.contains(' ') || key.platform_context.contains(' ') {
            return Err(LockfileError::InvalidPackageKey(value.into()));
        }
        Ok(key)
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
        package.validate()?;
        let key = package.key();
        if self.packages.contains_key(&key) {
            return Err(LockfileError::DuplicatePackage(key));
        }
        for (name, dependency) in &package.dependencies {
            let target = dependency.parse::<LockfilePackageKey>()?;
            if dependency == &key {
                return Err(LockfileError::SelfDependency(key));
            }
            if target.name.to_string() != *name {
                return Err(LockfileError::DependencyNameMismatch {
                    package: package.key(),
                    dependency: name.clone(),
                    target: target.name.to_string(),
                });
            }
            if !self.packages.contains_key(dependency) {
                return Err(LockfileError::DanglingDependency {
                    package: package.key(),
                    dependency: dependency.clone(),
                });
            }
        }
        self.packages.insert(key, package);
        Ok(())
    }

    pub fn packages(&self) -> &BTreeMap<String, LockedPackage> {
        &self.packages
    }

    /// Returns package entries with their validated typed keys.
    pub fn packages_typed(
        &self,
    ) -> Result<Vec<(LockfilePackageKey, &LockedPackage)>, LockfileError> {
        self.packages
            .iter()
            .map(|(encoded, package)| encoded.parse().map(|key| (key, package)))
            .collect()
    }

    pub fn root_manifest_digest(&self) -> &str {
        &self.root_manifest_digest
    }

    /// Checks that this lockfile belongs to the current root manifest.
    pub fn validate_replay(&self, current_root_manifest_digest: &str) -> Result<(), LockfileError> {
        if self.root_manifest_digest != current_root_manifest_digest {
            return Err(LockfileError::RootManifestDigestMismatch {
                expected: self.root_manifest_digest.clone(),
                actual: current_root_manifest_digest.to_owned(),
            });
        }
        Ok(())
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
            for (name, dependency) in &package.dependencies {
                let target = dependency.parse::<LockfilePackageKey>()?;
                if dependency == key {
                    return Err(LockfileError::SelfDependency(key.clone()));
                }
                if target.name.to_string() != *name {
                    return Err(LockfileError::DependencyNameMismatch {
                        package: key.clone(),
                        dependency: name.clone(),
                        target: target.name.to_string(),
                    });
                }
                if !lockfile.packages.contains_key(dependency) {
                    return Err(LockfileError::DanglingDependency {
                        package: key.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
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
    /// Explicit replay identity for the verified unpacked store tree.
    tree_digest: String,
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
            tree_digest: unpacked_digest.to_owned(),
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

    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }

    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.dependencies
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
        self.tree_digest
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
        let name = name.parse::<PackageName>().map_err(LockfileError::Domain)?;
        let parsed = key.parse::<LockfilePackageKey>()?;
        if parsed.name != name {
            return Err(LockfileError::DependencyNameMismatch {
                package: self.key(),
                dependency: name.to_string(),
                target: parsed.name.to_string(),
            });
        }
        if key == self.key() {
            return Err(LockfileError::SelfDependency(key.to_owned()));
        }
        self.dependencies.insert(name.to_string(), key.to_owned());
        Ok(())
    }
}
