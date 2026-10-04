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
    DependencyEdge, InstanceKey, LayoutInput, PackageInstance, VerifiedTreeReference,
    WorkspaceLinkPlan, WorkspacePackage, plan_workspace_links,
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
        LayoutInput,
        BTreeMap<String, PathBuf>,
        StoreTransaction,
        WorkspaceLinkPlan,
    ),
    String,
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
                    let (registry, package) = dependency_identity(registry_config, name)?;
                    let requirement = range.parse::<Requirement>().map_err(|error| {
                        format!(
                            "invalid workspace member dependency '{name}' range '{range}': {error}"
                        )
                    })?;
                    registry_dependencies.push(WorkspaceRegistryDependency {
                        member_key: locked[package_index].key(),
                        manifest_name: name.clone(),
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

fn package_registry_route(
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
) -> Result<crate::registry::RegistryRoute, String> {
    if registry.to_string() == JSR {
        return Ok(crate::registry::RegistryRoute {
            origin: registry.clone(),
            token: None,
            policy: "jsr".to_owned(),
        });
    }
    let route = registry_config.route(name.as_str())?;
    if route.origin != *registry {
        return Err(format!(
            "registry identity mismatch for package {name}: selected {}, requested {registry}",
            route.origin
        ));
    }
    Ok(route)
}

fn transport_for_route<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    route: crate::registry::RegistryRoute,
    allowed_origins: &[String],
    artifact: bool,
) -> Result<&'a HttpsTransport, String> {
    let origin = route.origin.to_string();
    let key = (origin.clone(), route.policy);
    if !cache.contains_key(&key) {
        let credentials = route
            .token
            .map(|token| vec![(origin.clone(), token)])
            .unwrap_or_default();
        let transport = if artifact {
            HttpsTransport::authenticated_artifact(allowed_origins.to_vec(), credentials)
        } else {
            HttpsTransport::authenticated_metadata(allowed_origins.to_vec(), credentials)
        }
        .map_err(|error| format!("cannot create registry transport: {error}"))?;
        cache.insert(key.clone(), transport);
    }
    Ok(cache
        .get(&key)
        .expect("transport cache entry was just found or inserted"))
}

pub(crate) fn metadata_transport_for_package<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allowed_origins: &[String],
) -> Result<&'a HttpsTransport, String> {
    let route = package_registry_route(registry_config, registry, name)?;
    transport_for_route(cache, route, allowed_origins, false)
}

fn artifact_transport_for_package<'a>(
    cache: &'a mut BTreeMap<(String, String), HttpsTransport>,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allowed_origins: &[String],
) -> Result<&'a HttpsTransport, String> {
    let route = package_registry_route(registry_config, registry, name)?;
    transport_for_route(cache, route, allowed_origins, true)
}

fn remote_records(
    transport: &HttpsTransport,
    registry_config: &crate::registry::RegistryConfig,
    registry: &RegistryOrigin,
    name: &PackageName,
    allow_missing_integrity: bool,
) -> Result<Vec<PackageRecord>, String> {
    if registry.to_string() != JSR {
        let route = registry_config.route(name.as_str())?;
        if route.origin != *registry {
            return Err(format!(
                "registry identity mismatch for package {name}: selected {}, requested {registry}",
                route.origin
            ));
        }
    }
    let artifacts: Vec<RegistryArtifact> = if registry.to_string() == JSR {
        JsrRegistry::new(transport, registry.clone()).fetch(&name.to_string())
    } else {
        NpmRegistry::new(transport, registry.clone())
            .fetch_with_options(&name.to_string(), allow_missing_integrity)
    }
    .map_err(|e| format!("cannot fetch metadata for {registry}:{name}: {e}"))?;
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

fn npm_os(value: &str) -> &str {
    match value {
        "macos" => "darwin",
        "windows" => "win32",
        value => value,
    }
}

fn npm_cpu(value: &str) -> &str {
    match value {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        value => value,
    }
}

fn selected_platform_context_for(
    os: &str,
    cpu: &str,
    libc: Option<&str>,
    constraints: &PackagePlatform,
) -> Result<tapid_core::PlatformContext, String> {
    let libc_context = if constraints.libc.is_empty()
        || npm_os(os) != "linux"
        || (constraints.libc.iter().all(|value| value.starts_with('!')) && libc.is_none())
    {
        None
    } else {
        Some(libc.ok_or("selected package requires a libc platform context")?)
    };
    tapid_core::PlatformContext::new(
        (!constraints.os.is_empty()).then_some(npm_os(os)),
        (!constraints.cpu.is_empty()).then_some(npm_cpu(cpu)),
        libc_context,
    )
    .map_err(|error| error.to_string())
}

fn current_libc() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        Some("musl")
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        Some("glibc")
    }
    #[cfg(any(
        not(target_os = "linux"),
        all(target_os = "linux", not(any(target_env = "musl", target_env = "gnu")))
    ))]
    {
        None
    }
}

fn platform_matches_for(
    os: &str,
    cpu: &str,
    libc: Option<&str>,
    platform: &PackagePlatform,
) -> bool {
    fn value_matches(values: &[String], current: Option<&str>) -> bool {
        if values.is_empty() {
            return true;
        }
        let Some(current) = current else {
            return false;
        };
        let mut has_positive = false;
        let mut positive_match = false;
        for value in values {
            if let Some(excluded) = value.strip_prefix('!') {
                if excluded == current {
                    return false;
                }
            } else {
                has_positive = true;
                positive_match |= value == current;
            }
        }
        !has_positive || positive_match
    }

    let os = npm_os(os);
    let cpu = npm_cpu(cpu);
    let libc_matches = if os != "linux"
        || (libc.is_none() && platform.libc.iter().all(|value| value.starts_with('!')))
    {
        true
    } else {
        value_matches(&platform.libc, libc)
    };

    value_matches(&platform.os, Some(os)) && value_matches(&platform.cpu, Some(cpu)) && libc_matches
}

fn current_platform_matches(platform: &PackagePlatform) -> bool {
    platform_matches_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        current_libc(),
        platform,
    )
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

#[derive(Clone)]
struct NormalizedRecord {
    metadata: PackageVersionMetadata,
    optional_dependencies: BTreeMap<PackageName, Requirement>,
}

type NormalizedRecords = BTreeMap<PackageRecordKey, Result<NormalizedRecord, String>>;

fn parse_registry_requirement(
    name: &str,
    kind: &str,
    requirement: &str,
) -> Result<Requirement, String> {
    if requirement.trim().is_empty() {
        return Err(format!("{kind} {name} has an empty requirement"));
    }
    requirement.parse::<Requirement>().map_err(|error| {
        format!("{kind} {name} has unsupported requirement {requirement}: {error}")
    })
}

fn normalize_record(package: &PackageRecord) -> Result<NormalizedRecord, String> {
    if !current_platform_matches(&package.platform) {
        return Err("version is incompatible with the current platform".to_owned());
    }
    let dependencies = package
        .dependencies
        .iter()
        .map(|(name, requirement)| {
            let parsed_name =
                name.parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| {
                        format!("dependency {name} has an unsupported name: {error}")
                    })?;
            let parsed_requirement = parse_registry_requirement(name, "dependency", requirement)?;
            Ok((parsed_name, parsed_requirement))
        })
        .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
    let peer_dependencies = package
        .peer_dependencies
        .iter()
        .map(|(name, requirement)| {
            let parsed_name =
                name.parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| {
                        format!("peer dependency {name} has an unsupported name: {error}")
                    })?;
            let parsed_requirement =
                parse_registry_requirement(name, "peer dependency", requirement)?;
            Ok((parsed_name, parsed_requirement))
        })
        .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
    let optional_peer_dependencies = package
        .optional_peer_dependencies
        .iter()
        .map(|name| {
            name.parse::<PackageName>()
                .map_err(|error: tapid_core::DomainError| {
                    format!("optional peer dependency {name} has an unsupported name: {error}")
                })
        })
        .collect::<Result<BTreeSet<PackageName>, String>>()?;
    if let Some(name) = optional_peer_dependencies
        .iter()
        .find(|name| !peer_dependencies.contains_key(*name))
    {
        return Err(format!(
            "optional peer metadata refers to undeclared peer {name}"
        ));
    }
    let metadata = PackageVersionMetadata {
        name: package.name.clone(),
        version: package.version.clone(),
        dependencies,
        peer_dependencies: peer_dependencies.clone(),
        optional_peer_dependencies,
    };
    let optional_dependencies = package
        .optional_dependencies
        .iter()
        .map(|(name, requirement)| {
            let parsed_name =
                name.parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| {
                        format!("optional dependency {name} has an unsupported name: {error}")
                    })?;
            let parsed_requirement =
                parse_registry_requirement(name, "optional dependency", requirement)?;
            Ok((parsed_name, parsed_requirement))
        })
        .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
    Ok(NormalizedRecord {
        metadata,
        optional_dependencies,
    })
}

type PackageRecordKey = (String, String, String);
type ResolvedRecords = (Resolution, BTreeMap<PackageRecordKey, PackageRecord>);

#[cfg(test)]
thread_local! {
    static RESOLVER_METADATA_BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RESOLVER_METADATA_VERSION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RESOLVER_METADATA_PARENT_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn insert_records<F>(
    records: &mut BTreeMap<PackageRecordKey, PackageRecord>,
    normalized: &mut NormalizedRecords,
    metadata: &mut Vec<RegistryMetadata>,
    overrides: &BTreeMap<PackageName, Requirement>,
    packages: Vec<PackageRecord>,
    registry_for_dependency: &mut F,
) -> Result<(), String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
{
    let mut inserted_keys = BTreeSet::new();
    let mut inserted_names = BTreeMap::<String, BTreeSet<PackageName>>::new();
    for mut package in packages {
        if package.registry.to_string() == NPM {
            for (name, requirement) in overrides {
                let name = name.to_string();
                if let Some(dependency) = package.dependencies.get_mut(&name) {
                    *dependency = requirement.raw.clone();
                }
                if let Some(dependency) = package.optional_dependencies.get_mut(&name) {
                    *dependency = requirement.raw.clone();
                }
            }
        }
        let registry = package.registry.to_string();
        let key = (
            registry.clone(),
            package.name.to_string(),
            package.version.to_string(),
        );
        inserted_names
            .entry(registry)
            .or_default()
            .insert(package.name.clone());
        normalized.insert(key.clone(), normalize_record(&package));
        records.insert(key.clone(), package);
        inserted_keys.insert(key);
    }

    let mut candidates = BTreeMap::<(String, String), Vec<&PackageRecordKey>>::new();
    for key in records.keys() {
        candidates
            .entry((key.0.clone(), key.1.clone()))
            .or_default()
            .push(key);
    }
    let candidate_matches =
        |registry: &RegistryOrigin, name: &PackageName, requirement: &Requirement| {
            candidates
                .get(&(registry.to_string(), name.to_string()))
                .into_iter()
                .flatten()
                .any(|key| {
                    normalized
                        .get(*key)
                        .and_then(|record| record.as_ref().ok())
                        .is_some()
                        && records.get(*key).is_some_and(|candidate| {
                            current_platform_matches(&candidate.platform)
                                && requirement.matches(&candidate.version)
                        })
                })
        };

    let mut additions = BTreeMap::<RegistryOrigin, Vec<PackageVersionMetadata>>::new();
    for key in &inserted_keys {
        let Some(record) = normalized.get(key).and_then(|record| record.as_ref().ok()) else {
            continue;
        };
        #[cfg(test)]
        RESOLVER_METADATA_VERSION_VISITS.set(RESOLVER_METADATA_VERSION_VISITS.get() + 1);
        let package = records.get(key).expect("record inserted before metadata");
        let mut package_metadata = record.metadata.clone();
        for (name, requirement) in &record.optional_dependencies {
            let target_registry = registry_for_dependency(&package.registry, name)?;
            if candidate_matches(&target_registry, name, requirement) {
                package_metadata
                    .dependencies
                    .insert(name.clone(), requirement.clone());
            }
        }
        additions
            .entry(package.registry.clone())
            .or_default()
            .push(package_metadata);
    }

    for (registry, additions) in additions {
        let registry_index =
            if let Some(index) = metadata.iter().position(|entry| entry.registry == registry) {
                index
            } else {
                metadata.push(
                    RegistryMetadata::normalize(registry.clone(), Vec::new())
                        .map_err(|error| error.to_string())?,
                );
                metadata.sort_by(|left, right| left.registry.cmp(&right.registry));
                metadata
                    .iter()
                    .position(|entry| entry.registry == registry)
                    .expect("inserted registry metadata")
            };
        let mut combined = std::mem::take(&mut metadata[registry_index].packages);
        combined.extend(additions);
        metadata[registry_index] =
            RegistryMetadata::normalize(registry, combined).map_err(|error| error.to_string())?;
    }

    for registry_metadata in metadata {
        let parent_registry = registry_metadata.registry.clone();
        for parent in &mut registry_metadata.packages {
            #[cfg(test)]
            RESOLVER_METADATA_PARENT_VISITS.set(RESOLVER_METADATA_PARENT_VISITS.get() + 1);
            let parent_key = (
                parent_registry.to_string(),
                parent.name.to_string(),
                parent.version.to_string(),
            );
            let Some(parent_record) = normalized
                .get(&parent_key)
                .and_then(|record| record.as_ref().ok())
            else {
                continue;
            };
            for (name, requirement) in &parent_record.optional_dependencies {
                let target_registry = registry_for_dependency(&parent_registry, name)?;
                if inserted_names
                    .get(&target_registry.to_string())
                    .is_some_and(|names| names.contains(name))
                    && candidate_matches(&target_registry, name, requirement)
                {
                    parent
                        .dependencies
                        .insert(name.clone(), requirement.clone());
                }
            }
        }
    }
    Ok(())
}

fn discarded_version_diagnostics(
    normalized: &NormalizedRecords,
    registry: &str,
    name: &str,
) -> String {
    normalized
        .iter()
        .filter_map(|((candidate_registry, candidate_name, version), result)| {
            (candidate_registry == registry && candidate_name == name)
                .then(|| {
                    result
                        .as_ref()
                        .err()
                        .map(|reason| format!("discarded version {version}: {reason}"))
                })
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn metadata_progress_checkpoint(fetches: usize) -> bool {
    fetches == 1 || fetches.is_multiple_of(50)
}

fn artifact_progress_checkpoint(completed: usize, total: usize) -> bool {
    total > 0 && (completed == 1 || completed == total || completed.is_multiple_of(50))
}

fn report_metadata_progress(fetches: usize) {
    if metadata_progress_checkpoint(fetches) {
        eprintln!("Registry metadata progress: {fetches} package(s) fetched");
    }
}

/// Resolves incrementally, fetching metadata only when the resolver reaches a
/// package on its currently selected graph.
#[cfg(test)]
fn resolve_with_fetch<F>(roots: &[Dependency], fetch: F) -> Result<ResolvedRecords, String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(
        roots,
        |parent, _| Ok(parent.clone()),
        &BTreeMap::new(),
        fetch,
    )
}

#[cfg(test)]
fn resolve_with_overrides<F>(
    roots: &[Dependency],
    overrides: &BTreeMap<PackageName, Requirement>,
    fetch: F,
) -> Result<ResolvedRecords, String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(roots, |parent, _| Ok(parent.clone()), overrides, fetch)
}

#[cfg(test)]
fn resolve_with_fetch_routed<R, F>(
    roots: &[Dependency],
    registry_for_dependency: R,
    fetch: F,
) -> Result<ResolvedRecords, String>
where
    R: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(roots, registry_for_dependency, &BTreeMap::new(), fetch)
}

fn resolve_with_fetch_routed_and_overrides<R, F>(
    roots: &[Dependency],
    mut registry_for_dependency: R,
    overrides: &BTreeMap<PackageName, Requirement>,
    mut fetch: F,
) -> Result<ResolvedRecords, String>
where
    R: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    let mut fetched = BTreeSet::<(String, String)>::new();
    let mut records = BTreeMap::<PackageRecordKey, PackageRecord>::new();
    let mut normalized = NormalizedRecords::new();
    let mut metadata = Vec::<RegistryMetadata>::new();

    loop {
        #[cfg(test)]
        RESOLVER_METADATA_BUILD_COUNT.set(RESOLVER_METADATA_BUILD_COUNT.get() + 1);

        match resolve_graph_with_routing(
            roots,
            &metadata,
            ResolutionOptions::default(),
            |parent, dependency| registry_for_dependency(parent, dependency),
        ) {
            Ok(resolution) => {
                let mut optional_frontier = BTreeSet::<(RegistryOrigin, PackageName)>::new();
                for parent in &resolution.selected {
                    let key = (
                        parent.registry.to_string(),
                        parent.name.to_string(),
                        parent.version.to_string(),
                    );
                    if let Some(record) = records.get(&key) {
                        for raw_name in record.optional_dependencies.keys() {
                            let name: PackageName = raw_name
                                .parse()
                                .map_err(|error: tapid_core::DomainError| error.to_string())?;
                            let registry = registry_for_dependency(&parent.registry, &name)?;
                            if !fetched.contains(&(registry.to_string(), name.to_string())) {
                                optional_frontier.insert((registry, name));
                            }
                        }
                    }
                }
                if !optional_frontier.is_empty() {
                    for (registry, name) in optional_frontier {
                        fetched.insert((registry.to_string(), name.to_string()));
                        report_metadata_progress(fetched.len());
                        insert_records(
                            &mut records,
                            &mut normalized,
                            &mut metadata,
                            overrides,
                            fetch(&registry, &name)?,
                            &mut registry_for_dependency,
                        )?;
                    }
                    continue;
                }
                return Ok((resolution, records));
            }
            Err(ResolveError::MissingMetadata { packages }) => {
                for (registry, name) in packages {
                    let key = (registry.clone(), name.clone());
                    if !fetched.insert(key) {
                        let discarded =
                            discarded_version_diagnostics(&normalized, &registry, &name);
                        let error = format!(
                            "resolution failed: metadata for {registry}:{name} remains unavailable"
                        );
                        return if discarded.is_empty() {
                            Err(error)
                        } else {
                            Err(format!("{error}; {discarded}"))
                        };
                    }
                    report_metadata_progress(fetched.len());
                    let registry: RegistryOrigin = registry
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?;
                    let name: PackageName = name
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?;
                    insert_records(
                        &mut records,
                        &mut normalized,
                        &mut metadata,
                        overrides,
                        fetch(&registry, &name)?,
                        &mut registry_for_dependency,
                    )?;
                }
            }
            Err(error @ ResolveError::MissingCandidate { .. })
            | Err(error @ ResolveError::Conflict { .. }) => {
                let (registry, name) = match &error {
                    ResolveError::MissingCandidate { registry, name, .. }
                    | ResolveError::Conflict { registry, name, .. } => {
                        (registry.clone(), name.clone())
                    }
                    _ => unreachable!(),
                };
                let key = (registry.clone(), name.clone());
                if !fetched.insert(key) {
                    let discarded = discarded_version_diagnostics(&normalized, &registry, &name);
                    return if discarded.is_empty() {
                        Err(format!("resolution failed: {error}"))
                    } else {
                        Err(format!("resolution failed: {error}; {discarded}"))
                    };
                }
                report_metadata_progress(fetched.len());
                let registry: RegistryOrigin = registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let name: PackageName = name
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                insert_records(
                    &mut records,
                    &mut normalized,
                    &mut metadata,
                    overrides,
                    fetch(&registry, &name)?,
                    &mut registry_for_dependency,
                )?;
            }
            Err(error) => return Err(format!("resolution failed: {error}")),
        }
    }
}

pub(crate) fn manifest_overrides(
    manifest: &PackageManifest,
) -> Result<BTreeMap<PackageName, Requirement>, String> {
    let mut overrides = BTreeMap::new();
    for (name, range) in manifest.overrides() {
        if name.starts_with("jsr:") {
            return Err(format!(
                "unsupported override target '{name}': npm registry package names only are supported"
            ));
        }
        let selector_name = name.strip_prefix("npm:").unwrap_or(name);
        let has_version_selector = if let Some(scoped) = selector_name.strip_prefix('@') {
            scoped.contains('@')
        } else {
            selector_name.contains('@')
        };
        if has_version_selector {
            return Err(format!(
                "unsupported override selector '{name}': version-qualified selectors are not supported"
            ));
        }
        let (registry, package) = dep_parts(name)?;
        if registry.to_string() != NPM {
            return Err(format!(
                "unsupported override target '{name}': npm registry package names only are supported"
            ));
        }
        let requirement = range
            .parse::<Requirement>()
            .map_err(|error| format!("invalid override '{name}' range '{range}': {error}"))?;
        match overrides.insert(package.clone(), requirement.clone()) {
            Some(previous) if previous != requirement => {
                return Err(format!("conflicting override declarations for '{package}'"));
            }
            _ => {}
        }
    }
    Ok(overrides)
}

#[cfg(test)]
pub(crate) fn manifest_roots(manifest: &PackageManifest) -> Result<Vec<Dependency>, String> {
    manifest_roots_with_config(manifest, &crate::registry::RegistryConfig::default())
}

#[cfg(test)]
fn manifest_roots_with_config(
    manifest: &PackageManifest,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<Vec<Dependency>, String> {
    let overrides = manifest_overrides(manifest)?;
    let mut roots = Vec::new();
    for (kind, map) in [
        ("dependencies", manifest.dependencies()),
        ("devDependencies", manifest.dev_dependencies()),
        ("optionalDependencies", manifest.optional_dependencies()),
    ] {
        for (name, range) in map {
            if range.starts_with("workspace:") {
                return Err(format!(
                    "unsupported workspace dependency reference: {name}@{range}; workspace linking is not implemented"
                ));
            }
            let (registry, package) = dependency_identity(registry_config, name)?;
            let requirement = range.parse::<Requirement>().map_err(|error| {
                format!("invalid {kind} dependency '{name}' range '{range}': {error}")
            })?;
            roots.push(Dependency::new(registry, package, requirement));
        }
    }
    for dependency in &roots {
        if overrides
            .get(&dependency.name)
            .is_some_and(|override_requirement| {
                override_requirement.raw != dependency.requirement.raw
            })
        {
            return Err(format!(
                "unsupported direct dependency override for '{}': npm requires the override range to match the declared dependency range",
                dependency.name
            ));
        }
    }
    Ok(roots)
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

fn workspace_root_resolution(
    manifest: &PackageManifest,
    workspace: &WorkspaceMaterialization,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<WorkspaceRootResolution, String> {
    let overrides = manifest_overrides(manifest)?;
    let mut roots = Vec::new();
    let mut direct_root_identities = BTreeSet::new();
    let mut registry_dependencies = Vec::with_capacity(workspace.registry_dependencies.len());
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
                workspace_root_keys.push(LockfilePackageKey::workspace(source.clone()).to_string());
                continue;
            }
            if range.starts_with("workspace:") {
                return Err(format!(
                    "workspace dependency '{name}@{range}' has no matching local workspace member; refusing registry fallback"
                ));
            }
            let (registry, package) = dependency_identity(registry_config, name)?;
            let requirement = range.parse::<Requirement>().map_err(|error| {
                format!("invalid {kind} dependency '{name}' range '{range}': {error}")
            })?;
            if overrides
                .get(&package)
                .is_some_and(|override_requirement| override_requirement.raw != requirement.raw)
            {
                return Err(format!(
                    "unsupported direct dependency override for '{package}': npm requires the override range to match the declared dependency range"
                ));
            }
            direct_root_identities.insert((registry.clone(), package.clone()));
            roots.push(Dependency::new(registry, package, requirement));
        }
    }
    for dependency in &workspace.registry_dependencies {
        let requirement = overrides
            .get(&dependency.package)
            .cloned()
            .unwrap_or_else(|| dependency.requirement.clone());
        roots.push(Dependency::new(
            dependency.registry.clone(),
            dependency.package.clone(),
            requirement.clone(),
        ));
        let mut resolved_dependency = dependency.clone();
        resolved_dependency.requirement = requirement;
        registry_dependencies.push(resolved_dependency);
    }
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
            let registry: RegistryOrigin = p
                .registry
                .parse()
                .map_err(|_| format!("invalid fixture registry {}", p.registry))?;
            let name: PackageName = p
                .name
                .parse()
                .map_err(|_| format!("invalid fixture package {}", p.name))?;
            let version: PackageVersion = p
                .version
                .parse()
                .map_err(|_| format!("invalid fixture version {}", p.version))?;
            let integrity = p
                .integrity
                .clone()
                .map(|v| {
                    v.parse()
                        .map_err(|_| format!("invalid fixture integrity {v}"))
                })
                .transpose()?;
            if registry.to_string() == NPM && integrity.is_none() && !allow_missing_integrity {
                return Err(format!(
                    "fixture npm metadata for {name}@{version} is missing dist.integrity; pass --allow-unverified-registry-artifacts for an explicit compatibility exception"
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
    store
        .recover_transactions()
        .map_err(|error| format!("cannot prepare shared store for recovery: {error}"))?;
    fs::create_dir_all(store.root()).map_err(|e| format!("cannot create store: {e}"))?;
    let mut store_transaction = store.transaction();
    let mut lock = Lockfile::new(&root_digest(project)?).map_err(|e| e.to_string())?;
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
                .ok_or_else(|| format!("missing artifact metadata: {id}"))?;
            records.insert(key3.clone(), p.clone());
            p
        };
        let platform_context = selected_platform_context_for(
            std::env::consts::OS,
            std::env::consts::ARCH,
            current_libc(),
            &record.platform,
        )?;
        platform_contexts.insert(id.clone(), platform_context.clone());
        let bytes = if record.fixture {
            if let Some(encoded) = record.artifact.strip_prefix("base64:") {
                STANDARD
                    .decode(encoded)
                    .map_err(|e| format!("invalid artifact encoding: {e}"))?
            } else {
                fs::read(&record.artifact)
                    .map_err(|e| format!("cannot read artifact {}: {e}", record.artifact))?
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
            .map_err(|e| format!("cannot download {id}: {e}"))?;
            if response.status != 200 {
                return Err(format!("cannot download {}: HTTP {}", id, response.status));
            }
            response.body
        };
        let actual = integrity(&bytes);
        if record
            .integrity
            .as_ref()
            .is_some_and(|expected| !integrity_matches(expected, &actual))
        {
            return Err(format!("integrity mismatch for {id}"));
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
        .map_err(|e| format!("cannot extract {id}: {e}"))?;
        let tree_digest: ArtifactDigest = canonical_tree_digest(&temp)
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
        let tree = store_transaction
            .stage_verified_tree(&tree_digest, &temp)
            .map_err(|e| e.to_string())?;
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
        .map_err(|e| e.to_string())?;
        if !record.fixture {
            locked
                .set_artifact_url(&record.artifact)
                .map_err(|e| e.to_string())?;
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
            tree: VerifiedTreeReference::new(&tree_digest.to_string(), &tree)
                .map_err(|e| e.to_string())?,
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
    let locked_packages: Result<Vec<_>, String> = packages
        .values()
        .map(|(locked, _, id)| {
            let mut locked = locked.clone();
            for edge in dependencies_by_parent.get(id).into_iter().flatten() {
                let target = &edge.child;
                let target_platform = platform_contexts
                    .get(target)
                    .ok_or_else(|| format!("missing platform context for {target}"))?;
                let target_key = LockfilePackageKey::new(
                    target.registry.clone(),
                    target.name.clone(),
                    target.version.clone(),
                    resolution.peer_contexts.get(target).unwrap_or(&empty_peer),
                    target_platform,
                )
                .to_string();
                locked
                    .add_dependency(&edge.dependency.to_string(), &target_key)
                    .map_err(|e| e.to_string())?;
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
        let dependency_name = dependency.package.to_string();
        if member
            .dependencies()
            .get(&dependency_name)
            .is_some_and(|existing| existing != &target_key)
        {
            return Err(format!(
                "workspace member '{}' declares ambiguous registry identities for dependency '{}'; refusing to overwrite a lockfile edge",
                dependency.member_key, dependency_name
            ));
        }
        member
            .add_dependency(&dependency_name, &target_key)
            .map_err(|error| error.to_string())?;
    }
    lock.insert_graph(locked_packages?, workspace_locked)
        .map_err(|e| e.to_string())?;
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
    lock.set_roots(root_keys).map_err(|e| e.to_string())?;
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
            .ok_or_else(|| format!("missing parent instance for {}", edge.parent))?;
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
            .ok_or_else(|| format!("missing child instance for {}", edge.child))?;
        edge_list.push(DependencyEdge {
            parent: parent.clone(),
            child: child.clone(),
        });
    }
    for id in &resolution.roots {
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
        root_deps.push(instance.clone());
    }
    Ok((
        lock,
        LayoutInput {
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
mod tests {
    use super::*;
    #[test]
    fn custom_private_origins_use_the_npm_metadata_protocol() {
        let registry: RegistryOrigin = "https://127.0.0.1:9".parse().unwrap();
        let transport =
            HttpsTransport::authenticated_metadata([registry.to_string()], std::iter::empty())
                .unwrap();
        let config = crate::registry::RegistryConfig::from_toml(
            "[registries.default]\nurl='https://127.0.0.1:9'\n",
        )
        .unwrap();
        let error = remote_records(
            &transport,
            &config,
            &registry,
            &"private-package".parse().unwrap(),
            false,
        )
        .err()
        .unwrap();
        assert!(error.contains("cannot fetch metadata"), "{error}");
        assert!(!error.contains("unsupported registry origin"), "{error}");
    }

    #[test]
    fn private_package_resolves_unscoped_transitive_dependency_from_public_npm() {
        let config = crate::registry::RegistryConfig::from_toml(
            r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
        )
        .unwrap();
        let (private_origin, package_name) = config.identity_for_spec("@acme/widget").unwrap();
        let root = Dependency::new(private_origin.clone(), package_name, "*".parse().unwrap());
        let mut private_package = named_record("@acme/widget", "1.0.0", &[("left-pad", "^1")]);
        private_package.registry = private_origin.clone();
        let public_origin: RegistryOrigin = NPM.parse().unwrap();
        let mut requests = Vec::new();

        let result = resolve_with_fetch_routed(
            &[root],
            |parent, dependency| config.registry_for_dependency(parent, dependency),
            |registry, name| {
                requests.push((registry.to_string(), name.to_string()));
                match name.to_string().as_str() {
                    "@acme/widget" => Ok(vec![private_package.clone()]),
                    "left-pad" => Ok(vec![named_record("left-pad", "1.3.0", &[])]),
                    other => panic!("unexpected metadata request for {other}"),
                }
            },
        );

        let (resolution, _) = match result {
            Ok(resolution) => resolution,
            Err(error) => panic!("mixed-origin resolution failed: {error}"),
        };
        let child = resolution
            .selected
            .iter()
            .find(|package| package.name.to_string() == "left-pad")
            .expect("public transitive dependency must be selected");
        assert_eq!(child.registry, public_origin);
        assert!(requests.contains(&(NPM.to_owned(), "left-pad".to_owned())));
    }

    #[test]
    fn public_package_resolves_scoped_transitive_dependency_from_private_registry() {
        let config = crate::registry::RegistryConfig::from_toml(
            r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
        )
        .unwrap();
        let private_origin = config.origin_for("@acme/helper").unwrap();
        let root = Dependency::new(
            NPM.parse().unwrap(),
            "public-app".parse().unwrap(),
            "*".parse().unwrap(),
        );
        let public_package = named_record("public-app", "1.0.0", &[("@acme/helper", "^1")]);
        let mut private_package = named_record("@acme/helper", "1.2.0", &[]);
        private_package.registry = private_origin.clone();
        let mut requests = Vec::new();

        let result = resolve_with_fetch_routed(
            &[root],
            |parent, dependency| config.registry_for_dependency(parent, dependency),
            |registry, name| {
                requests.push((registry.to_string(), name.to_string()));
                match name.to_string().as_str() {
                    "public-app" => Ok(vec![public_package.clone()]),
                    "@acme/helper" => Ok(vec![private_package.clone()]),
                    other => panic!("unexpected metadata request for {other}"),
                }
            },
        );

        let (resolution, _) = match result {
            Ok(resolution) => resolution,
            Err(error) => panic!("mixed-origin resolution failed: {error}"),
        };
        let child = resolution
            .selected
            .iter()
            .find(|package| package.name.to_string() == "@acme/helper")
            .expect("private scoped dependency must be selected");
        assert_eq!(child.registry, private_origin);
        assert!(requests.contains(&(private_origin.to_string(), "@acme/helper".to_owned())));
    }

    #[test]
    fn optional_dependency_uses_its_configured_registry_route() {
        let config = crate::registry::RegistryConfig::from_toml(
            r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
        )
        .unwrap();
        let private_origin = config.origin_for("@acme/feature").unwrap();
        let root = Dependency::new(
            NPM.parse().unwrap(),
            "public-app".parse().unwrap(),
            "*".parse().unwrap(),
        );
        let mut public_package = named_record("public-app", "1.0.0", &[]);
        public_package
            .optional_dependencies
            .insert("@acme/feature".into(), "^1".into());
        let mut private_feature = named_record("@acme/feature", "1.1.0", &[]);
        private_feature.registry = private_origin.clone();

        let result = resolve_with_fetch_routed(
            &[root],
            |parent, dependency| config.registry_for_dependency(parent, dependency),
            |_, name| match name.to_string().as_str() {
                "public-app" => Ok(vec![public_package.clone()]),
                "@acme/feature" => Ok(vec![private_feature.clone()]),
                other => panic!("unexpected metadata request for {other}"),
            },
        );
        let (resolution, _) = match result {
            Ok(resolution) => resolution,
            Err(error) => panic!("optional dependency resolution failed: {error}"),
        };

        assert!(resolution.selected.iter().any(|package| {
            package.name.to_string() == "@acme/feature" && package.registry == private_origin
        }));
    }

    #[test]
    fn peer_provider_uses_the_configured_registry_route() {
        let config = crate::registry::RegistryConfig::from_toml(
            r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
        )
        .unwrap();
        let private_origin = config.origin_for("@acme/host").unwrap();
        let roots = vec![
            Dependency::new(
                NPM.parse().unwrap(),
                "plugin".parse().unwrap(),
                "*".parse().unwrap(),
            ),
            Dependency::new(
                private_origin.clone(),
                "@acme/host".parse().unwrap(),
                "*".parse().unwrap(),
            ),
        ];
        let mut plugin = named_record("plugin", "1.0.0", &[]);
        plugin
            .peer_dependencies
            .insert("@acme/host".into(), "^1".into());
        let mut host = named_record("@acme/host", "1.2.0", &[]);
        host.registry = private_origin.clone();

        let result = resolve_with_fetch_routed(
            &roots,
            |parent, dependency| config.registry_for_dependency(parent, dependency),
            |_, name| match name.to_string().as_str() {
                "plugin" => Ok(vec![plugin.clone()]),
                "@acme/host" => Ok(vec![host.clone()]),
                other => panic!("unexpected metadata request for {other}"),
            },
        );

        let (resolution, _) = match result {
            Ok(resolution) => resolution,
            Err(error) => panic!("cross-origin peer resolution failed: {error}"),
        };
        assert!(resolution.selected.iter().any(|package| {
            package.name.to_string() == "@acme/host" && package.registry == private_origin
        }));
    }

    #[test]
    fn same_origin_authenticated_and_public_routes_use_separate_transports() {
        let config = crate::registry::RegistryConfig::from_toml(
            "[registries.'@acme']\nurl='https://registry.npmjs.org'\ntoken-env='ACME_TOKEN'\n",
        )
        .unwrap();
        let authenticated = config
            .route_with_env("@acme/private", |_| Some("fixture-only-token".into()))
            .unwrap();
        let public = config.route("left-pad").unwrap();
        assert_eq!(authenticated.origin, public.origin);
        assert!(authenticated.token.is_some());
        assert!(public.token.is_none());
        assert_ne!(authenticated.policy, public.policy);

        for artifact in [false, true] {
            let mut cache = BTreeMap::new();
            let authenticated_transport = transport_for_route(
                &mut cache,
                authenticated.clone(),
                &[NPM.to_owned()],
                artifact,
            )
            .unwrap() as *const HttpsTransport;
            let public_transport =
                transport_for_route(&mut cache, public.clone(), &[NPM.to_owned()], artifact)
                    .unwrap() as *const HttpsTransport;

            assert_ne!(authenticated_transport, public_transport);
            assert_eq!(cache.len(), 2);
        }
    }

    #[test]
    fn artifact_progress_is_emitted_at_bounded_completion_checkpoints() {
        let checkpoints = (1..=625)
            .filter(|completed| artifact_progress_checkpoint(*completed, 625))
            .collect::<Vec<_>>();

        assert_eq!(checkpoints.first(), Some(&1));
        assert_eq!(checkpoints.last(), Some(&625));
        assert!(checkpoints.len() <= 14);
    }

    #[test]
    fn manifest_overrides_rejects_version_qualified_selectors() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","overrides":{"typescript@*":"$typescript"}}"#,
        )
        .unwrap();

        let error = manifest_overrides(&manifest).unwrap_err();
        assert!(error.contains("unsupported override selector"));
        assert!(error.contains("typescript@*"));
    }

    #[test]
    fn manifest_roots_rejects_conflicting_direct_dependency_override() {
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"postcss":"8.4.31"},"overrides":{"postcss":"8.5.28"}}"#,
        )
        .unwrap();

        let error = manifest_roots(&manifest).unwrap_err();
        assert!(error.contains("unsupported direct dependency override"));
        assert!(error.contains("postcss"));
    }

    #[test]
    fn root_override_replaces_a_transitive_dependency_requirement() {
        let root = Dependency::new(
            NPM.parse().unwrap(),
            "next".parse().unwrap(),
            "1.0.0".parse().unwrap(),
        );
        let overrides = BTreeMap::from([("postcss".parse().unwrap(), "8.5.28".parse().unwrap())]);
        let (resolution, records) =
            resolve_with_overrides(&[root], &overrides, |_, name| {
                match name.to_string().as_str() {
                    "next" => Ok(vec![named_record(
                        "next",
                        "1.0.0",
                        &[("postcss", "8.4.31")],
                    )]),
                    "postcss" => Ok(vec![
                        named_record("postcss", "8.4.31", &[]),
                        named_record("postcss", "8.5.28", &[]),
                    ]),
                    _ => panic!("unexpected metadata request for {name}"),
                }
            })
            .unwrap();

        assert!(resolution.selected.iter().any(|package| {
            package.name.to_string() == "postcss" && package.version.to_string() == "8.5.28"
        }));
        assert!(!resolution.selected.iter().any(|package| {
            package.name.to_string() == "postcss" && package.version.to_string() == "8.4.31"
        }));
        assert_eq!(
            records[&(NPM.into(), "next".into(), "1.0.0".into())].dependencies["postcss"],
            "8.5.28"
        );
    }

    #[test]
    fn wide_required_frontier_rebuilds_metadata_only_once_per_wave() {
        RESOLVER_METADATA_BUILD_COUNT.set(0);
        let registry: RegistryOrigin = NPM.parse().unwrap();
        let roots = (0..64)
            .map(|index| {
                Dependency::new(
                    registry.clone(),
                    format!("pkg-{index}").parse().unwrap(),
                    "1.0.0".parse().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let mut fetched = Vec::new();

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            fetched.push(name.to_string());
            Ok(vec![named_record(&name.to_string(), "1.0.0", &[])])
        })
        .unwrap();

        assert_eq!(resolution.selected.len(), 64);
        assert_eq!(fetched.len(), 64);
        assert_eq!(RESOLVER_METADATA_BUILD_COUNT.get(), 2);
    }

    #[test]
    fn deep_required_frontier_normalizes_each_version_once() {
        RESOLVER_METADATA_VERSION_VISITS.set(0);
        let registry: RegistryOrigin = NPM.parse().unwrap();
        let root = Dependency::new(registry, "pkg-0".parse().unwrap(), "1.0.0".parse().unwrap());

        let (resolution, _) = resolve_with_fetch(&[root], |_, name| {
            let index = name.to_string()[4..].parse::<usize>().unwrap();
            let mut package = named_record(&name.to_string(), "1.0.0", &[]);
            if index < 31 {
                package
                    .dependencies
                    .insert(format!("pkg-{}", index + 1), "1.0.0".into());
            }
            Ok(vec![package])
        })
        .unwrap();

        assert_eq!(resolution.selected.len(), 32);
        assert_eq!(RESOLVER_METADATA_VERSION_VISITS.get(), 32);
    }

    #[test]
    fn one_packument_updates_parent_metadata_once_per_version() {
        RESOLVER_METADATA_PARENT_VISITS.set(0);
        let registry: RegistryOrigin = NPM.parse().unwrap();
        let root = Dependency::new(registry, "large".parse().unwrap(), "*".parse().unwrap());
        let version_count = 256;

        let (resolution, _) = resolve_with_fetch(&[root], |_, name| {
            assert_eq!(name.to_string(), "large");
            Ok((0..version_count)
                .map(|patch| named_record("large", &format!("1.0.{patch}"), &[]))
                .collect())
        })
        .unwrap();

        assert_eq!(resolution.selected[0].version.to_string(), "1.0.255");
        assert!(
            RESOLVER_METADATA_PARENT_VISITS.get() <= version_count * 2,
            "one packument caused {} parent metadata visits",
            RESOLVER_METADATA_PARENT_VISITS.get()
        );
    }

    #[test]
    fn metadata_progress_is_emitted_at_bounded_checkpoints() {
        let checkpoints = (1..=612)
            .filter(|fetches| metadata_progress_checkpoint(*fetches))
            .collect::<Vec<_>>();

        assert_eq!(checkpoints.first(), Some(&1));
        assert_eq!(checkpoints.last(), Some(&600));
        assert!(checkpoints.len() <= 13);
    }

    #[test]
    fn padded_and_unpadded_sha512_inputs_canonicalize_and_verify() {
        let bytes = b"archive bytes";
        let padded = integrity(bytes);
        let unpadded_text = padded.to_string().trim_end_matches('=').to_owned();
        let unpadded: PackageIntegrity = unpadded_text.parse().unwrap();

        assert_ne!(unpadded_text, padded.to_string());
        assert_eq!(unpadded.to_string(), padded.to_string());
        let different = integrity(b"different bytes");
        assert!(integrity_matches(&padded, &integrity(bytes)));
        assert!(integrity_matches(&unpadded, &integrity(bytes)));
        assert!(!integrity_matches(&padded, &different));
        assert!(!integrity_matches(&unpadded, &different));
    }

    #[test]
    fn explicit_registry_prefixes_are_mapped_safely() {
        let (r, n) = dep_parts("jsr:@std/path").unwrap();
        assert_eq!(r.to_string(), JSR);
        assert_eq!(n.to_string(), "@std/path");
        let (r, n) = dep_parts("npm:foo").unwrap();
        assert_eq!(r.to_string(), NPM);
        assert_eq!(n.to_string(), "foo");
    }

    fn record(version: &str, dependency_requirement: Option<&str>) -> PackageRecord {
        named_record(
            "framer-motion",
            version,
            &dependency_requirement
                .map(|requirement| vec![("popmotion", requirement)])
                .unwrap_or_default(),
        )
    }

    fn named_record(name: &str, version: &str, dependencies: &[(&str, &str)]) -> PackageRecord {
        PackageRecord {
            registry: NPM.parse().unwrap(),
            name: name.parse().unwrap(),
            version: version.parse().unwrap(),
            integrity: None,
            artifact: format!("https://registry.npmjs.org/{name}/-/{version}.tgz"),
            dependencies: dependencies
                .iter()
                .map(|(name, requirement)| ((*name).into(), (*requirement).into()))
                .collect(),
            peer_dependencies: BTreeMap::new(),
            optional_peer_dependencies: BTreeSet::new(),
            optional_dependencies: BTreeMap::new(),
            platform: PackagePlatform::unrestricted(),
            fixture: false,
        }
    }

    #[test]
    fn fixture_metadata_preserves_peer_dependencies_separately() {
        let fixture: Fixture = serde_json::from_str(
            r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","artifact":"base64:AA==","dependencies":{"runtime":"^1.0.0"},"peerDependencies":{"host":"^2.0.0"},"optionalPeerDependencies":["host"]}]}"#,
        )
        .unwrap();

        assert_eq!(fixture.packages[0].dependencies["runtime"], "^1.0.0");
        assert_eq!(fixture.packages[0].peer_dependencies["host"], "^2.0.0");
        assert!(
            fixture.packages[0]
                .optional_peer_dependencies
                .contains("host")
        );
        assert!(!fixture.packages[0].dependencies.contains_key("host"));
    }

    #[test]
    fn peer_metadata_is_preserved_separately_for_peer_context_resolution() {
        let mut record = named_record("plugin", "1.0.0", &[("runtime", "^1.0.0")]);
        record
            .peer_dependencies
            .insert("host".into(), "^1.0.0".into());

        let normalized = normalize_record(&record).unwrap();
        assert_eq!(normalized.metadata.dependencies.len(), 1);
        assert_eq!(
            normalized.metadata.peer_dependencies[&"host".parse().unwrap()].raw,
            "^1.0.0"
        );
    }

    #[test]
    fn normalization_preserves_optional_peer_markers() {
        let mut record = named_record("plugin", "1.0.0", &[]);
        record
            .peer_dependencies
            .insert("host".into(), "^2.0.0".into());
        record.optional_peer_dependencies.insert("host".into());

        let normalized = normalize_record(&record).unwrap();
        assert!(
            normalized
                .metadata
                .optional_peer_dependencies
                .contains(&"host".parse().unwrap())
        );
    }

    #[test]
    fn normalization_rejects_optional_marker_for_undeclared_peer() {
        let mut record = named_record("plugin", "1.0.0", &[]);
        record.optional_peer_dependencies.insert("host".into());

        let error = match normalize_record(&record) {
            Err(error) => error,
            Ok(_) => panic!("optional marker for an undeclared peer was accepted"),
        };
        assert!(error.contains("optional peer metadata refers to undeclared peer host"));
    }

    #[test]
    fn malformed_peer_requirement_fails_without_flattening() {
        let mut record = named_record("plugin", "1.0.0", &[("runtime", "^1.0.0")]);
        record
            .peer_dependencies
            .insert("host".into(), "not-a-range".into());

        let error = match normalize_record(&record) {
            Ok(_) => panic!("malformed peer metadata was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("peer dependency host has unsupported requirement"));
        assert!(!error.contains("ordinary"));
    }

    #[test]
    fn malformed_peer_name_fails_closed() {
        let mut record = named_record("plugin", "1.0.0", &[]);
        record
            .peer_dependencies
            .insert("../host".into(), "^1.0.0".into());

        let error = match normalize_record(&record) {
            Ok(_) => panic!("malformed peer metadata was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("peer dependency ../host has an unsupported name"));
    }

    #[test]
    fn selected_platform_constraints_produce_an_exact_lockfile_context() {
        let platform = PackagePlatform {
            os: vec!["darwin".into()],
            cpu: vec!["arm64".into()],
            libc: Vec::new(),
        };

        let context = selected_platform_context_for("macos", "aarch64", None, &platform).unwrap();

        assert_eq!(context.os.as_deref(), Some("darwin"));
        assert_eq!(context.cpu.as_deref(), Some("arm64"));
        assert_eq!(context.libc, None);
    }

    #[test]
    fn libc_constraints_follow_linux_only_npm_semantics() {
        let positive = PackagePlatform {
            os: Vec::new(),
            cpu: Vec::new(),
            libc: vec!["glibc".into()],
        };
        let exclusion_only = PackagePlatform {
            os: Vec::new(),
            cpu: Vec::new(),
            libc: vec!["!musl".into()],
        };

        assert!(platform_matches_for("macos", "aarch64", None, &positive));
        assert!(!platform_matches_for("linux", "x86_64", None, &positive));
        assert!(platform_matches_for(
            "linux",
            "x86_64",
            None,
            &exclusion_only
        ));
        assert_eq!(
            selected_platform_context_for("macos", "aarch64", None, &positive)
                .unwrap()
                .libc,
            None
        );
    }

    #[test]
    fn incompatible_package_versions_are_not_usable() {
        let mut package = named_record("native", "1.0.0", &[]);
        package.platform.os = vec!["definitely-not-this-platform".into()];

        assert!(usable_versions(vec![package]).is_empty());
    }

    #[test]
    fn unsupported_historical_dependencies_do_not_hide_usable_versions() {
        let versions = usable_versions(vec![
            record("2.9.5", Some("git+https://example.test/popmotion.git")),
            record("11.18.2", None),
        ]);

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "11.18.2");
    }

    #[test]
    fn empty_dependency_ranges_exclude_only_affected_versions() {
        let versions = usable_versions(vec![record("3.0.1", Some("")), record("4.0.5", None)]);

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "4.0.5");
    }

    #[test]
    fn production_normalization_rejects_empty_registry_dependency_ranges() {
        let package = record("3.0.1", Some(""));
        let error = match normalize_record(&package) {
            Ok(_) => panic!("empty registry dependency ranges must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("dependency"));
        assert!(error.contains("empty requirement"));
    }

    #[test]
    fn all_unsupported_versions_remain_unavailable_to_the_resolver() {
        let versions = usable_versions(vec![record(
            "2.9.5",
            Some("git+https://example.test/popmotion.git"),
        )]);

        assert!(versions.is_empty());
    }

    #[test]
    fn resolution_error_reports_discarded_version_and_requirement() {
        let roots = vec![Dependency::new(
            NPM.parse().unwrap(),
            "framer-motion".parse().unwrap(),
            "*".parse().unwrap(),
        )];
        let result = resolve_with_fetch(&roots, |_, _| {
            Ok(vec![record(
                "2.9.5",
                Some("git+https://example.test/popmotion.git"),
            )])
        });
        let error = match result {
            Ok(_) => panic!("unsupported versions must not resolve"),
            Err(error) => error,
        };

        assert!(error.contains("discarded version 2.9.5"), "{error}");
        assert!(
            error.contains("git+https://example.test/popmotion.git"),
            "{error}"
        );
    }

    #[test]
    fn selected_bare_major_dependency_range_remains_usable() {
        let roots = vec![Dependency::new(
            NPM.parse().unwrap(),
            "app".parse().unwrap(),
            "*".parse().unwrap(),
        )];

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            Ok(match name.to_string().as_str() {
                "app" => vec![named_record("app", "1.0.0", &[("inherits", "2")])],
                "inherits" => vec![
                    named_record("inherits", "1.0.0", &[]),
                    named_record("inherits", "2.0.0", &[]),
                    named_record("inherits", "2.0.4", &[]),
                    named_record("inherits", "3.0.0", &[]),
                ],
                other => panic!("unexpected metadata fetch for {other}"),
            })
        })
        .unwrap();

        let inherits = resolution
            .selected
            .iter()
            .find(|package| package.name.to_string() == "inherits")
            .expect("inherits must be selected");
        assert_eq!(inherits.version.to_string(), "2.0.4");
    }

    #[test]
    fn selected_npm_or_and_prerelease_ranges_remain_usable() {
        let versions = usable_versions(vec![named_record(
            "eslint-plugin-react",
            "7.37.5",
            &[
                ("jsx-ast-utils", "^2.4.1 || ^3.0.0"),
                ("resolve", "^2.0.0-next.5"),
            ],
        )]);

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "7.37.5");
    }

    #[test]
    fn unavailable_optional_requirement_does_not_fail_resolution() {
        let roots = vec![Dependency::new(
            NPM.parse().unwrap(),
            "app".parse().unwrap(),
            "*".parse().unwrap(),
        )];

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            Ok(match name.to_string().as_str() {
                "app" => {
                    let mut package = named_record("app", "1.0.0", &[]);
                    package
                        .optional_dependencies
                        .insert("native".into(), "2.0.0".into());
                    vec![package]
                }
                "native" => vec![named_record("native", "1.0.0", &[])],
                other => panic!("unexpected metadata fetch for {other}"),
            })
        })
        .unwrap();

        assert_eq!(resolution.selected.len(), 1);
        assert_eq!(resolution.selected[0].name.to_string(), "app");
    }

    #[test]
    fn unusable_optional_candidate_does_not_become_a_required_edge() {
        let roots = vec![Dependency::new(
            NPM.parse().unwrap(),
            "app".parse().unwrap(),
            "*".parse().unwrap(),
        )];

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            Ok(match name.to_string().as_str() {
                "app" => {
                    let mut package = named_record("app", "1.0.0", &[]);
                    package
                        .optional_dependencies
                        .insert("native".into(), "1.0.0".into());
                    vec![package]
                }
                "native" => vec![named_record(
                    "native",
                    "1.0.0",
                    &[("historical", "git+https://example.test/repo.git")],
                )],
                other => panic!("unexpected metadata fetch for {other}"),
            })
        })
        .unwrap();

        assert_eq!(resolution.selected.len(), 1);
        assert_eq!(resolution.selected[0].name.to_string(), "app");
    }

    #[test]
    fn incremental_resolution_fetches_and_selects_compatible_optional_dependencies() {
        let roots = vec![Dependency::new(
            NPM.parse().unwrap(),
            "app".parse().unwrap(),
            "*".parse().unwrap(),
        )];
        let mut fetched = Vec::new();

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            fetched.push(name.to_string());
            Ok(match name.to_string().as_str() {
                "app" => {
                    let mut package = named_record("app", "1.0.0", &[]);
                    package
                        .optional_dependencies
                        .insert("native".into(), "1.0.0".into());
                    vec![package]
                }
                "native" => vec![named_record("native", "1.0.0", &[])],
                other => panic!("unexpected metadata fetch for {other}"),
            })
        })
        .unwrap();

        assert_eq!(fetched, vec!["app", "native"]);
        assert!(
            resolution
                .selected
                .iter()
                .any(|id| id.name.to_string() == "native")
        );
        assert!(
            resolution
                .dependencies
                .iter()
                .any(|edge| edge.dependency.to_string() == "native")
        );
    }

    #[test]
    fn incremental_resolution_fetches_only_the_selected_versions_dependencies() {
        let roots = vec![Dependency::new(
            NPM.parse().unwrap(),
            "app".parse().unwrap(),
            "^2.0.0".parse().unwrap(),
        )];
        let mut fetched = Vec::new();

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            fetched.push(name.to_string());
            Ok(match name.to_string().as_str() {
                "app" => vec![
                    named_record("app", "1.0.0", &[("historical", "*")]),
                    named_record("app", "2.0.0", &[("selected", "*")]),
                ],
                "selected" => vec![named_record("selected", "1.0.0", &[])],
                other => panic!("unexpected metadata fetch for {other}"),
            })
        })
        .unwrap();

        assert_eq!(fetched, vec!["app", "selected"]);
        assert_eq!(
            resolution
                .selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "https://registry.npmjs.org:app@2.0.0",
                "https://registry.npmjs.org:selected@1.0.0",
            ]
        );
    }

    #[test]
    fn incremental_resolution_fetches_metadata_before_reporting_constraint_conflicts() {
        let roots = vec![
            Dependency::new(
                NPM.parse().unwrap(),
                "shared".parse().unwrap(),
                "^0.4.0".parse().unwrap(),
            ),
            Dependency::new(
                NPM.parse().unwrap(),
                "shared".parse().unwrap(),
                "^0.4.2".parse().unwrap(),
            ),
        ];
        let mut fetches = 0;

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            fetches += 1;
            assert_eq!(name.to_string(), "shared");
            Ok(vec![
                named_record("shared", "0.4.0", &[]),
                named_record("shared", "0.4.3", &[]),
            ])
        })
        .unwrap();

        assert_eq!(fetches, 1);
        assert_eq!(
            resolution.selected[0].to_string(),
            "https://registry.npmjs.org:shared@0.4.3"
        );
    }

    #[test]
    fn incremental_resolution_fetches_one_packument_for_multiple_selected_versions() {
        let roots = vec![
            Dependency::new(
                NPM.parse().unwrap(),
                "a".parse().unwrap(),
                "*".parse().unwrap(),
            ),
            Dependency::new(
                NPM.parse().unwrap(),
                "b".parse().unwrap(),
                "*".parse().unwrap(),
            ),
        ];
        let mut fetched = Vec::new();

        let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
            fetched.push(name.to_string());
            Ok(match name.to_string().as_str() {
                "a" => vec![named_record("a", "1.0.0", &[("debug", "^3.0.0")])],
                "b" => vec![named_record("b", "1.0.0", &[("debug", "^4.0.0")])],
                "debug" => vec![
                    named_record("debug", "3.2.7", &[]),
                    named_record("debug", "4.3.7", &[]),
                ],
                other => panic!("unexpected metadata fetch for {other}"),
            })
        })
        .unwrap();

        assert_eq!(fetched, vec!["a", "b", "debug"]);
        assert_eq!(
            resolution
                .selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "https://registry.npmjs.org:a@1.0.0",
                "https://registry.npmjs.org:b@1.0.0",
                "https://registry.npmjs.org:debug@3.2.7",
                "https://registry.npmjs.org:debug@4.3.7",
            ]
        );
    }
}
