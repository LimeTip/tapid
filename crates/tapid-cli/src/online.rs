mod artifact;
mod copied;
mod graph;
mod preview;
pub(crate) use copied::{
    copied_root_names, declaration as copied_declaration, validate_copied_root_binding,
    validate_locked_files,
};
pub(crate) use preview::{GraphPackage, GraphSnapshot, resolve as resolve_preview_graph};
mod locked;
use crate::application::outcome::{ErrorKind, OperationalError};
pub(crate) use locked::{
    PreparedLockedInstall, hydrate_locked, prepare_locked_install,
    validate_locked_artifact_sources, validate_locked_routes,
};
mod npm_import;
pub(crate) use npm_import::{fetch_imported, imported_workspace_manifests};
mod resolution;
mod update;
#[cfg(test)]
use resolution::{
    RESOLVER_METADATA_BUILD_COUNT, RESOLVER_METADATA_PARENT_VISITS,
    RESOLVER_METADATA_VERSION_VISITS, metadata_progress_checkpoint, normalize_record,
    parse_registry_requirement, resolve_with_fetch, resolve_with_fetch_routed,
    resolve_with_overrides,
};
pub(crate) use resolution::{manifest_overrides, manifest_roots};

mod routing;
pub(crate) use routing::metadata_transport_for_package;
#[cfg(test)]
use routing::transport_for_route;
use routing::{artifact_transport_for_package, registry_for_requirement};

mod platform;
mod selection;
use platform::current_platform_matches;
pub(crate) use platform::{current_libc, selection_target};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tapid_archive::{ArchiveFormat, ArchiveLimits, canonical_tree_digest, extract_to};
use tapid_core::{
    ArtifactDigest, PackageIntegrity, PackageName, PackageSource, PackageVersion, RegistryOrigin,
};
use tapid_linker::{
    InstanceKey, NamedDependency, NamedDependencyEdge, NamedLayoutInput, PackageInstance,
    VerifiedTreeReference, WorkspaceLinkPlan, WorkspacePackage, plan_workspace_links,
};
use tapid_lockfile::{
    LocalWorkspaceSource, LockedPackage, LockedWorkspacePackage, Lockfile, LockfilePackageKey,
    RegistryIntegrityProvenance,
};
use tapid_manifest::{PackageManifest, Workspace};
use tapid_registry_client::{
    HttpsTransport, JsrRegistry, NpmRegistry, PackagePlatform, RegistryArtifact,
};
use tapid_resolver::{
    Dependency, PackageVersionMetadata, RegistryMetadata, Requirement, Resolution,
    ResolutionOptions, ResolveError,
};
use tapid_store::{Store, StoreTransaction};

type PackageRecordKey = (String, String, String);

const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";
static NEXT_TEMP_TREE_ID: AtomicU64 = AtomicU64::new(0);

type ResolveAndFetchOutput = Result<PreparedInstall, OperationalError>;

pub(crate) struct PreparedInstall {
    pub candidate_exclusions: crate::application::outcome::CandidateExclusions,
    pub lock: Lockfile,
    pub layout: PreparedLayout,
    pub transaction: StoreTransaction,
}

/// All paths that must move together when transaction trees are published.
pub(crate) struct PreparedLayout {
    pub input: NamedLayoutInput,
    pub trees: BTreeMap<String, PathBuf>,
    pub workspace: WorkspaceInstallPlan,
}
impl PreparedLayout {
    pub(crate) fn rebase(&mut self, publication: &tapid_store::StorePublication) {
        for path in self.trees.values_mut() {
            *path = publication.resolve_path(path);
        }
        for input in std::iter::once(&mut self.input).chain(
            self.workspace
                .members
                .values_mut()
                .map(|member| &mut member.input),
        ) {
            for instance in &mut input.instances {
                instance.tree.root = publication.resolve_path(&instance.tree.root);
            }
        }
    }
}

/// Layouts activated together under the root lifecycle journal.
#[derive(Default)]
pub(crate) struct WorkspaceInstallPlan {
    pub links: WorkspaceLinkPlan,
    pub members: BTreeMap<PathBuf, WorkspaceMemberInstall>,
}
pub(crate) struct WorkspaceMemberInstall {
    pub input: NamedLayoutInput,
    pub links: WorkspaceLinkPlan,
}
impl WorkspaceInstallPlan {
    /// Clear prior member layouts when converting an imported lock to native resolution.
    pub(crate) fn include_owned_members(&mut self) -> Result<(), String> {
        for link in &self.links.links {
            if fs::symlink_metadata(link.source.join(".tapid-managed")).is_ok() {
                crate::filesystem::activation::validate_managed_node_modules(&link.source)?;
                self.members
                    .entry(link.source.clone())
                    .or_insert_with(|| WorkspaceMemberInstall {
                        input: NamedLayoutInput {
                            instances: Vec::new(),
                            root_dependencies: Vec::new(),
                            dependency_edges: Vec::new(),
                        },
                        links: WorkspaceLinkPlan::default(),
                    });
            }
        }
        Ok(())
    }
}
impl From<WorkspaceLinkPlan> for WorkspaceInstallPlan {
    fn from(links: WorkspaceLinkPlan) -> Self {
        Self {
            links,
            members: BTreeMap::new(),
        }
    }
}

struct TemporaryTree(PathBuf);

impl Drop for TemporaryTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Deserialize, Clone)]
struct FixturePackage {
    #[serde(default)]
    git_ref: Option<String>,
    #[serde(default)]
    dist_tags: BTreeSet<String>,
    registry: String,
    name: String,
    version: String,
    #[serde(default)]
    integrity: Option<String>,
    artifact: String,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalPeerDependencies")]
    optional_peer_dependencies: BTreeSet<String>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    os: Vec<String>,
    #[serde(default)]
    cpu: Vec<String>,
    #[serde(default)]
    libc: Vec<String>,
}
#[derive(Debug, Deserialize)]
struct Fixture {
    packages: Vec<FixturePackage>,
}

#[derive(Clone)]
struct PackageRecord {
    git_reference: Option<String>,
    copied_archive: Option<std::sync::Arc<Vec<u8>>>,
    dist_tags: BTreeSet<String>,
    registry: PackageSource,
    name: PackageName,
    version: PackageVersion,
    integrity: Option<PackageIntegrity>,
    artifact: String,
    dependencies: BTreeMap<String, String>,
    peer_dependencies: BTreeMap<String, String>,
    optional_peer_dependencies: BTreeSet<String>,
    optional_dependencies: BTreeMap<String, String>,
    platform: PackagePlatform,
    fixture: bool,
}

fn digest(data: &[u8]) -> ArtifactDigest {
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256-{}", hex::encode(h.finalize()))
        .parse()
        .expect("sha256 digest")
}
pub(crate) fn integrity(data: &[u8]) -> PackageIntegrity {
    let mut h = Sha512::new();
    h.update(data);
    format!("sha512-{}", STANDARD.encode(h.finalize()))
        .parse()
        .expect("sha512 integrity")
}

fn integrity_matches(expected: &PackageIntegrity, actual: &PackageIntegrity) -> bool {
    expected == actual
}
pub(crate) fn root_digest(project: &Path) -> Result<String, String> {
    let data = fs::read(project.join("package.json")).map_err(|e| e.to_string())?;
    Ok(digest(&data).to_string())
}

pub(crate) fn workspace_requirement(
    dependency: &str,
    version: &PackageVersion,
) -> Result<Requirement, String> {
    let range = if let Some(protocol) = dependency.strip_prefix("workspace:") {
        match protocol {
            "*" => "*".to_owned(),
            "^" => format!("^{version}"),
            "~" => format!("~{version}"),
            _ => {
                return Err(format!(
                    "unsupported workspace protocol '{dependency}'; supported compatibility forms are workspace:*, workspace:^, and workspace:~"
                ));
            }
        }
    } else {
        dependency.to_owned()
    };
    range
        .parse::<Requirement>()
        .map_err(|error| format!("invalid workspace dependency range '{dependency}': {error}"))
}

#[derive(Clone)]
pub(crate) struct WorkspaceRegistryDependency {
    pub member_key: String,
    pub section: tapid_resolver::EntryPointSection,
    pub manifest_name: String,
    pub registry: PackageSource,
    pub package: PackageName,
    pub requirement: Requirement,
}

#[derive(Clone)]
pub(crate) enum WorkspacePeerProvider {
    Workspace {
        key: String,
        version: PackageVersion,
    },
    Registry {
        registry: PackageSource,
        package: PackageName,
    },
}

#[derive(Clone)]
pub(crate) struct WorkspacePeerDependency {
    pub member_key: String,
    pub manifest_name: String,
    pub requirement: Requirement,
    pub provider: WorkspacePeerProvider,
}

pub(crate) struct WorkspaceMaterialization {
    pub manifests: BTreeMap<String, PackageManifest>,
    pub links: WorkspaceLinkPlan,
    pub locked: Vec<LockedWorkspacePackage>,
    pub members: BTreeMap<String, (LocalWorkspaceSource, PackageVersion)>,
    pub registry_dependencies: Vec<WorkspaceRegistryDependency>,
    pub peer_dependencies: Vec<WorkspacePeerDependency>,
    pub declarations: Vec<crate::application::outcome::RequirementDiagnostic>,
}

pub(crate) fn workspace_materialization(
    project: &Path,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<WorkspaceMaterialization, OperationalError> {
    workspace_materialization_with_manifest(project, registry_config, None)
}

pub(super) fn workspace_materialization_with_manifest(
    project: &Path,
    registry_config: &crate::registry::RegistryConfig,
    replacement: Option<(&Path, &PackageManifest)>,
) -> Result<WorkspaceMaterialization, OperationalError> {
    let workspace = Workspace::discover_typed(project)
        .map_err(|error| OperationalError::from_source(ErrorKind::InvalidData, error))?;
    workspace_materialization_from_discovered(project, registry_config, replacement, &workspace)
}

/// Plan local links, lock receipts, and member declarations from an existing discovery.
/// `replacement` substitutes one selected manifest for lifecycle planning. This
/// checks root identity and reads bounded member bytes, but does not fetch packages,
/// write links, or repeat workspace enumeration.
pub(crate) fn workspace_materialization_from_discovered(
    project: &Path,
    registry_config: &crate::registry::RegistryConfig,
    replacement: Option<(&Path, &PackageManifest)>,
    workspace: &Workspace,
) -> Result<WorkspaceMaterialization, OperationalError> {
    let root = workspace
        .root_path()
        .parent()
        .ok_or("workspace root manifest has no parent")?;
    let canonical_project = fs::canonicalize(project)
        .map_err(|error| format!("cannot resolve install project root: {error}"))?;
    if root != canonical_project {
        return Err("resolved workspace root does not match install project root".into());
    }
    let mut packages = Vec::new();
    let mut locked = Vec::new();
    let mut members = BTreeMap::new();
    let mut manifests = BTreeMap::new();
    let mut locked_by_name = BTreeMap::new();
    let mut registry_dependencies = Vec::new();
    let mut peer_dependencies = Vec::new();
    let mut declarations = Vec::new();
    for member in workspace.members() {
        let manifest = replacement
            .filter(|(path, _)| *path == member.path())
            .map_or(member.manifest(), |(_, manifest)| manifest);
        declarations.extend(
            crate::application::outcome::DiagnosticContext::manifest_requirements(
                manifest,
                member.path(),
                Some(member.name()),
            ),
        );
        let member_root = member
            .path()
            .parent()
            .ok_or("workspace member manifest has no parent")?;
        let canonical = fs::canonicalize(member_root).map_err(|error| {
            format!(
                "cannot resolve workspace member {}: {error}",
                member_root.display()
            )
        })?;
        if !canonical.starts_with(root) || canonical == root {
            return Err(format!(
                "workspace member escapes workspace root: {}",
                member_root.display()
            )
            .into());
        }
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| "workspace member path is outside workspace root")?;
        let relative_posix = relative
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .ok_or_else(|| "workspace member path is not valid UTF-8".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let version = manifest.version().clone();
        let source =
            LocalWorkspaceSource::new(&relative_posix, member.name(), &version.to_string())
                .map_err(|error| error.to_string())?;
        let manifest_bytes = if replacement.is_some_and(|(path, _)| path == member.path()) {
            manifest.to_json().into_bytes()
        } else {
            crate::filesystem::read::read_bounded_file(
                member.path(),
                tapid_manifest::MAX_MANIFEST_BYTES,
            )
            .map_err(|_| {
                format!(
                    "workspace member manifest must be a regular file no larger than 32 MiB: {}",
                    member.path().display()
                )
            })?
        };
        locked_by_name.insert(member.name().to_owned(), locked.len());
        locked.push(
            LockedWorkspacePackage::new(source.clone(), &digest(&manifest_bytes).to_string())
                .map_err(|error| error.to_string())?,
        );
        manifests.insert(
            LockfilePackageKey::workspace(source.clone()).to_string(),
            manifest.clone(),
        );
        packages.push(WorkspacePackage {
            root: relative.to_path_buf(),
            name: member
                .name()
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?,
            version: version.clone(),
        });
        members.insert(member.name().to_owned(), (source, version));
    }
    for member in workspace.members() {
        let manifest = replacement
            .filter(|(path, _)| *path == member.path())
            .map_or(member.manifest(), |(_, manifest)| manifest);
        let package_index = *locked_by_name
            .get(member.name())
            .ok_or("workspace member has no lockfile identity")?;
        for (name, range) in manifest.peer_dependencies() {
            let (requirement, provider) = if let Some((source, version)) = members.get(name) {
                let requirement = workspace_requirement(range, version)?;
                if !requirement.matches(version) {
                    return Err(format!(
                        "workspace member peer dependency '{}@{}' in '{}' does not match local provider version {}",
                        name,
                        range,
                        member.name(),
                        version
                    ).into());
                }
                (
                    requirement,
                    WorkspacePeerProvider::Workspace {
                        key: LockfilePackageKey::workspace(source.clone()).to_string(),
                        version: version.clone(),
                    },
                )
            } else {
                if range.starts_with("workspace:") {
                    return Err(format!(
                        "workspace peer dependency '{}' in member '{}' has no local workspace provider; refusing registry fallback",
                        name,
                        member.name()
                    ).into());
                }
                let (registry, package) = dependency_identity(registry_config, name)?;
                if copied::declaration(range)
                    .map_err(|reason| format!("dependency '{name}': {reason}"))?
                    .is_some()
                {
                    return Err(format!(
                        "copied dependency '{name}' in a workspace member is unsupported; declare copied artifacts at the project root"
                    ).into());
                }
                let requirement = range.parse::<Requirement>().map_err(|error| {
                    format!(
                        "invalid workspace member peer dependency '{name}' range '{range}': {error}"
                    )
                })?;
                (
                    requirement,
                    WorkspacePeerProvider::Registry {
                        registry: registry.into(),
                        package,
                    },
                )
            };
            peer_dependencies.push(WorkspacePeerDependency {
                member_key: locked[package_index].key(),
                manifest_name: name.clone(),
                requirement,
                provider,
            });
        }
        for (section, dependency_map) in [
            (
                tapid_resolver::EntryPointSection::Dependencies,
                manifest.dependencies(),
            ),
            (
                tapid_resolver::EntryPointSection::DevDependencies,
                manifest.dev_dependencies(),
            ),
            (
                tapid_resolver::EntryPointSection::OptionalDependencies,
                manifest.optional_dependencies(),
            ),
        ] {
            for (name, range) in dependency_map {
                let Some((source, version)) = members.get(name) else {
                    if range.starts_with("workspace:") {
                        return Err(format!(
                            "workspace dependency '{}' in member '{}' has no local workspace target; refusing registry fallback",
                            name,
                            member.name()
                        ).into());
                    }
                    if copied::declaration(range)
                        .map_err(|reason| format!("dependency '{name}': {reason}"))?
                        .is_some()
                    {
                        return Err(format!(
                            "copied workspace member dependency '{name}' is unsupported; declare copied artifacts at the project root"
                        ).into());
                    }
                    let requirement = range.parse::<Requirement>().map_err(|error| {
                        format!(
                            "invalid workspace member dependency '{name}' range '{range}': {error}"
                        )
                    })?;
                    let (manifest_registry, local_package) = dep_parts(name)?;
                    if manifest_registry.to_string() == JSR && requirement.is_alias() {
                        return Err(format!(
                            "npm alias '{name}@{range}' cannot use a JSR dependency name"
                        )
                        .into());
                    }
                    if manifest_registry.as_str() == JSR && requirement.dist_tag().is_some() {
                        return Err(format!(
                            "unsupported JSR dist-tag dependency '{name}@{range}': npm dist-tags only are supported"
                        ).into());
                    }
                    let package = requirement.package_name(&local_package).clone();
                    let registry = registry_for_requirement(registry_config, name, &requirement)?;
                    registry_dependencies.push(WorkspaceRegistryDependency {
                        section,
                        member_key: locked[package_index].key(),
                        manifest_name: local_package.to_string(),
                        registry,
                        package,
                        requirement,
                    });
                    continue;
                };
                let requirement = workspace_requirement(range, version)?;
                if !requirement.matches(version) {
                    return Err(format!(
                        "workspace dependency '{}@{}' in member '{}' does not match local member version {}",
                        name,
                        range,
                        member.name(),
                        version
                    ).into());
                }
                let target = LockfilePackageKey::workspace(source.clone()).to_string();
                locked[package_index]
                    .add_dependency(name, &target)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    let links = plan_workspace_links(root, packages).map_err(|error| error.to_string())?;
    Ok(WorkspaceMaterialization {
        manifests,
        links,
        locked,
        members,
        registry_dependencies,
        peer_dependencies,
        declarations,
    })
}

fn validate_workspace_peer_providers(
    peers: &[WorkspacePeerDependency],
    direct_root_dependencies: &BTreeSet<(PackageSource, PackageName)>,
    resolution: &Resolution,
    records: &BTreeMap<PackageRecordKey, PackageRecord>,
) -> Result<(), String> {
    for peer in peers {
        let WorkspacePeerProvider::Registry { registry, package } = &peer.provider else {
            continue;
        };
        if !direct_root_dependencies.contains(&(registry.clone(), package.clone()))
            || !resolution.roots.iter().any(|root| {
                root.registry == *registry
                    && root.name == *package
                    && records
                        .get(&(
                            root.registry.to_string(),
                            root.name.to_string(),
                            root.version.to_string(),
                        ))
                        .is_some_and(|record| {
                            peer.requirement
                                .matches_tagged_version(&root.version, &record.dist_tags)
                        })
            })
        {
            return Err(format!(
                "workspace member peer dependency '{}' has no direct root provider satisfying {:?}",
                peer.manifest_name, peer.requirement
            ));
        }
    }
    Ok(())
}

pub(crate) fn dep_parts(name: &str) -> Result<(RegistryOrigin, PackageName), String> {
    let (origin, raw) = if let Some(v) = name.strip_prefix("jsr:") {
        (JSR, v)
    } else if let Some(v) = name.strip_prefix("npm:") {
        (NPM, v)
    } else {
        (NPM, name)
    };
    Ok((
        origin
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?,
        raw.parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?,
    ))
}

fn dependency_identity(
    registry_config: &crate::registry::RegistryConfig,
    name: &str,
) -> Result<(RegistryOrigin, PackageName), String> {
    if name.starts_with("jsr:") {
        dep_parts(name)
    } else {
        registry_config.identity_for_spec(name)
    }
}
fn fixture(path: &Path) -> Result<Fixture, String> {
    let f: Fixture = serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid registry fixture: {e}"))?;
    if f.packages.is_empty() {
        return Err("registry fixture contains no packages".into());
    }
    Ok(f)
}

fn remote_records(
    transport: &HttpsTransport,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allow_missing_integrity: bool,
) -> Result<Vec<PackageRecord>, OperationalError> {
    if registry.to_string() != JSR {
        let route = registry_config.route(name.as_str())?;
        if route.origin != *registry {
            return Err(OperationalError::new(
                ErrorKind::RegistryConfiguration,
                format!(
                    "registry identity mismatch for package {name}: selected {}, requested {registry}",
                    route.origin
                ),
            ));
        }
    }
    let artifacts: Vec<RegistryArtifact> = if registry.to_string() == JSR {
        JsrRegistry::new(transport, registry.clone()).fetch(&name.to_string())
    } else {
        NpmRegistry::new(transport, registry.clone())
            .fetch_with_options(&name.to_string(), allow_missing_integrity)
    }
    .map_err(|e| {
        OperationalError::from(e).context(format!("cannot fetch metadata for {registry}:{name}"))
    })?;
    Ok(artifacts
        .into_iter()
        .map(|a| PackageRecord {
            git_reference: None,
            copied_archive: None,
            dist_tags: a.dist_tags,
            registry: a.identity.registry,
            name: a.identity.name,
            version: a.identity.version,
            integrity: a.integrity,
            artifact: a.artifact_url,
            dependencies: a
                .dependencies
                .into_iter()
                .map(|(n, r)| (n.to_string(), r))
                .collect(),
            peer_dependencies: a
                .peer_dependencies
                .into_iter()
                .map(|(n, r)| (n.to_string(), r))
                .collect(),
            optional_peer_dependencies: a
                .optional_peer_dependencies
                .into_iter()
                .map(|name| name.to_string())
                .collect(),
            optional_dependencies: a
                .optional_dependencies
                .into_iter()
                .map(|(n, r)| (n.to_string(), r))
                .collect(),
            platform: a.platform,
            fixture: false,
        })
        .collect())
}

/// Converts only package versions whose dependency requirements Tapid can resolve.
///
/// Registry metadata contains obsolete versions and requirement syntaxes that are
/// irrelevant when a newer compatible version is selected. Keeping such versions
/// out of the candidate set prevents historical metadata from aborting resolution.
#[cfg(test)]
fn usable_versions(packages: Vec<PackageRecord>) -> Vec<PackageVersionMetadata> {
    let mut versions = Vec::new();
    for package in packages {
        if !current_platform_matches(&package.platform) {
            continue;
        }
        let dependencies = package
            .dependencies
            .iter()
            .map(|(name, requirement)| {
                let name = name
                    .parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let requirement =
                    parse_registry_requirement(name.as_str(), "dependency", requirement)?;
                Ok((name, requirement))
            })
            .collect::<Result<BTreeMap<PackageName, Requirement>, String>>();
        if let Ok(dependencies) = dependencies {
            versions.push(PackageVersionMetadata {
                dist_tags: package.dist_tags,
                name: package.name,
                version: package.version,
                dependencies,
                peer_dependencies: BTreeMap::new(),
                optional_peer_dependencies: BTreeSet::new(),
            });
        }
    }
    versions
}

fn artifact_progress_checkpoint(completed: usize, total: usize) -> bool {
    total > 0 && (completed == 1 || completed == total || completed.is_multiple_of(50))
}

pub(crate) fn validate_manifest_roots(
    project: &Path,
    manifest: &PackageManifest,
) -> Result<(), OperationalError> {
    let invalid = |error| OperationalError::new(ErrorKind::InvalidRequest, error);
    let registry_config = crate::registry::RegistryConfig::load(project).map_err(invalid)?;
    copied::validate_declarations(project, manifest).map_err(invalid)?;
    let workspace = workspace_materialization(project, &registry_config).map_err(|mut error| {
        // Preserve the request-validation category while retaining typed evidence.
        error.kind = ErrorKind::InvalidRequest;
        error
    })?;
    workspace_root_resolution(manifest, &workspace, &registry_config)
        .map(|_| ())
        .map_err(invalid)
}

pub(crate) fn resolved_workspace_registry_dependencies(
    manifest: &PackageManifest,
    workspace: &WorkspaceMaterialization,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<Vec<WorkspaceRegistryDependency>, String> {
    Ok(workspace_root_resolution(manifest, workspace, registry_config)?.registry_dependencies)
}

struct WorkspaceRootResolution {
    entry_points: Vec<(Dependency, tapid_resolver::EntryPoint)>,
    roots: Vec<Dependency>,
    direct_root_identities: BTreeSet<(PackageSource, PackageName)>,
    workspace_root_keys: Vec<String>,
    registry_dependencies: Vec<WorkspaceRegistryDependency>,
    overrides: BTreeMap<PackageName, Requirement>,
}

fn register_local_root_identity(
    identities: &mut BTreeMap<PackageName, String>,
    local_name: PackageName,
    identity: String,
) -> Result<(), String> {
    if identities
        .get(&local_name)
        .is_some_and(|previous| previous != &identity)
    {
        return Err(format!(
            "conflicting package identities for local dependency '{local_name}'"
        ));
    }
    identities.insert(local_name, identity);
    Ok(())
}

fn workspace_root_resolution(
    manifest: &PackageManifest,
    workspace: &WorkspaceMaterialization,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<WorkspaceRootResolution, String> {
    let overrides = manifest_overrides(manifest)?;
    let mut roots = Vec::new();
    let mut entry_points = Vec::new();
    let mut direct_root_identities = BTreeSet::new();
    let mut registry_dependencies = Vec::with_capacity(workspace.registry_dependencies.len());
    let mut local_root_identities = BTreeMap::new();
    for (name, (source, _)) in &workspace.members {
        let local_name = name
            .parse::<PackageName>()
            .map_err(|error| error.to_string())?;
        register_local_root_identity(
            &mut local_root_identities,
            local_name,
            format!("workspace:{}", source.path()),
        )?;
    }
    let mut workspace_root_keys = workspace
        .locked
        .iter()
        .map(LockedWorkspacePackage::key)
        .collect::<Vec<_>>();
    for (kind, map) in [
        ("dependencies", manifest.dependencies()),
        ("devDependencies", manifest.dev_dependencies()),
        ("optionalDependencies", manifest.optional_dependencies()),
    ] {
        for (name, range) in map {
            if let Some((source, version)) = workspace.members.get(name) {
                let requirement = workspace_requirement(range, version)?;
                if !requirement.matches(version) {
                    return Err(format!(
                        "workspace dependency '{name}@{range}' does not match local member version {version}"
                    ));
                }
                let (_, package) = dependency_identity(registry_config, name)?;
                if overrides
                    .get(&package)
                    .is_some_and(|override_requirement| override_requirement.raw != requirement.raw)
                {
                    return Err(format!(
                        "unsupported direct dependency override for '{package}': npm requires the override range to match the declared dependency range"
                    ));
                }
                let local_name = name
                    .parse::<PackageName>()
                    .map_err(|error| error.to_string())?;
                register_local_root_identity(
                    &mut local_root_identities,
                    local_name,
                    format!("workspace:{}", source.path()),
                )?;
                workspace_root_keys.push(LockfilePackageKey::workspace(source.clone()).to_string());
                continue;
            }
            if range.starts_with("workspace:") {
                return Err(format!(
                    "workspace dependency '{name}@{range}' has no matching local workspace member; refusing registry fallback"
                ));
            }
            if copied::declaration(range)
                .map_err(|reason| format!("dependency '{name}': {reason}"))?
                .is_some()
            {
                continue;
            }
            let requirement = range.parse::<Requirement>().map_err(|error| {
                format!("invalid {kind} dependency '{name}' range '{range}': {error}")
            })?;
            let (manifest_registry, local_package) = dep_parts(name)?;
            if manifest_registry.to_string() == JSR && requirement.is_alias() {
                return Err(format!(
                    "npm alias '{name}@{range}' cannot use a JSR dependency name"
                ));
            }
            let package = requirement.package_name(&local_package).clone();
            let registry = registry_for_requirement(registry_config, name, &requirement)?;
            register_local_root_identity(
                &mut local_root_identities,
                local_package.clone(),
                format!("registry:{registry}|{package}"),
            )?;
            if overrides
                .get(&package)
                .is_some_and(|override_requirement| override_requirement.raw != requirement.raw)
            {
                return Err(format!(
                    "unsupported direct dependency override for '{package}': npm requires the override range to match the declared dependency range"
                ));
            }
            direct_root_identities.insert((registry.clone(), package.clone()));
            let dependency = Dependency::new(registry, local_package, requirement);
            entry_points.push((
                dependency.clone(),
                tapid_resolver::EntryPoint {
                    workspace: None,
                    section: match kind {
                        "optionalDependencies" => {
                            tapid_resolver::EntryPointSection::OptionalDependencies
                        }
                        "devDependencies" => tapid_resolver::EntryPointSection::DevDependencies,
                        _ => tapid_resolver::EntryPointSection::Dependencies,
                    },
                },
            ));
            roots.push(dependency);
        }
    }
    for dependency in &workspace.registry_dependencies {
        let (_, local_name) = dep_parts(&dependency.manifest_name)?;
        register_local_root_identity(
            &mut local_root_identities,
            local_name,
            format!("registry:{}|{}", dependency.registry, dependency.package),
        )?;
        let requirement = overrides
            .get(&dependency.package)
            .cloned()
            .unwrap_or_else(|| dependency.requirement.clone());
        roots.push(Dependency::new(
            dependency.registry.clone(),
            dependency
                .manifest_name
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?,
            requirement.clone(),
        ));
        entry_points.push((
            roots.last().expect("workspace root").clone(),
            tapid_resolver::EntryPoint {
                workspace: Some(dependency.member_key.clone()),
                section: dependency.section,
            },
        ));
        let mut resolved_dependency = dependency.clone();
        resolved_dependency.requirement = requirement;
        registry_dependencies.push(resolved_dependency);
    }
    workspace_root_keys.sort();
    workspace_root_keys.dedup();
    Ok(WorkspaceRootResolution {
        entry_points,
        roots,
        direct_root_identities,
        workspace_root_keys,
        registry_dependencies,
        overrides,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_and_fetch_with_lock(
    project: &Path,
    manifest: &PackageManifest,
    store: &Store,
    fixture_path: Option<&Path>,
    allow_missing_integrity: bool,
    registry_config: &crate::registry::RegistryConfig,
    previous_lock: Option<&Lockfile>,
    update: Option<crate::application::lifecycle::UpdateRequest<'_>>,
    mut progress: impl FnMut(crate::application::install::Progress),
) -> ResolveAndFetchOutput {
    let workspace = workspace_materialization(project, registry_config)?;
    let graph::ResolvedGraph {
        workspace_manifests,
        omissions,
        candidate_exclusions,
        metadata_lookups: _,
        resolution,
        records,
        workspace_links,
        mut workspace_locked,
        workspace_registry_dependencies,
        root_registry_identities,
        workspace_root_keys,
        direct_local_names,
        updated_manifest,
    } = graph::resolve_package_graph(
        project,
        manifest,
        workspace,
        fixture_path,
        allow_missing_integrity,
        registry_config,
        previous_lock,
        update,
        &mut progress,
    )?;
    let configured_origins = registry_config.configured_origins();
    if let Some(request) = update.filter(|request| request.save.is_some()) {
        let saved = updated_manifest.as_ref().expect("resolved update manifest");
        fs::write(request.target, saved.to_json()).map_err(|error| {
            OperationalError::from_source(ErrorKind::Transaction, error)
                .context("cannot save updated requirements")
        })?;
        workspace_locked = workspace_materialization(project, registry_config)?.locked;
    }
    store.recover_transactions().map_err(|error| {
        OperationalError::from(error).context("cannot prepare shared store for recovery")
    })?;
    fs::create_dir_all(store.root()).map_err(|e| {
        OperationalError::from_source(ErrorKind::Store, e).context("cannot create store")
    })?;
    let mut store_transaction = store.transaction();
    let mut lock = Lockfile::new(
        &root_digest(project).map_err(|error| OperationalError::new(ErrorKind::Manifest, error))?,
    )
    .map_err(OperationalError::from)?;
    let empty_peer = tapid_core::PeerContext::default();
    let mut platform_contexts = BTreeMap::new();
    let mut packages = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut instances = Vec::new();
    let mut artifact_fetcher = artifact::ArtifactFetcher {
        store,
        config: registry_config,
        transports: BTreeMap::new(),
        allowed_origins: configured_origins,
    };
    let pinned_packages = previous_lock
        .map(locked::reusable_locked_packages)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|(key, package)| {
            Ok((
                (
                    key.source
                        .package_source()
                        .ok_or("expected registry identity")?,
                    key.name,
                    key.version,
                ),
                package,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, OperationalError>>()?;
    let artifact_total = resolution.selected.len();
    for (index, id) in resolution.selected.iter().enumerate() {
        let peer_context = resolution
            .peer_contexts
            .get(id)
            .cloned()
            .unwrap_or_default();
        let key3 = (
            id.registry.to_string(),
            id.name.to_string(),
            id.version.to_string(),
        );
        let record = records.get(&key3).cloned().ok_or_else(|| {
            OperationalError::new(
                ErrorKind::RegistryMetadata,
                format!("missing selected artifact metadata: {id}"),
            )
        })?;
        let pinned = pinned_packages
            .get(&(id.registry.clone(), id.name.clone(), id.version.clone()))
            .copied();
        let mut record = record;
        if let Some(package) = pinned {
            if id.registry.registry().is_some()
                && package.registry_integrity_declared() != Some(true)
            {
                return Err(OperationalError::new(
                    ErrorKind::Integrity,
                    "locked artifact lacks registry-declared integrity provenance",
                ));
            }
            record.integrity = Some(
                package
                    .artifact_integrity()
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?,
            );
            if !record.fixture
                && let Some(url) = package.artifact_url()
            {
                record.artifact = url.to_owned();
            }
        }
        let platform_context = record
            .platform
            .selected_context(std::env::consts::OS, std::env::consts::ARCH, current_libc())
            .map_err(|error| OperationalError::new(ErrorKind::Resolution, error))?;
        platform_contexts.insert(id.clone(), platform_context.clone());
        let (actual, tree_digest, tree) = artifact_fetcher
            .prepare(id, &record, pinned, &mut store_transaction)
            .map_err(|error| error.for_package(id.into()))?;
        let key = LockfilePackageKey::from_source(
            id.registry.clone(),
            id.name.clone(),
            id.version.clone(),
            &peer_context,
            &platform_context,
        )
        .to_string();
        let integrity_provenance = if id.registry.registry().is_some() && record.integrity.is_some()
        {
            RegistryIntegrityProvenance::RegistryDeclared
        } else {
            RegistryIntegrityProvenance::LocallyComputed
        };
        let mut locked = LockedPackage::new_with_context_and_provenance(
            &id.registry.to_string(),
            &id.name.to_string(),
            &id.version.to_string(),
            &actual.to_string(),
            &tree_digest.to_string(),
            (&peer_context, &platform_context),
            integrity_provenance,
        )
        .map_err(OperationalError::from)?;
        if let Some(url) = pinned.and_then(LockedPackage::artifact_url).or_else(|| {
            (!record.fixture && !record.artifact.is_empty()).then_some(record.artifact.as_str())
        }) {
            locked
                .set_artifact_url(url)
                .map_err(OperationalError::from)?;
        }
        packages.insert(key.clone(), (locked, record, id.clone()));
        trees.insert(key, tree.clone());
        instances.push(PackageInstance {
            id: tapid_core::PackageInstanceId::from_source(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
            ),
            peer_context,
            platform_context,
            tree: VerifiedTreeReference::new(&tree_digest.to_string(), &tree).map_err(|error| {
                OperationalError::from_source(ErrorKind::Materialization, error)
            })?,
        });
        let completed = index + 1;
        if artifact_progress_checkpoint(completed, artifact_total) {
            progress(crate::application::install::Progress::Artifact(
                completed,
                artifact_total,
            ));
        }
    }
    let mut dependencies_by_parent = BTreeMap::new();
    for edge in &resolution.dependencies {
        dependencies_by_parent
            .entry(edge.parent.clone())
            .or_insert_with(Vec::new)
            .push(edge);
    }
    let locked_packages: Result<Vec<_>, OperationalError> = packages
        .values()
        .map(|(locked, _, id)| {
            let mut locked = locked.clone();
            for edge in dependencies_by_parent.get(id).into_iter().flatten() {
                let target = &edge.child;
                let target_platform = platform_contexts.get(target).ok_or_else(|| {
                    OperationalError::new(
                        ErrorKind::Resolution,
                        format!("missing platform context for {target}"),
                    )
                })?;
                let target_key = LockfilePackageKey::from_source(
                    target.registry.clone(),
                    target.name.clone(),
                    target.version.clone(),
                    resolution.peer_contexts.get(target).unwrap_or(&empty_peer),
                    target_platform,
                )
                .to_string();
                locked
                    .add_alias_dependency(&edge.dependency.to_string(), &target_key)
                    .map_err(OperationalError::from)?;
            }
            Ok(locked)
        })
        .collect();
    for dependency in &workspace_registry_dependencies {
        let id = graph::workspace_registry_target(dependency, &resolution, &records)?;
        let target_platform = platform_contexts
            .get(id)
            .ok_or_else(|| format!("missing platform context for {id}"))?;
        let target_key = LockfilePackageKey::from_source(
            id.registry.clone(),
            id.name.clone(),
            id.version.clone(),
            resolution.peer_contexts.get(id).unwrap_or(&empty_peer),
            target_platform,
        )
        .to_string();
        let member = workspace_locked
            .iter_mut()
            .find(|member| member.key() == dependency.member_key)
            .ok_or("workspace member dependency has no lockfile identity")?;
        let dependency_name = dependency.manifest_name.clone();
        if member
            .dependencies()
            .get(&dependency_name)
            .is_some_and(|existing| existing != &target_key)
        {
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "workspace member '{}' declares ambiguous registry identities for dependency '{}'; refusing to overwrite a lockfile edge",
                    dependency.member_key, dependency_name
                ),
            ));
        }
        if dependency.requirement.is_alias() {
            member
                .add_alias_dependency(&dependency_name, &target_key)
                .map_err(OperationalError::from)?;
        } else {
            member
                .add_dependency(&dependency_name, &target_key)
                .map_err(OperationalError::from)?;
        }
    }
    lock.insert_graph(locked_packages?, workspace_locked)
        .map_err(OperationalError::from)?;
    let direct_bindings = resolution
        .root_bindings
        .iter()
        .filter(|((registry, name), id)| {
            direct_local_names.contains(name)
                && root_registry_identities.contains(&(registry.clone(), id.name.clone()))
        })
        .collect::<Vec<_>>();
    let mut root_keys = direct_bindings
        .iter()
        .map(|(_, id)| *id)
        .map(|id| {
            let platform = platform_contexts
                .get(id)
                .expect("selected root platform context");
            LockfilePackageKey::from_source(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
                resolution.peer_contexts.get(id).unwrap_or(&empty_peer),
                platform,
            )
            .to_string()
        })
        .collect::<Vec<_>>();
    root_keys.extend(workspace_root_keys);
    root_keys.sort();
    root_keys.dedup();
    lock.set_roots(root_keys).map_err(OperationalError::from)?;
    lock.set_root_bindings(
        direct_bindings
            .iter()
            .map(|((_, name), id)| {
                let platform = platform_contexts
                    .get(id)
                    .expect("selected root platform context");
                (
                    name.to_string(),
                    LockfilePackageKey::from_source(
                        id.registry.clone(),
                        id.name.clone(),
                        id.version.clone(),
                        resolution.peer_contexts.get(id).unwrap_or(&empty_peer),
                        platform,
                    )
                    .to_string(),
                )
            })
            .collect(),
    )
    .map_err(OperationalError::from)?;
    selection::record_registry_selection(
        &mut lock,
        manifest,
        &workspace_manifests,
        &resolution,
        &records,
        registry_config,
        omissions,
    )?;
    let instance_keys = instances
        .iter()
        .map(|instance| {
            (
                (
                    instance.id.registry.clone(),
                    instance.id.name.clone(),
                    instance.id.version.clone(),
                    instance.peer_context.clone(),
                ),
                InstanceKey::from(instance),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut edge_list = Vec::new();
    let mut root_deps = Vec::new();
    for edge in &resolution.dependencies {
        let parent = instance_keys
            .get(&(
                edge.parent.registry.clone(),
                edge.parent.name.clone(),
                edge.parent.version.clone(),
                resolution
                    .peer_contexts
                    .get(&edge.parent)
                    .cloned()
                    .unwrap_or_default(),
            ))
            .ok_or_else(|| {
                OperationalError::new(
                    ErrorKind::Resolution,
                    format!("missing parent instance for {}", edge.parent),
                )
            })?;
        let child = instance_keys
            .get(&(
                edge.child.registry.clone(),
                edge.child.name.clone(),
                edge.child.version.clone(),
                resolution
                    .peer_contexts
                    .get(&edge.child)
                    .cloned()
                    .unwrap_or_default(),
            ))
            .ok_or_else(|| {
                OperationalError::new(
                    ErrorKind::Resolution,
                    format!("missing child instance for {}", edge.child),
                )
            })?;
        edge_list.push(NamedDependencyEdge {
            parent: parent.clone(),
            dependency: NamedDependency {
                name: edge.dependency.clone(),
                child: child.clone(),
            },
        });
    }
    for ((_, name), id) in direct_bindings {
        let instance = instance_keys
            .get(&(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
                resolution
                    .peer_contexts
                    .get(id)
                    .cloned()
                    .unwrap_or_default(),
            ))
            .ok_or_else(|| format!("missing root instance for {id}"))?;
        root_deps.push(NamedDependency {
            name: name.clone(),
            child: instance.clone(),
        });
    }
    for dependency in &workspace_registry_dependencies {
        let id = graph::workspace_registry_target(dependency, &resolution, &records)?;
        let instance = instance_keys
            .get(&(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
                resolution
                    .peer_contexts
                    .get(id)
                    .cloned()
                    .unwrap_or_default(),
            ))
            .ok_or_else(|| format!("missing workspace dependency instance for {id}"))?;
        root_deps.push(NamedDependency {
            name: dependency
                .manifest_name
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?,
            child: instance.clone(),
        });
    }
    Ok(PreparedInstall {
        candidate_exclusions,
        lock,
        layout: PreparedLayout {
            input: NamedLayoutInput {
                instances,
                root_dependencies: root_deps,
                dependency_edges: edge_list,
            },
            trees,
            workspace: workspace_links.into(),
        },
        transaction: store_transaction,
    })
}

/// Membership and member manifest bytes both participate in replay identity.
pub(crate) fn workspace_matches_lock(
    workspace: &WorkspaceMaterialization,
    lock: &Lockfile,
) -> bool {
    let current = workspace
        .locked
        .iter()
        .map(|p| (p.key(), p.manifest_digest()))
        .collect::<BTreeMap<_, _>>();
    let locked = lock
        .workspace_packages()
        .iter()
        .map(|(k, p)| (k.clone(), p.manifest_digest()))
        .collect::<BTreeMap<_, _>>();
    current == locked
}

#[cfg(test)]
#[path = "online/tests.rs"]
mod tests;
