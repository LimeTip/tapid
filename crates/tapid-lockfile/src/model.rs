use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Component;
use tapid_core::{
    ArtifactDigest, PackageIntegrity, PackageName, PackageSource, PackageVersion, PeerContext,
    PlatformContext, RegistryOrigin,
};

use crate::{
    COPIED_SOURCE_LOCKFILE_VERSION, LEGACY_LOCKFILE_VERSION, LIFECYCLE_LOCKFILE_VERSION,
    LOCKFILE_VERSION, LockfileError, PROVENANCE_LEGACY_LOCKFILE_VERSION,
    REGISTRY_ONLY_LOCKFILE_VERSION, ROOTS_LEGACY_LOCKFILE_VERSION, validation,
};

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

/// Identifies a package provided by a workspace member.
///
/// The path is a normalized, workspace-root-relative POSIX path. The source
/// records local package identity in the lockfile; installation resolves it to
/// the separately validated workspace member and never treats it as a registry artifact.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LocalWorkspaceSource {
    path: String,
    name: String,
    version: String,
}

impl LocalWorkspaceSource {
    pub fn new(path: &str, name: &str, version: &str) -> Result<Self, LockfileError> {
        let path = canonical_workspace_path(path)?;
        Ok(Self {
            path,
            name: name
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            version: version
                .parse::<PackageVersion>()
                .map_err(LockfileError::Domain)?
                .to_string(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn identity(&self) -> String {
        format!("workspace:{}:{}@{}", self.path, self.name, self.version)
    }
}

impl<'de> Deserialize<'de> for LocalWorkspaceSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            path: String,
            name: String,
            version: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(&wire.path, &wire.name, &wire.version).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for LocalWorkspaceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.identity().fmt(f)
    }
}

impl std::str::FromStr for LocalWorkspaceSource {
    type Err = LockfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let rest = value
            .strip_prefix("workspace:")
            .ok_or_else(|| LockfileError::InvalidWorkspaceSource(value.to_owned()))?;
        let (path, package) = rest
            .rsplit_once(':')
            .ok_or_else(|| LockfileError::InvalidWorkspaceSource(value.to_owned()))?;
        let (name, version) = package
            .rsplit_once('@')
            .ok_or_else(|| LockfileError::InvalidWorkspaceSource(value.to_owned()))?;
        let source = Self::new(path, name, version)?;
        if source.identity() != value {
            return Err(LockfileError::InvalidWorkspaceSource(value.to_owned()));
        }
        Ok(source)
    }
}

fn canonical_workspace_path(value: &str) -> Result<String, LockfileError> {
    if value.is_empty()
        || value.contains('\\')
        || value.contains('|')
        || value.chars().any(char::is_control)
    {
        return Err(LockfileError::InvalidWorkspaceSource(value.to_owned()));
    }
    let path = std::path::Path::new(value);
    if path.is_absolute() {
        return Err(LockfileError::InvalidWorkspaceSource(value.to_owned()));
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| LockfileError::InvalidWorkspaceSource(value.to_owned()))?;
                if part.is_empty() || part == "." || part.contains(':') {
                    return Err(LockfileError::InvalidWorkspaceSource(value.to_owned()));
                }
                components.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LockfileError::InvalidWorkspaceSource(value.to_owned()));
            }
        }
    }
    if components.is_empty() {
        return Err(LockfileError::InvalidWorkspaceSource(value.to_owned()));
    }
    Ok(components.join("/"))
}

/// Package source encoded in a persisted package identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LockfilePackageSource {
    Registry(RegistryOrigin),
    Copied(PackageSource),
    Workspace(LocalWorkspaceSource),
}

impl LockfilePackageSource {
    pub fn package_source(&self) -> Option<PackageSource> {
        match self {
            Self::Registry(origin) => Some(origin.clone().into()),
            Self::Copied(source) => Some(source.clone()),
            Self::Workspace(_) => None,
        }
    }
    pub fn copied(&self) -> Option<&PackageSource> {
        if let Self::Copied(source) = self {
            Some(source)
        } else {
            None
        }
    }

    pub fn registry(&self) -> Option<&RegistryOrigin> {
        match self {
            Self::Registry(origin) => Some(origin),
            Self::Workspace(_) | Self::Copied(_) => None,
        }
    }

    pub fn workspace(&self) -> Option<&LocalWorkspaceSource> {
        match self {
            Self::Registry(_) | Self::Copied(_) => None,
            Self::Workspace(source) => Some(source),
        }
    }
}

/// Exact persisted package identity. Parsing requires an already canonical
/// source origin and canonical peer/platform context.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LockfilePackageKey {
    pub source: LockfilePackageSource,
    pub name: PackageName,
    pub version: PackageVersion,
    pub peer_context: String,
    pub platform_context: String,
}

impl LockfilePackageKey {
    pub fn from_source(
        source: PackageSource,
        name: PackageName,
        version: PackageVersion,
        peer: &PeerContext,
        platform: &PlatformContext,
    ) -> Self {
        let source = match source.registry() {
            Some(origin) => LockfilePackageSource::Registry(origin.clone()),
            None => LockfilePackageSource::Copied(source),
        };
        Self::with_source(source, name, version, peer, platform)
    }

    pub fn new(
        registry: RegistryOrigin,
        name: PackageName,
        version: PackageVersion,
        peer_context: &PeerContext,
        platform_context: &PlatformContext,
    ) -> Self {
        Self::with_source(
            LockfilePackageSource::Registry(registry),
            name,
            version,
            peer_context,
            platform_context,
        )
    }

    pub fn with_source(
        source: LockfilePackageSource,
        name: PackageName,
        version: PackageVersion,
        peer_context: &PeerContext,
        platform_context: &PlatformContext,
    ) -> Self {
        Self {
            source,
            name,
            version,
            peer_context: canonical_peer_context(peer_context),
            platform_context: canonical_platform_context(platform_context),
        }
    }

    pub fn workspace(source: LocalWorkspaceSource) -> Self {
        let name = source
            .name
            .parse()
            .expect("validated workspace package name");
        let version = source
            .version
            .parse()
            .expect("validated workspace package version");
        Self {
            source: LockfilePackageSource::Workspace(source),
            name,
            version,
            peer_context: String::new(),
            platform_context: canonical_platform_context(
                &PlatformContext::new(None, None, None).unwrap(),
            ),
        }
    }
}

impl std::fmt::Display for LockfilePackageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match &self.source {
            LockfilePackageSource::Registry(origin) => origin.to_string(),
            LockfilePackageSource::Copied(source) => source.to_string(),
            LockfilePackageSource::Workspace(source) => source.identity(),
        };
        write!(
            f,
            "{}|{}@{}|peer={}|platform={}",
            source,
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
        let source = if p[0].starts_with("workspace:") {
            LockfilePackageSource::Workspace(p[0].parse()?)
        } else if p[0].starts_with("file:") || p[0].starts_with("git+") {
            LockfilePackageSource::Copied(p[0].parse().map_err(LockfileError::Domain)?)
        } else {
            let origin: RegistryOrigin = p[0].parse().map_err(LockfileError::Domain)?;
            if origin.as_str() != p[0] {
                return Err(LockfileError::NonCanonicalRegistryIdentity);
            }
            LockfilePackageSource::Registry(origin)
        };
        let key = Self {
            source,
            name: name.parse().map_err(LockfileError::Domain)?,
            version: version.parse().map_err(LockfileError::Domain)?,
            peer_context: parse_context(p[2], "peer=", value)?,
            platform_context: parse_context(p[3], "platform=", value)?,
        };
        validate_peer_context(&key.peer_context, value)?;
        validate_platform_context(&key.platform_context, value)?;
        if key.to_string() != value {
            return Err(LockfileError::InvalidPackageKey(value.into()));
        }
        match &key.source {
            LockfilePackageSource::Workspace(source)
                if source.name() != key.name.as_str()
                    || source.version() != key.version.to_string() =>
            {
                return Err(LockfileError::InvalidPackageKey(value.into()));
            }
            _ => {}
        }
        Ok(key)
    }
}

fn validate_dependency_target(
    package: &str,
    name: &str,
    dependency: &str,
    aliases: &BTreeMap<String, String>,
    exists: impl Fn(&str) -> bool,
) -> Result<(), LockfileError> {
    let target = dependency.parse::<LockfilePackageKey>()?;
    if dependency == package {
        return Err(LockfileError::SelfDependency(package.to_owned()));
    }
    if target.name.as_str() != name && aliases.get(name) != Some(&target.name.to_string()) {
        return Err(LockfileError::DependencyNameMismatch {
            package: package.to_owned(),
            dependency: name.to_owned(),
            target: target.name.to_string(),
        });
    }
    if !exists(dependency) {
        return Err(LockfileError::DanglingDependency {
            package: package.to_owned(),
            dependency: dependency.to_owned(),
        });
    }
    Ok(())
}

/// A package supplied by a local workspace member, with no registry artifact.
/// This is lock state for package management and does not imply runtime support.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedWorkspacePackage {
    source: LocalWorkspaceSource,
    manifest_digest: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    dependency_aliases: BTreeMap<String, String>,
}

impl LockedWorkspacePackage {
    pub fn new(source: LocalWorkspaceSource, manifest_digest: &str) -> Result<Self, LockfileError> {
        let manifest_digest = manifest_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?
            .to_string();
        Ok(Self {
            source,
            manifest_digest,
            dependencies: BTreeMap::new(),
            dependency_aliases: BTreeMap::new(),
        })
    }

    pub fn key(&self) -> String {
        LockfilePackageKey::workspace(self.source.clone()).to_string()
    }

    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.dependencies
    }

    pub fn add_alias_dependency(&mut self, name: &str, key: &str) -> Result<(), LockfileError> {
        let name = name
            .parse::<PackageName>()
            .map_err(LockfileError::Domain)?
            .to_string();
        let target = key.parse::<LockfilePackageKey>()?;
        if key == self.key() {
            return Err(LockfileError::SelfDependency(key.to_owned()));
        }
        if name != target.name.as_str() {
            self.dependency_aliases
                .insert(name.clone(), target.name.to_string());
        } else {
            self.dependency_aliases.remove(&name);
        }
        self.dependencies.insert(name, key.to_owned());
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
        self.dependency_aliases.remove(name.as_str());
        Ok(())
    }

    fn validate(&self) -> Result<(), LockfileError> {
        let source = LocalWorkspaceSource::new(
            self.source.path(),
            self.source.name(),
            self.source.version(),
        )?;
        if source != self.source {
            return Err(LockfileError::InvalidWorkspaceSource(
                self.source.identity(),
            ));
        }
        let digest = self
            .manifest_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?;
        if digest.to_string() != self.manifest_digest {
            return Err(LockfileError::InvalidPackageKey(self.key()));
        }
        for (name, dependency) in &self.dependencies {
            let parsed = dependency.parse::<LockfilePackageKey>()?;
            if parsed.name.as_str() != name
                && self.dependency_aliases.get(name) != Some(&parsed.name.to_string())
            {
                return Err(LockfileError::DependencyNameMismatch {
                    package: self.key(),
                    dependency: name.clone(),
                    target: parsed.name.to_string(),
                });
            }
            if dependency == &self.key() {
                return Err(LockfileError::SelfDependency(self.key()));
            }
        }
        for (name, actual) in &self.dependency_aliases {
            name.parse::<PackageName>().map_err(LockfileError::Domain)?;
            actual
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?;
            let target = self
                .dependencies
                .get(name)
                .ok_or_else(|| LockfileError::DependencyNameMismatch {
                    package: self.key(),
                    dependency: name.clone(),
                    target: actual.clone(),
                })?
                .parse::<LockfilePackageKey>()?;
            if actual != target.name.as_str() || name == actual {
                return Err(LockfileError::DependencyNameMismatch {
                    package: self.key(),
                    dependency: name.clone(),
                    target: target.name.to_string(),
                });
            }
        }
        Ok(())
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    root_bindings: BTreeMap<String, String>,
    packages: BTreeMap<String, LockedPackage>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_packages: BTreeMap<String, LockedWorkspacePackage>,
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
            root_bindings: BTreeMap::new(),
            packages: BTreeMap::new(),
            workspace_packages: BTreeMap::new(),
        })
    }

    pub fn insert_package(&mut self, package: LockedPackage) -> Result<(), LockfileError> {
        self.insert_packages(std::iter::once(package))
    }

    /// Records outputs produced by exact approved hooks without changing source identity.
    pub fn set_derived_hooks(
        &mut self,
        key: &str,
        outputs: Vec<DerivedHookOutput>,
    ) -> Result<(), LockfileError> {
        validate_derived_hooks(&outputs)?;
        let package = self
            .packages
            .get_mut(key)
            .ok_or_else(|| LockfileError::InvalidPackageKey(key.to_owned()))?;
        package.derived_hooks = outputs;
        if !package.derived_hooks.is_empty() {
            self.lockfile_version = self.lockfile_version.max(LIFECYCLE_LOCKFILE_VERSION);
        }
        Ok(())
    }

    /// Canonical source graph used to bind lifecycle recipes independently of outputs.
    pub fn source_graph(&self) -> Self {
        let mut source = self.clone();
        if source.lockfile_version == LIFECYCLE_LOCKFILE_VERSION {
            source.lockfile_version = LOCKFILE_VERSION;
        }
        for package in source.packages.values_mut() {
            package.derived_hooks.clear();
        }
        source
    }

    pub fn insert_workspace_package(
        &mut self,
        package: LockedWorkspacePackage,
    ) -> Result<(), LockfileError> {
        self.insert_graph([], [package])
    }

    /// Inserts registry and workspace packages together, validating graph edges
    /// against both source classes before making any changes.
    pub fn insert_graph<R, W>(
        &mut self,
        registry_packages: R,
        workspace_packages: W,
    ) -> Result<(), LockfileError>
    where
        R: IntoIterator<Item = LockedPackage>,
        W: IntoIterator<Item = LockedWorkspacePackage>,
    {
        let registry_packages: Vec<_> = registry_packages.into_iter().collect();
        let workspace_packages: Vec<_> = workspace_packages.into_iter().collect();
        let mut registry_batch = BTreeMap::new();
        let mut workspace_batch = BTreeMap::new();
        for package in &registry_packages {
            package.validate()?;
            let key = package.key();
            if self.packages.contains_key(&key)
                || self.workspace_packages.contains_key(&key)
                || registry_batch.contains_key(&key)
                || workspace_batch.contains_key(&key)
            {
                return Err(LockfileError::DuplicatePackage(key));
            }
            registry_batch.insert(key, package);
        }
        for package in &workspace_packages {
            package.validate()?;
            let key = package.key();
            if self.packages.contains_key(&key)
                || self.workspace_packages.contains_key(&key)
                || registry_batch.contains_key(&key)
                || workspace_batch.contains_key(&key)
            {
                return Err(LockfileError::DuplicatePackage(key));
            }
            workspace_batch.insert(key, package);
        }
        let exists = |key: &str| {
            self.packages.contains_key(key)
                || self.workspace_packages.contains_key(key)
                || registry_batch.contains_key(key)
                || workspace_batch.contains_key(key)
        };
        for (key, package) in &registry_batch {
            for (name, dependency) in &package.dependencies {
                validate_dependency_target(
                    key,
                    name,
                    dependency,
                    &package.dependency_aliases,
                    exists,
                )?;
            }
        }
        for (key, package) in &workspace_batch {
            for (name, dependency) in &package.dependencies {
                validate_dependency_target(
                    key,
                    name,
                    dependency,
                    &package.dependency_aliases,
                    exists,
                )?;
            }
        }
        if registry_packages.iter().any(|package| {
            package
                .source()
                .is_some_and(|source| source.registry().is_none())
        }) {
            self.lockfile_version = COPIED_SOURCE_LOCKFILE_VERSION;
        }
        self.packages.extend(
            registry_packages
                .into_iter()
                .map(|package| (package.key(), package)),
        );
        self.workspace_packages.extend(
            workspace_packages
                .into_iter()
                .map(|package| (package.key(), package)),
        );
        Ok(())
    }

    /// Inserts a validated registry-package batch, allowing dependency edges
    /// within the batch and to already inserted workspace packages.
    pub fn insert_packages<I>(&mut self, packages: I) -> Result<(), LockfileError>
    where
        I: IntoIterator<Item = LockedPackage>,
    {
        self.insert_graph(packages, std::iter::empty())
    }

    pub fn packages(&self) -> &BTreeMap<String, LockedPackage> {
        &self.packages
    }

    pub fn workspace_packages(&self) -> &BTreeMap<String, LockedWorkspacePackage> {
        &self.workspace_packages
    }

    pub fn workspace_packages_typed(
        &self,
    ) -> Result<Vec<(LockfilePackageKey, &LockedWorkspacePackage)>, LockfileError> {
        self.workspace_packages
            .iter()
            .map(|(encoded, package)| encoded.parse().map(|key| (key, package)))
            .collect()
    }

    pub fn contains_package_key(&self, key: &str) -> bool {
        self.packages.contains_key(key) || self.workspace_packages.contains_key(key)
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
            if !self.contains_package_key(root) {
                return Err(LockfileError::DanglingRoot(root.clone()));
            }
        }
        self.roots = roots;
        self.root_bindings.clear();
        Ok(())
    }

    /// Returns exact canonical package keys selected as project roots.
    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    /// Records each local root name and its immutable actual package key.
    pub fn set_root_bindings(
        &mut self,
        bindings: BTreeMap<String, String>,
    ) -> Result<(), LockfileError> {
        for (name, target) in &bindings {
            name.parse::<PackageName>().map_err(LockfileError::Domain)?;
            target.parse::<LockfilePackageKey>()?;
            if !self.roots.contains(target) {
                return Err(LockfileError::DanglingRoot(target.clone()));
            }
        }
        let bound_roots = bindings
            .values()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for root in &self.roots {
            if !bound_roots.contains(root)
                && root
                    .parse::<LockfilePackageKey>()
                    .ok()
                    .is_none_or(|key| key.source.workspace().is_none())
            {
                return Err(LockfileError::NonCanonicalRoots);
            }
        }
        self.root_bindings = bindings;
        Ok(())
    }

    pub fn root_bindings(&self) -> &BTreeMap<String, String> {
        &self.root_bindings
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
        if self.lockfile_version == PROVENANCE_LEGACY_LOCKFILE_VERSION {
            return Err(LockfileError::RegenerationRequired(
                PROVENANCE_LEGACY_LOCKFILE_VERSION,
            ));
        }
        if let Some((key, _)) = self
            .packages
            .iter()
            .find(|(_, package)| package.registry_integrity_declared == Some(false))
        {
            return Err(LockfileError::UnverifiedRegistryArtifact(key.clone()));
        }
        Ok(())
    }

    fn validate_alias_schema(&self) -> Result<(), LockfileError> {
        if self.lockfile_version < LIFECYCLE_LOCKFILE_VERSION
            && self
                .packages
                .values()
                .any(|package| !package.derived_hooks.is_empty())
        {
            return Err(LockfileError::InvalidDerivedOutput(
                "derived hooks require schema 9".into(),
            ));
        }
        if self.lockfile_version < COPIED_SOURCE_LOCKFILE_VERSION
            && self.packages.values().any(|package| {
                package
                    .source()
                    .is_some_and(|source| source.registry().is_none())
            })
        {
            return Err(LockfileError::UnsupportedVersion(self.lockfile_version));
        }
        for package in self.packages.values() {
            validate_derived_hooks(&package.derived_hooks)?;
        }
        if self.lockfile_version < LOCKFILE_VERSION
            && (!self.root_bindings.is_empty()
                || self
                    .packages
                    .values()
                    .any(|package| !package.dependency_aliases.is_empty()))
        {
            return Err(LockfileError::AliasMetadataInLegacySchema(
                self.lockfile_version,
            ));
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, LockfileError> {
        self.validate_alias_schema()?;
        if self.lockfile_version >= ROOTS_LEGACY_LOCKFILE_VERSION
            && (!self.packages.is_empty() || !self.workspace_packages.is_empty())
            && self.roots.is_empty()
        {
            return Err(LockfileError::MissingRoots);
        }
        if self.lockfile_version >= ROOTS_LEGACY_LOCKFILE_VERSION
            && let Some((key, _)) = self.packages.iter().find(|(_, package)| {
                package
                    .source()
                    .is_some_and(|source| source.registry().is_some())
                    && package.registry_integrity_declared.is_none()
            })
        {
            return Err(LockfileError::MissingRegistryIntegrityProvenance(
                key.clone(),
            ));
        }
        serde_json::to_string_pretty(self)
            .map(|json| format!("{json}\n"))
            .map_err(LockfileError::Serialization)
    }

    pub fn from_json(input: &str) -> Result<Self, LockfileError> {
        let mut lockfile: Self =
            serde_json::from_str(input).map_err(LockfileError::Serialization)?;
        if lockfile.lockfile_version != COPIED_SOURCE_LOCKFILE_VERSION
            && lockfile.lockfile_version != LOCKFILE_VERSION
            && lockfile.lockfile_version != LIFECYCLE_LOCKFILE_VERSION
            && lockfile.lockfile_version != LEGACY_LOCKFILE_VERSION
            && lockfile.lockfile_version != PROVENANCE_LEGACY_LOCKFILE_VERSION
            && lockfile.lockfile_version != REGISTRY_ONLY_LOCKFILE_VERSION
        {
            return Err(LockfileError::UnsupportedVersion(lockfile.lockfile_version));
        }
        lockfile.root_manifest_digest = canonical_artifact_digest(&lockfile.root_manifest_digest)?;
        if lockfile.lockfile_version >= PROVENANCE_LEGACY_LOCKFILE_VERSION
            && (!lockfile.packages.is_empty() || !lockfile.workspace_packages.is_empty())
            && lockfile.roots.is_empty()
        {
            return Err(LockfileError::MissingRoots);
        }
        if lockfile.lockfile_version >= REGISTRY_ONLY_LOCKFILE_VERSION
            && let Some((key, _)) = lockfile.packages.iter().find(|(_, package)| {
                package
                    .source()
                    .is_some_and(|source| source.registry().is_some())
                    && package.registry_integrity_declared.is_none()
            })
        {
            return Err(LockfileError::MissingRegistryIntegrityProvenance(
                key.clone(),
            ));
        }
        let mut canonical_roots = lockfile.roots.clone();
        canonical_roots.sort();
        canonical_roots.dedup();
        if canonical_roots != lockfile.roots {
            return Err(LockfileError::NonCanonicalRoots);
        }
        if lockfile.lockfile_version < LOCKFILE_VERSION && !lockfile.workspace_packages.is_empty() {
            return Err(LockfileError::WorkspaceIdentityRequiresCurrentVersion);
        }
        for root in &lockfile.roots {
            root.parse::<LockfilePackageKey>()?;
            if !lockfile.contains_package_key(root) {
                return Err(LockfileError::DanglingRoot(root.clone()));
            }
        }
        lockfile.validate_alias_schema()?;
        if !lockfile.root_bindings.is_empty() {
            lockfile.set_root_bindings(lockfile.root_bindings.clone())?;
        }
        for (key, package) in &lockfile.packages {
            key.parse::<LockfilePackageKey>()?;
            package.validate()?;
            if key != &package.key() {
                return Err(LockfileError::PackageKeyMismatch(key.clone()));
            }
        }
        for (key, package) in &lockfile.workspace_packages {
            key.parse::<LockfilePackageKey>()?;
            package.validate()?;
            if key != &package.key() {
                return Err(LockfileError::PackageKeyMismatch(key.clone()));
            }
        }
        let exists = |key: &str| lockfile.contains_package_key(key);
        for (key, package) in &lockfile.packages {
            for (name, dependency) in &package.dependencies {
                validate_dependency_target(
                    key,
                    name,
                    dependency,
                    &package.dependency_aliases,
                    exists,
                )?;
            }
        }
        for (key, package) in &lockfile.workspace_packages {
            for (name, dependency) in &package.dependencies {
                validate_dependency_target(
                    key,
                    name,
                    dependency,
                    &package.dependency_aliases,
                    exists,
                )?;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registry_integrity_declared: Option<bool>,
    unpacked_digest: String,
    /// Explicit replay identity for the verified unpacked store tree.
    tree_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    derived_hooks: Vec<DerivedHookOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_url: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    peer_context: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    platform_context: String,
    dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    dependency_aliases: BTreeMap<String, String>,
}

/// A generated verified tree bound to one hook's complete derivation identity.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DerivedHookOutput {
    attestation: String,
    hook: String,
    key: String,
    tree_digest: String,
}
impl DerivedHookOutput {
    pub fn new(
        hook: &str,
        key: &str,
        tree_digest: &str,
        attestation: &str,
    ) -> Result<Self, LockfileError> {
        let output = Self {
            attestation: attestation.into(),
            hook: hook.into(),
            key: key.into(),
            tree_digest: tree_digest.into(),
        };
        validate_derived_hooks(std::slice::from_ref(&output))?;
        Ok(output)
    }
    pub fn attestation(&self) -> &str {
        &self.attestation
    }
    pub fn hook(&self) -> &str {
        &self.hook
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }
}
fn validate_derived_hooks(outputs: &[DerivedHookOutput]) -> Result<(), LockfileError> {
    let mut previous = None;
    for output in outputs {
        let tag = output
            .attestation
            .strip_prefix("hmac-sha256-")
            .ok_or_else(|| {
                LockfileError::InvalidDerivedOutput(
                    "missing lifecycle output authentication".into(),
                )
            })?;
        if tag.len() != 64
            || !tag
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(LockfileError::InvalidDerivedOutput(
                "invalid lifecycle output authentication".into(),
            ));
        }
        let rank = match output.hook.as_str() {
            "preinstall" => 0,
            "install" => 1,
            "postinstall" => 2,
            _ => {
                return Err(LockfileError::InvalidDerivedOutput(
                    "unsupported hook".into(),
                ));
            }
        };
        if previous.is_some_and(|previous| previous >= rank) {
            return Err(LockfileError::InvalidDerivedOutput(
                "duplicate or unordered hooks".into(),
            ));
        }
        previous = Some(rank);
        for digest in [&output.key, &output.tree_digest] {
            if canonical_artifact_digest(digest)? != *digest || !digest.starts_with("sha256-") {
                return Err(LockfileError::InvalidDerivedOutput(
                    "noncanonical digest".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Identifies the source of a package artifact integrity value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryIntegrityProvenance {
    /// The registry metadata declared the verified integrity value.
    RegistryDeclared,
    /// Tapid computed the value locally under an explicit compatibility exception.
    LocallyComputed,
}

impl LockedPackage {
    pub fn new_with_provenance(
        registry: &str,
        name: &str,
        version: &str,
        artifact_integrity: &str,
        unpacked_digest: &str,
        integrity_provenance: RegistryIntegrityProvenance,
    ) -> Result<Self, LockfileError> {
        Self::new_with_context_and_provenance(
            registry,
            name,
            version,
            artifact_integrity,
            unpacked_digest,
            (
                &PeerContext::default(),
                &PlatformContext::new(None, None, None).unwrap(),
            ),
            integrity_provenance,
        )
    }

    pub fn new_with_context_and_provenance(
        registry: &str,
        name: &str,
        version: &str,
        artifact_integrity: &str,
        unpacked_digest: &str,
        contexts: (&PeerContext, &PlatformContext),
        integrity_provenance: RegistryIntegrityProvenance,
    ) -> Result<Self, LockfileError> {
        let package = Self {
            registry: registry
                .parse::<PackageSource>()
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
            registry_integrity_declared: registry.parse::<PackageSource>().ok().and_then(
                |source| {
                    source.registry().map(|_| {
                        matches!(
                            integrity_provenance,
                            RegistryIntegrityProvenance::RegistryDeclared
                        )
                    })
                },
            ),
            unpacked_digest: unpacked_digest
                .parse::<ArtifactDigest>()
                .map_err(LockfileError::Domain)?
                .to_string(),
            tree_digest: unpacked_digest.to_owned(),
            derived_hooks: Vec::new(),
            artifact_url: None,
            peer_context: canonical_peer_context(contexts.0),
            platform_context: canonical_platform_context(contexts.1),
            dependencies: BTreeMap::new(),
            dependency_aliases: BTreeMap::new(),
        };
        package.validate()?;
        Ok(package)
    }

    pub fn key(&self) -> String {
        let registry: PackageSource = self.registry.parse().expect("validated package source");
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

    /// Pinned HTTPS archive address, when recorded by the producer.
    pub fn artifact_url(&self) -> Option<&str> {
        self.artifact_url.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn source(&self) -> Option<PackageSource> {
        self.registry.parse().ok()
    }
    pub fn registry(&self) -> &str {
        &self.registry
    }
    pub fn has_declared_registry_integrity(&self) -> bool {
        self.registry_integrity_declared == Some(true)
    }
    /// Canonical SHA-512 archive integrity recorded in the lock.
    pub fn artifact_integrity(&self) -> &str {
        &self.artifact_integrity
    }
    pub fn derived_hooks(&self) -> &[DerivedHookOutput] {
        &self.derived_hooks
    }
    /// Tree activated for installation after policy validation by the caller.
    pub fn install_tree_digest(&self) -> &str {
        self.derived_hooks
            .last()
            .map_or(&self.tree_digest, |output| output.tree_digest())
    }

    /// Whether the producer verified registry-declared archive integrity.
    pub fn registry_integrity_declared(&self) -> Option<bool> {
        self.registry_integrity_declared
    }

    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }

    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.dependencies
    }

    fn validate(&self) -> Result<(), LockfileError> {
        let registry = self
            .registry
            .parse::<PackageSource>()
            .map_err(LockfileError::Domain)?;
        if registry.as_str() != self.registry {
            return Err(LockfileError::NonCanonicalRegistryIdentity);
        }
        self.name
            .parse::<PackageName>()
            .map_err(LockfileError::Domain)?;
        self.version
            .parse::<PackageVersion>()
            .map_err(LockfileError::Domain)?;
        let artifact_integrity = self
            .artifact_integrity
            .parse::<PackageIntegrity>()
            .map_err(LockfileError::Domain)?;
        if artifact_integrity.to_string() != self.artifact_integrity {
            return Err(LockfileError::InvalidSha512(
                self.artifact_integrity.clone(),
            ));
        }
        self.unpacked_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?;
        self.tree_digest
            .parse::<ArtifactDigest>()
            .map_err(LockfileError::Domain)?;
        let key = self.key();
        validate_peer_context(&self.peer_context, &key)?;
        validate_platform_context(&self.platform_context, &key)?;
        if registry.registry().is_some() {
            validation::validate_registry_url(&self.registry)?;
        } else if self.registry_integrity_declared.is_some()
            || self.artifact_url.is_some()
            || !self.derived_hooks.is_empty()
        {
            return Err(LockfileError::InvalidCopiedArtifactEvidence);
        }
        if let Some(url) = &self.artifact_url {
            validation::validate_artifact_url(url)?;
        }
        for (name, actual) in &self.dependency_aliases {
            name.parse::<PackageName>().map_err(LockfileError::Domain)?;
            actual
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?;
            let target = self
                .dependencies
                .get(name)
                .ok_or_else(|| LockfileError::DependencyNameMismatch {
                    package: self.key(),
                    dependency: name.clone(),
                    target: actual.clone(),
                })?
                .parse::<LockfilePackageKey>()?;
            if actual != target.name.as_str() || name == actual {
                return Err(LockfileError::DependencyNameMismatch {
                    package: self.key(),
                    dependency: name.clone(),
                    target: target.name.to_string(),
                });
            }
        }
        for name in self.dependencies.keys() {
            name.parse::<PackageName>().map_err(LockfileError::Domain)?;
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
        self.dependency_aliases.remove(name.as_str());
        Ok(())
    }

    /// Binds a validated local alias to an exact actual registry package.
    pub fn add_alias_dependency(&mut self, name: &str, key: &str) -> Result<(), LockfileError> {
        let name = name
            .parse::<PackageName>()
            .map_err(LockfileError::Domain)?
            .to_string();
        let target = key.parse::<LockfilePackageKey>()?;
        if key == self.key() {
            return Err(LockfileError::SelfDependency(key.to_owned()));
        }
        if name != target.name.as_str() {
            self.dependency_aliases
                .insert(name.clone(), target.name.to_string());
        } else {
            self.dependency_aliases.remove(&name);
        }
        self.dependencies.insert(name, key.to_owned());
        Ok(())
    }
}
