use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tapid_core::{ArtifactDigest, PackageName, PackageVersion};

use crate::{LOCKFILE_VERSION, LockfileError, validation};

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
            validation::validate_url(package.registry.as_str())?;
            package
                .name
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?;
            package
                .version
                .parse::<PackageVersion>()
                .map_err(LockfileError::Domain)?;
            package
                .unpacked_digest
                .parse::<ArtifactDigest>()
                .map_err(LockfileError::Domain)?;
            validation::validate_sha512(&package.artifact_integrity)?;
            if let Some(url) = &package.artifact_url {
                validation::validate_url(url)?;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_url: Option<String>,
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
        validation::validate_url(registry)?;
        validation::validate_sha512(artifact_integrity)?;
        Ok(Self {
            registry: registry.to_owned(),
            name: name
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            version: version
                .parse::<PackageVersion>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            artifact_integrity: artifact_integrity.to_owned(),
            unpacked_digest: unpacked_digest
                .parse::<ArtifactDigest>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            artifact_url: None,
            dependencies: BTreeMap::new(),
        })
    }

    pub fn key(&self) -> String {
        format!("{}@{}", self.name, self.version)
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
