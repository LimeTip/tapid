//! Complete native selections. These are deliberately not legacy verified locks.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName};

use crate::{LockedPackage, LockfileError, LockfilePackageKey, validation};

pub const PROFILE_LOCKFILE_VERSION: u32 = 11;

/// Invocation-local materialization policy; it never changes the accepted selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallProfile {
    pub include_dev: bool,
    pub include_optional: bool,
}

impl Default for InstallProfile {
    fn default() -> Self {
        Self {
            include_dev: true,
            include_optional: true,
        }
    }
}

impl InstallProfile {
    pub fn production() -> Self {
        Self {
            include_dev: false,
            ..Self::default()
        }
    }

    fn enables(self, kind: DeclarationKind) -> bool {
        match kind {
            DeclarationKind::Required => true,
            DeclarationKind::Dev => self.include_dev,
            DeclarationKind::Optional => self.include_optional,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeclarationKind {
    Required,
    Dev,
    Optional,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DependencyKind {
    Required,
    Optional,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declaration {
    pub workspace: Option<String>,
    pub name: String,
    pub kind: DeclarationKind,
    pub target: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub kind: DependencyKind,
    pub target: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum SourceEvidence {
    #[serde(rename_all = "camelCase")]
    Pending {
        artifact_url: String,
        artifact_integrity: String,
    },
    Verified {
        package: Box<LockedPackage>,
    },
    Workspace {
        source: crate::LocalWorkspaceSource,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedPackage {
    pub source_evidence: SourceEvidence,
    pub dependencies: BTreeMap<String, Dependency>,
    pub peer_providers: BTreeMap<String, String>,
}

/// Validated schema-11 native selection, including packages omitted from a profile.
///
/// Parsing validates the complete graph before reachability is exposed. The legacy
/// `Lockfile` reader intentionally does not accept this representation.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSelection {
    lockfile_version: u32,
    resolver_version: String,
    linker_version: String,
    root_manifest_digest: String,
    workspace_manifest_digests: BTreeMap<String, String>,
    declarations: Vec<Declaration>,
    packages: BTreeMap<String, SelectedPackage>,
}

impl NativeSelection {
    /// Construct a complete selection from typed producer evidence, validating all records.
    pub fn new(
        resolver_version: String,
        linker_version: String,
        root_manifest_digest: String,
        workspace_manifest_digests: BTreeMap<String, String>,
        declarations: Vec<Declaration>,
        packages: BTreeMap<String, SelectedPackage>,
    ) -> Result<Self, LockfileError> {
        let selection = Self {
            lockfile_version: PROFILE_LOCKFILE_VERSION,
            resolver_version,
            linker_version,
            root_manifest_digest,
            workspace_manifest_digests,
            declarations,
            packages,
        };
        selection.validate()?;
        Ok(selection)
    }

    pub fn from_json(input: &str) -> Result<Self, LockfileError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            lockfile_version: u32,
            resolver_version: String,
            linker_version: String,
            root_manifest_digest: String,
            workspace_manifest_digests: BTreeMap<String, String>,
            declarations: Vec<Declaration>,
            packages: BTreeMap<String, SelectedPackage>,
        }
        let value = crate::selection_json::parse(input).map_err(LockfileError::Serialization)?;
        let wire: Wire = serde_json::from_value(value).map_err(LockfileError::Serialization)?;
        let selection = Self {
            lockfile_version: wire.lockfile_version,
            resolver_version: wire.resolver_version,
            linker_version: wire.linker_version,
            root_manifest_digest: wire.root_manifest_digest,
            workspace_manifest_digests: wire.workspace_manifest_digests,
            declarations: wire.declarations,
            packages: wire.packages,
        };
        selection.validate()?;
        Ok(selection)
    }

    /// Complete validated records, including selections omitted from installation.
    pub fn packages(&self) -> &BTreeMap<String, SelectedPackage> {
        &self.packages
    }

    pub fn to_json(&self) -> Result<String, LockfileError> {
        serde_json::to_string_pretty(self).map_err(LockfileError::Serialization)
    }

    /// Exact retained keys, including required providers and their dependencies.
    /// This does not fetch artifacts or produce a destructively filtered lock.
    pub fn materialized_keys(
        &self,
        profile: InstallProfile,
    ) -> Result<BTreeSet<String>, LockfileError> {
        let mut pending: Vec<_> = self
            .declarations
            .iter()
            .filter(|edge| profile.enables(edge.kind))
            .map(|edge| edge.target.clone())
            .collect();
        let mut visited = BTreeSet::new();
        while let Some(key) = pending.pop() {
            if !visited.insert(key.clone()) {
                continue;
            }
            let package = self
                .packages
                .get(&key)
                .ok_or_else(|| LockfileError::DanglingRoot(key.clone()))?;
            pending.extend(
                package
                    .dependencies
                    .values()
                    .filter(|edge| {
                        matches!(edge.kind, DependencyKind::Required) || profile.include_optional
                    })
                    .map(|edge| edge.target.clone()),
            );
            pending.extend(package.peer_providers.values().cloned());
        }
        Ok(visited)
    }

    fn validate(&self) -> Result<(), LockfileError> {
        if self.lockfile_version != PROFILE_LOCKFILE_VERSION {
            return Err(LockfileError::UnsupportedVersion(self.lockfile_version));
        }
        validate_digest(&self.root_manifest_digest)?;
        for (path, digest) in &self.workspace_manifest_digests {
            // Reuse the workspace source path contract rather than another normalizer.
            let source = crate::LocalWorkspaceSource::new(path, "workspace", "0.0.0")?;
            if source.path() != path {
                return invalid("noncanonical workspace path");
            }
            validate_digest(digest)?;
        }
        let mut bindings = BTreeMap::new();
        let mut declarations = BTreeSet::new();
        for edge in &self.declarations {
            edge.name
                .parse::<PackageName>()
                .map_err(LockfileError::Domain)?;
            if edge
                .workspace
                .as_ref()
                .is_some_and(|path| !self.workspace_manifest_digests.contains_key(path))
            {
                return invalid("declaration references unknown workspace");
            }
            if !self.packages.contains_key(&edge.target) {
                return Err(LockfileError::DanglingRoot(edge.target.clone()));
            }
            let binding = (edge.workspace.as_deref(), edge.name.as_str());
            if bindings
                .insert(binding, &edge.target)
                .is_some_and(|target| target != &edge.target)
            {
                return invalid("conflicting declaration binding");
            }
            let overridden = match edge.kind {
                DeclarationKind::Required => declarations.contains(&(binding, 2)),
                DeclarationKind::Optional => declarations.contains(&(binding, 0)),
                DeclarationKind::Dev => false,
            };
            if overridden {
                return invalid("optional override must be applied before recording declarations");
            }
            if !declarations.insert((
                binding,
                match edge.kind {
                    DeclarationKind::Required => 0,
                    DeclarationKind::Dev => 1,
                    DeclarationKind::Optional => 2,
                },
            )) {
                return invalid("duplicate declaration");
            }
        }
        for (key, package) in &self.packages {
            let identity = key.parse::<LockfilePackageKey>()?;

            match &package.source_evidence {
                SourceEvidence::Workspace { source } => {
                    if LockfilePackageKey::workspace(source.clone()).to_string() != *key
                        || !self.workspace_manifest_digests.contains_key(source.path())
                    {
                        return invalid("workspace evidence disagrees with selection");
                    }
                }

                SourceEvidence::Pending {
                    artifact_url,
                    artifact_integrity,
                } => {
                    if identity.source.registry().is_none() {
                        return invalid("pending evidence requires a registry selection");
                    }
                    validation::validate_artifact_url(artifact_url)?;
                    let integrity = artifact_integrity
                        .parse::<PackageIntegrity>()
                        .map_err(LockfileError::Domain)?;
                    if integrity.to_string() != *artifact_integrity {
                        return Err(LockfileError::InvalidSha512(artifact_integrity.clone()));
                    }
                }
                SourceEvidence::Verified { package: evidence } => {
                    evidence.validate()?;
                    crate::model::validate_derived_hooks(evidence.derived_hooks())?;
                    if evidence.key() != *key {
                        return Err(LockfileError::PackageKeyMismatch(key.clone()));
                    }
                    if identity.source.registry().is_some()
                        && !evidence.has_declared_registry_integrity()
                    {
                        return Err(LockfileError::UnverifiedRegistryArtifact(key.clone()));
                    }
                    // Typed selection edges must cover exactly the old verified dependency map.
                    let targets: BTreeMap<_, _> = package
                        .dependencies
                        .iter()
                        .map(|(name, edge)| (name.clone(), edge.target.clone()))
                        .collect();
                    if evidence.dependencies() != &targets {
                        return invalid("verified dependency evidence disagrees with selection");
                    }
                }
            }
            for (name, edge) in &package.dependencies {
                name.parse::<PackageName>().map_err(LockfileError::Domain)?;
                if !self.packages.contains_key(&edge.target) {
                    return Err(LockfileError::DanglingDependency {
                        package: key.clone(),
                        dependency: name.clone(),
                    });
                }
            }
            let context = identity.peer_context()?;
            if context.entries().len() != package.peer_providers.len() {
                return invalid("peer provider evidence does not cover exact context");
            }
            for (name, target) in &package.peer_providers {
                let name = name.parse::<PackageName>().map_err(LockfileError::Domain)?;
                let provider = target.parse::<LockfilePackageKey>()?;
                if provider.name != name
                    || !self.packages.contains_key(target)
                    || context.entries().get(&name) != Some(&provider.version)
                {
                    return invalid("invalid exact peer provider");
                }
            }
        }
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), LockfileError> {
    let digest = value
        .parse::<ArtifactDigest>()
        .map_err(LockfileError::Domain)?;
    if digest.to_string() != value || !value.starts_with("sha256-") {
        return invalid("noncanonical selection digest");
    }
    Ok(())
}

fn invalid<T>(reason: &str) -> Result<T, LockfileError> {
    Err(LockfileError::InvalidProfileSelection(reason.to_owned()))
}
