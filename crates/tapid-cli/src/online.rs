use crate::application::outcome::{ErrorKind, OperationalError};
mod locked;
mod resolution;
pub(crate) use locked::prepare_locked_install;
use resolution::resolve_with_fetch_routed_and_overrides;
#[cfg(test)]
use resolution::{
    RESOLVER_METADATA_BUILD_COUNT, RESOLVER_METADATA_PARENT_VISITS,
    RESOLVER_METADATA_VERSION_VISITS, metadata_progress_checkpoint, normalize_record,
    parse_registry_requirement, resolve_with_fetch, resolve_with_fetch_routed,
    resolve_with_overrides,
};
pub(crate) use resolution::{manifest_overrides, manifest_roots};

mod routing;
use routing::artifact_transport_for_package;
pub(crate) use routing::metadata_transport_for_package;
#[cfg(test)]
use routing::transport_for_route;

mod platform;
#[cfg(test)]
use platform::platform_matches_for;
use platform::{current_libc, current_platform_matches, selected_platform_context_for};

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
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName, PackageVersion, RegistryOrigin};
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
    ResolutionOptions, ResolveError, resolve_graph_with_routing,
};
use tapid_store::{Store, StoreTransaction};

const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";
static NEXT_TEMP_TREE_ID: AtomicU64 = AtomicU64::new(0);

type ResolveAndFetchOutput = Result<
    (
        Lockfile,
        NamedLayoutInput,
        BTreeMap<String, PathBuf>,
        StoreTransaction,
        WorkspaceLinkPlan,
    ),
    OperationalError,
>;

struct TemporaryTree(PathBuf);

impl Drop for TemporaryTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Deserialize, Clone)]
struct FixturePackage {
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
}
#[derive(Debug, Deserialize)]
struct Fixture {
    packages: Vec<FixturePackage>,
}

#[derive(Clone)]
struct PackageRecord {
    registry: RegistryOrigin,
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
fn integrity(data: &[u8]) -> PackageIntegrity {
    let mut h = Sha512::new();
    h.update(data);
    format!("sha512-{}", STANDARD.encode(h.finalize()))
        .parse()
        .expect("sha512 integrity")
}

fn integrity_matches(expected: &PackageIntegrity, actual: &PackageIntegrity) -> bool {
    expected == actual
}
fn root_digest(project: &Path) -> Result<String, String> {
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
    pub manifest_name: String,
    pub registry: RegistryOrigin,
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
        registry: RegistryOrigin,
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
    pub links: WorkspaceLinkPlan,
    pub locked: Vec<LockedWorkspacePackage>,
    pub members: BTreeMap<String, (LocalWorkspaceSource, PackageVersion)>,
    pub registry_dependencies: Vec<WorkspaceRegistryDependency>,
    pub peer_dependencies: Vec<WorkspacePeerDependency>,
}

pub(crate) fn workspace_materialization(
    project: &Path,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<WorkspaceMaterialization, String> {
    let workspace = Workspace::discover(project)?;
    let root = workspace
        .root_path()
        .parent()
        .ok_or("workspace root manifest has no parent")?;
    if root != project {
        return Err("resolved workspace root does not match install project root".to_owned());
    }
    let mut packages = Vec::new();
    let mut locked = Vec::new();
    let mut members = BTreeMap::new();
    let mut locked_by_name = BTreeMap::new();
    let mut registry_dependencies = Vec::new();
    let mut peer_dependencies = Vec::new();
    for member in workspace.members() {
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
            ));
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
        let version = member.manifest().version().clone();
        let source =
            LocalWorkspaceSource::new(&relative_posix, member.name(), &version.to_string())
                .map_err(|error| error.to_string())?;
        let manifest_bytes = fs::read(member.path()).map_err(|error| {
            format!(
                "cannot read workspace member manifest {}: {error}",
                member.path().display()
            )
        })?;
        locked_by_name.insert(member.name().to_owned(), locked.len());
        locked.push(
            LockedWorkspacePackage::new(source.clone(), &digest(&manifest_bytes).to_string())
                .map_err(|error| error.to_string())?,
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
        let package_index = *locked_by_name
            .get(member.name())
            .ok_or("workspace member has no lockfile identity")?;
        for (name, range) in member.manifest().peer_dependencies() {
            let (requirement, provider) = if let Some((source, version)) = members.get(name) {
                let requirement = workspace_requirement(range, version)?;
                if !requirement.matches(version) {
                    return Err(format!(
                        "workspace member peer dependency '{}@{}' in '{}' does not match local provider version {}",
                        name,
                        range,
                        member.name(),
                        version
                    ));
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
                    ));
                }
                let (registry, package) = dependency_identity(registry_config, name)?;
                let requirement = range.parse::<Requirement>().map_err(|error| {
                    format!(
                        "invalid workspace member peer dependency '{name}' range '{range}': {error}"
                    )
                })?;
                (
                    requirement,
                    WorkspacePeerProvider::Registry { registry, package },
                )
            };
            peer_dependencies.push(WorkspacePeerDependency {
                member_key: locked[package_index].key(),
                manifest_name: name.clone(),
                requirement,
                provider,
            });
        }
        for dependency_map in [
            member.manifest().dependencies(),
            member.manifest().dev_dependencies(),
            member.manifest().optional_dependencies(),
        ] {
            for (name, range) in dependency_map {
                let Some((source, version)) = members.get(name) else {
                    if range.starts_with("workspace:") {
                        return Err(format!(
                            "workspace dependency '{}' in member '{}' has no local workspace target; refusing registry fallback",
                            name,
                            member.name()
                        ));
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
                        ));
                    }
                    let package = requirement.package_name(&local_package).clone();
                    let registry = if requirement.is_alias() {
                        registry_config.origin_for_name(&package)?
                    } else {
                        dependency_identity(registry_config, name)?.0
                    };
                    registry_dependencies.push(WorkspaceRegistryDependency {
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
                    ));
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
        links,
        locked,
        members,
        registry_dependencies,
        peer_dependencies,
    })
}

fn validate_workspace_peer_providers(
    peers: &[WorkspacePeerDependency],
    direct_root_dependencies: &BTreeSet<(RegistryOrigin, PackageName)>,
    resolution: &Resolution,
) -> Result<(), String> {
    for peer in peers {
        let WorkspacePeerProvider::Registry { registry, package } = &peer.provider else {
            continue;
        };
        if !direct_root_dependencies.contains(&(registry.clone(), package.clone()))
            || !resolution.roots.iter().any(|root| {
                root.registry == *registry
                    && root.name == *package
                    && peer.requirement.matches(&root.version)
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
) -> Result<(), String> {
    let registry_config = crate::registry::RegistryConfig::load(project)?;
    let workspace = workspace_materialization(project, &registry_config)?;
    workspace_root_resolution(manifest, &workspace, &registry_config).map(|_| ())
}

pub(crate) fn resolved_workspace_registry_dependencies(
    manifest: &PackageManifest,
    workspace: &WorkspaceMaterialization,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<Vec<WorkspaceRegistryDependency>, String> {
    Ok(workspace_root_resolution(manifest, workspace, registry_config)?.registry_dependencies)
}

struct WorkspaceRootResolution {
    roots: Vec<Dependency>,
    direct_root_identities: BTreeSet<(RegistryOrigin, PackageName)>,
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
            let registry = if requirement.is_alias() {
                registry_config.origin_for_name(&package)?
            } else {
                dependency_identity(registry_config, name)?.0
            };
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
            roots.push(Dependency::new(registry, local_package, requirement));
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
        let mut resolved_dependency = dependency.clone();
        resolved_dependency.requirement = requirement;
        registry_dependencies.push(resolved_dependency);
    }
    workspace_root_keys.sort();
    workspace_root_keys.dedup();
    Ok(WorkspaceRootResolution {
        roots,
        direct_root_identities,
        workspace_root_keys,
        registry_dependencies,
        overrides,
    })
}

pub fn resolve_and_fetch(
    project: &Path,
    manifest: &PackageManifest,
    store: &Store,
    fixture_path: Option<&Path>,
    allow_missing_integrity: bool,
    registry_config: &crate::registry::RegistryConfig,
) -> ResolveAndFetchOutput {
    let workspace = workspace_materialization(project, registry_config)?;
    let WorkspaceRootResolution {
        roots,
        direct_root_identities: root_registry_identities,
        workspace_root_keys,
        registry_dependencies: workspace_registry_dependencies,
        overrides,
    } = workspace_root_resolution(manifest, &workspace, registry_config)?;
    let direct_local_names = manifest_roots(manifest)?
        .into_iter()
        .map(|dependency| dependency.name)
        .collect::<BTreeSet<_>>();
    let WorkspaceMaterialization {
        links: workspace_links,
        locked: mut workspace_locked,
        members: _,
        registry_dependencies: _,
        peer_dependencies: workspace_peer_dependencies,
    } = workspace;
    let fixture = fixture_path.map(fixture).transpose()?;
    let mut fixture_records = BTreeMap::<(String, String, String), PackageRecord>::new();
    if let Some(f) = &fixture {
        for p in &f.packages {
            let registry: RegistryOrigin =
                p.registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                            .context(format!("invalid fixture registry {}", p.registry))
                    })?;
            let name: PackageName = p.name.parse().map_err(|error: tapid_core::DomainError| {
                OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                    .context(format!("invalid fixture package {}", p.name))
            })?;
            let version: PackageVersion =
                p.version
                    .parse()
                    .map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                            .context(format!("invalid fixture version {}", p.version))
                    })?;
            let integrity = p
                .integrity
                .clone()
                .map(|v| {
                    v.parse().map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                            .context(format!("invalid fixture integrity {v}"))
                    })
                })
                .transpose()?;
            if registry.to_string() == NPM && integrity.is_none() && !allow_missing_integrity {
                return Err(OperationalError::new(
                    ErrorKind::RegistryMetadata,
                    format!(
                        "fixture npm metadata for {}@{} is missing dist.integrity; pass --allow-unverified-registry-artifacts for an explicit compatibility exception",
                        name, version
                    ),
                ));
            }
            fixture_records.insert(
                (p.registry.clone(), p.name.clone(), p.version.clone()),
                PackageRecord {
                    registry,
                    name,
                    version,
                    integrity,
                    artifact: p.artifact.clone(),
                    dependencies: p.dependencies.clone(),
                    peer_dependencies: p.peer_dependencies.clone(),
                    optional_peer_dependencies: p.optional_peer_dependencies.clone(),
                    optional_dependencies: BTreeMap::new(),
                    platform: PackagePlatform::unrestricted(),
                    fixture: true,
                },
            );
        }
    }
    let configured_origins = registry_config.configured_origins();
    let mut metadata_transports = BTreeMap::<(String, String), HttpsTransport>::new();
    let (resolution, mut records) = resolve_with_fetch_routed_and_overrides(
        &roots,
        |parent, dependency| registry_config.registry_for_dependency(parent, dependency),
        &overrides,
        |registry, name| {
            if fixture.is_some() {
                Ok(fixture_records
                    .values()
                    .filter(|package| &package.registry == registry && &package.name == name)
                    .cloned()
                    .collect())
            } else {
                let transport = metadata_transport_for_package(
                    &mut metadata_transports,
                    registry_config,
                    registry,
                    name,
                    &configured_origins,
                )?;
                remote_records(
                    transport,
                    registry_config,
                    registry,
                    name,
                    allow_missing_integrity,
                )
            }
        },
    )?;
    validate_workspace_peer_providers(
        &workspace_peer_dependencies,
        &root_registry_identities,
        &resolution,
    )?;
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
    let mut artifact_transports = BTreeMap::<(String, String), HttpsTransport>::new();
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
        let record = if let Some(p) = records.get(&key3) {
            p.clone()
        } else {
            let transport = metadata_transport_for_package(
                &mut metadata_transports,
                registry_config,
                &id.registry,
                &id.name,
                &configured_origins,
            )?;
            let fetched = remote_records(
                transport,
                registry_config,
                &id.registry,
                &id.name,
                allow_missing_integrity,
            )?;
            let p = fetched
                .into_iter()
                .find(|p| p.version == id.version)
                .ok_or_else(|| {
                    OperationalError::new(
                        ErrorKind::RegistryMetadata,
                        format!("missing artifact metadata: {id}"),
                    )
                })?;
            records.insert(key3.clone(), p.clone());
            p
        };
        let platform_context = selected_platform_context_for(
            std::env::consts::OS,
            std::env::consts::ARCH,
            current_libc(),
            &record.platform,
        )
        .map_err(|error| OperationalError::new(ErrorKind::Resolution, error))?;
        platform_contexts.insert(id.clone(), platform_context.clone());
        let bytes = if record.fixture {
            if let Some(encoded) = record.artifact.strip_prefix("base64:") {
                STANDARD.decode(encoded).map_err(|e| {
                    OperationalError::from_source(ErrorKind::RegistryMetadata, e)
                        .context("invalid artifact encoding")
                })?
            } else {
                fs::read(&record.artifact).map_err(|e| {
                    OperationalError::from_source(ErrorKind::RegistryTransport, e)
                        .context(format!("cannot read artifact {}", record.artifact))
                })?
            }
        } else {
            let transport = artifact_transport_for_package(
                &mut artifact_transports,
                registry_config,
                &id.registry,
                &id.name,
                &configured_origins,
            )?;
            let response = if record.registry.to_string() == JSR {
                JsrRegistry::new(transport, record.registry.clone())
                    .download_artifact(&record.artifact)
            } else {
                NpmRegistry::new(transport, record.registry.clone())
                    .download_artifact(&record.artifact)
            }
            .map_err(|e| {
                OperationalError::from_source(ErrorKind::RegistryTransport, e)
                    .context(format!("cannot download {id}"))
            })?;
            if response.status != 200 {
                return Err(OperationalError::new(
                    ErrorKind::RegistryTransport,
                    format!("cannot download {}: HTTP {}", id, response.status),
                ));
            }
            response.body
        };
        let actual = integrity(&bytes);
        if record
            .integrity
            .as_ref()
            .is_some_and(|expected| !integrity_matches(expected, &actual))
        {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                format!("integrity mismatch for {id}"),
            ));
        }
        let temp_id = NEXT_TEMP_TREE_ID.fetch_add(1, Ordering::Relaxed);
        let temp = store.root().join(format!(
            ".online-tree-{}-{temp_id}-{}",
            std::process::id(),
            id.version
        ));
        let _temporary_tree = TemporaryTree(temp.clone());
        extract_to(
            &bytes,
            ArchiveFormat::TarGz,
            &temp,
            ArchiveLimits::default(),
        )
        .map_err(|e| {
            OperationalError::from_source(ErrorKind::Archive, e)
                .context(format!("cannot extract {id}"))
        })?;
        let tree_digest: ArtifactDigest = canonical_tree_digest(&temp)
            .map_err(|e| OperationalError::from_source(ErrorKind::Archive, e))?
            .parse()
            .map_err(|e: tapid_core::DomainError| {
                OperationalError::from_source(ErrorKind::Archive, e)
            })?;
        let tree = store_transaction
            .stage_verified_tree(&tree_digest, &temp)
            .map_err(OperationalError::from)?;
        let key = LockfilePackageKey::new(
            id.registry.clone(),
            id.name.clone(),
            id.version.clone(),
            &peer_context,
            &platform_context,
        )
        .to_string();
        let integrity_provenance = if record.integrity.is_some() {
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
        if !record.fixture {
            locked
                .set_artifact_url(&record.artifact)
                .map_err(OperationalError::from)?;
        }
        packages.insert(key.clone(), (locked, record, id.clone()));
        trees.insert(key, tree.clone());
        instances.push(PackageInstance {
            id: tapid_core::PackageInstanceId::new(
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
            eprintln!("Artifact verification progress: {completed}/{artifact_total}");
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
                let target_key = LockfilePackageKey::new(
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
        let id = resolution
            .roots
            .iter()
            .find(|id| {
                id.registry == dependency.registry
                    && id.name == dependency.package
                    && dependency.requirement.matches(&id.version)
            })
            .ok_or_else(|| {
                format!(
                    "no selected registry package satisfies workspace member dependency '{}' ({:?})",
                    dependency.manifest_name, dependency.requirement
                )
            })?;
        let target_platform = platform_contexts
            .get(id)
            .ok_or_else(|| format!("missing platform context for {id}"))?;
        let target_key = LockfilePackageKey::new(
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
    let mut root_keys = resolution
        .roots
        .iter()
        .filter(|id| root_registry_identities.contains(&(id.registry.clone(), id.name.clone())))
        .map(|id| {
            let platform = platform_contexts
                .get(id)
                .expect("selected root platform context");
            LockfilePackageKey::new(
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
        resolution
            .root_bindings
            .iter()
            .filter(|((registry, name), id)| {
                direct_local_names.contains(name)
                    && root_registry_identities.contains(&(registry.clone(), id.name.clone()))
            })
            .map(|((_, name), id)| {
                let platform = platform_contexts
                    .get(id)
                    .expect("selected root platform context");
                (
                    name.to_string(),
                    LockfilePackageKey::new(
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
    for ((registry, name), id) in &resolution.root_bindings {
        if !direct_local_names.contains(name)
            || !root_registry_identities.contains(&(registry.clone(), id.name.clone()))
        {
            continue;
        }
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
        let id = resolution
            .roots
            .iter()
            .find(|id| {
                id.registry == dependency.registry
                    && id.name == dependency.package
                    && dependency.requirement.matches(&id.version)
            })
            .ok_or_else(|| {
                format!(
                    "no selected registry package satisfies workspace member dependency '{}' ({:?})",
                    dependency.manifest_name, dependency.requirement
                )
            })?;
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
    Ok((
        lock,
        NamedLayoutInput {
            instances,
            root_dependencies: root_deps,
            dependency_edges: edge_list,
        },
        trees,
        store_transaction,
        workspace_links,
    ))
}

#[cfg(test)]
#[path = "online/tests.rs"]
mod tests;
