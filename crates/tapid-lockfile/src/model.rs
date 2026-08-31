use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tapid_core::{
    ArtifactDigest, PackageIntegrity, PackageName, PackageVersion, PeerContext, PlatformContext,
    RegistryOrigin,
};

use crate::{LEGACY_LOCKFILE_VERSION, LOCKFILE_VERSION, LockfileError, validation};

fn encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'@' => vec![byte],
            _ => format!("%{byte:02X}").into_bytes(),
        })
        .map(char::from)
        .collect()
}

fn canonical_peer_context(context: &PeerContext) -> String {
    context
        .entries()
        .iter()
        .map(|(name, version)| {
            format!(
                "name={};version={}",
                encode(&name.to_string()),
                encode(&version.to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn canonical_platform_context(context: &PlatformContext) -> String {
    format!(
        "os={};cpu={};libc={}",
        context.os.as_deref().map(encode).unwrap_or_default(),
        context.cpu.as_deref().map(encode).unwrap_or_default(),
        context.libc.as_deref().map(encode).unwrap_or_default()
    )
}

fn canonical_artifact_digest(value: &str) -> Result<String, LockfileError> {
    value
        .parse::<ArtifactDigest>()
        .map(|digest| digest.to_string())
        .map_err(LockfileError::Domain)
}

fn context_or_dash(context: &str) -> &str {
    if context.is_empty() { "-" } else { context }
}

fn parse_context(field: &str, prefix: &str, original: &str) -> Result<String, LockfileError> {
    let value = field
        .strip_prefix(prefix)
        .ok_or_else(|| LockfileError::InvalidPackageKey(original.into()))?;
    if value.is_empty() || value == "-" {
        return Ok(String::new());
    }
    if value.chars().any(char::is_whitespace) {
        return Err(LockfileError::InvalidPackageKey(original.into()));
    }
    Ok(value.to_owned())
}

fn percent_decode(value: &str, original: &str) -> Result<String, LockfileError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(LockfileError::InvalidPackageKey(original.into()));
            }
            let high = (bytes[index + 1] as char)
                .to_digit(16)
                .ok_or_else(|| LockfileError::InvalidPackageKey(original.into()))?;
            let low = (bytes[index + 2] as char)
                .to_digit(16)
                .ok_or_else(|| LockfileError::InvalidPackageKey(original.into()))?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| LockfileError::InvalidPackageKey(original.into()))
}

fn validate_peer_context(value: &str, original: &str) -> Result<(), LockfileError> {
    if value.is_empty() {
        return Ok(());
    }
    let mut context = PeerContext::default();
    for item in value.split(',') {
        let (name, version) = item
            .split_once(";version=")
            .and_then(|(name, version)| name.strip_prefix("name=").map(|name| (name, version)))
            .ok_or_else(|| LockfileError::InvalidPackageKey(original.into()))?;
        context = context.with(
            percent_decode(name, original)?
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?,
            percent_decode(version, original)?
                .parse::<PackageVersion>()
                .map_err(LockfileError::Domain)?,
        );
    }
    if canonical_peer_context(&context) != value {
        return Err(LockfileError::InvalidPackageKey(original.into()));
    }
    Ok(())
}

fn validate_platform_context(value: &str, original: &str) -> Result<(), LockfileError> {
    if value.is_empty() {
        return Ok(());
    }
    let (os, rest) = value
        .strip_prefix("os=")
        .and_then(|value| value.split_once(";cpu="))
        .ok_or_else(|| LockfileError::InvalidPackageKey(original.into()))?;
    let (cpu, libc) = rest
        .split_once(";libc=")
        .ok_or_else(|| LockfileError::InvalidPackageKey(original.into()))?;
    let decode = |field: &str| -> Result<Option<String>, LockfileError> {
        if field.is_empty() {
            Ok(None)
        } else {
            percent_decode(field, original).map(Some)
        }
    };
    let os = decode(os)?;
    let cpu = decode(cpu)?;
    let libc = decode(libc)?;
    let context = PlatformContext::new(os.as_deref(), cpu.as_deref(), libc.as_deref())
        .map_err(LockfileError::Domain)?;
    if canonical_platform_context(&context) != value {
        return Err(LockfileError::InvalidPackageKey(original.into()));
    }
    Ok(())
}

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
            peer_context: canonical_peer_context(peer_context),
            platform_context: canonical_platform_context(platform_context),
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
            context_or_dash(&self.peer_context),
            context_or_dash(&self.platform_context)
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
        let key = Self {
            registry: p[0].parse().map_err(LockfileError::Domain)?,
            name: name.parse().map_err(LockfileError::Domain)?,
            version: version.parse().map_err(LockfileError::Domain)?,
            peer_context: parse_context(p[2], "peer=", value)?,
            platform_context: parse_context(p[3], "platform=", value)?,
        };
        validate_peer_context(&key.peer_context, value)?;
        validate_platform_context(&key.platform_context, value)?;
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roots: Vec<String>,
    packages: BTreeMap<String, LockedPackage>,
}

impl Lockfile {
    pub fn new(root_manifest_digest: &str) -> Result<Self, LockfileError> {
        let root_manifest_digest = canonical_artifact_digest(root_manifest_digest)?;
        Ok(Self {
            lockfile_version: LOCKFILE_VERSION,
            root_manifest_digest: root_manifest_digest.to_owned(),
            resolver_version: "0".to_owned(),
            linker_version: "0".to_owned(),
            roots: Vec::new(),
            packages: BTreeMap::new(),
        })
    }

    pub fn insert_package(&mut self, package: LockedPackage) -> Result<(), LockfileError> {
        self.insert_packages(std::iter::once(package))
    }

    /// Inserts a validated package batch, allowing dependencies within the batch.
    ///
    /// A lockfile dependency graph need not be acyclic. Single-package insertion
    /// retains the historical dangling-dependency check, while batch insertion
    /// lets online materialization commit mutually dependent packages atomically.
    pub fn insert_packages<I>(&mut self, packages: I) -> Result<(), LockfileError>
    where
        I: IntoIterator<Item = LockedPackage>,
    {
        let packages: Vec<_> = packages.into_iter().collect();
        let mut batch = BTreeMap::new();
        for package in &packages {
            package.validate()?;
            let key = package.key();
            if self.packages.contains_key(&key) || batch.contains_key(&key) {
                return Err(LockfileError::DuplicatePackage(key));
            }
            batch.insert(key, package);
        }
        for (key, package) in &batch {
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
                if !self.packages.contains_key(dependency) && !batch.contains_key(dependency) {
                    return Err(LockfileError::DanglingDependency {
                        package: key.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        self.packages
            .extend(packages.into_iter().map(|package| (package.key(), package)));
        Ok(())
    }

    pub fn packages(&self) -> &BTreeMap<String, LockedPackage> {
        &self.packages
    }

    /// Replaces the exact root package identities used during replay.
    pub fn set_roots<I, S>(&mut self, roots: I) -> Result<(), LockfileError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut roots = roots
            .into_iter()
            .map(|root| root.as_ref().to_owned())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        for root in &roots {
            root.parse::<LockfilePackageKey>()?;
            if !self.packages.contains_key(root) {
                return Err(LockfileError::DanglingRoot(root.clone()));
            }
        }
        self.roots = roots;
        Ok(())
    }

    /// Returns exact canonical package keys selected as project roots.
    pub fn roots(&self) -> &[String] {
        &self.roots
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
        let current_root_manifest_digest = canonical_artifact_digest(current_root_manifest_digest)?;
        if self.root_manifest_digest != current_root_manifest_digest {
            return Err(LockfileError::RootManifestDigestMismatch {
                expected: self.root_manifest_digest.clone(),
                actual: current_root_manifest_digest,
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
        let mut lockfile: Self =
            serde_json::from_str(input).map_err(LockfileError::Serialization)?;
        if lockfile.lockfile_version != LOCKFILE_VERSION
            && lockfile.lockfile_version != LEGACY_LOCKFILE_VERSION
        {
            return Err(LockfileError::UnsupportedVersion(lockfile.lockfile_version));
        }
        lockfile.root_manifest_digest = canonical_artifact_digest(&lockfile.root_manifest_digest)?;
        if lockfile.lockfile_version == LOCKFILE_VERSION
            && !lockfile.packages.is_empty()
            && lockfile.roots.is_empty()
        {
            return Err(LockfileError::MissingRoots);
        }
        let mut canonical_roots = lockfile.roots.clone();
        canonical_roots.sort();
        canonical_roots.dedup();
        if canonical_roots != lockfile.roots {
            return Err(LockfileError::NonCanonicalRoots);
        }
        for root in &lockfile.roots {
            root.parse::<LockfilePackageKey>()?;
            if !lockfile.packages.contains_key(root) {
                return Err(LockfileError::DanglingRoot(root.clone()));
            }
        }
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
            peer_context: canonical_peer_context(peer_context),
            platform_context: canonical_platform_context(platform_context),
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
            context_or_dash(&self.peer_context),
            context_or_dash(&self.platform_context)
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
        let key = self.key();
        validate_peer_context(&self.peer_context, &key)?;
        validate_platform_context(&self.platform_context, &key)?;
        validation::validate_registry_url(&self.registry)?;
        if let Some(url) = &self.artifact_url {
            validation::validate_artifact_url(url)?;
        }
        Ok(())
    }

    pub fn set_artifact_url(&mut self, url: &str) -> Result<(), LockfileError> {
        validation::validate_artifact_url(url)?;
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
