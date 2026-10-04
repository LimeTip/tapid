use crate::application::outcome::{ErrorKind, OperationalError};
mod resolution;
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
    VerifiedTreeReference,
};
use tapid_lockfile::{LockedPackage, Lockfile, LockfilePackageKey, RegistryIntegrityProvenance};
use tapid_manifest::PackageManifest;
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

pub fn resolve_and_fetch(
    project: &Path,
    manifest: &PackageManifest,
    store: &Store,
    fixture_path: Option<&Path>,
    allow_missing_integrity: bool,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<
    (
        Lockfile,
        NamedLayoutInput,
        BTreeMap<String, PathBuf>,
        StoreTransaction,
    ),
    OperationalError,
> {
    let fixture = fixture_path
        .map(fixture)
        .transpose()
        .map_err(|error| OperationalError::new(ErrorKind::RegistryMetadata, error))?;
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
    let overrides = manifest_overrides(manifest)
        .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?;
    let roots = manifest_roots(manifest)
        .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?
        .into_iter()
        .map(|dependency| {
            if dependency.registry.to_string() == JSR {
                Ok(dependency)
            } else {
                let origin = registry_config
                    .origin_for_name(dependency.requirement.package_name(&dependency.name))?;
                Ok(Dependency::new(
                    origin,
                    dependency.name,
                    dependency.requirement,
                ))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
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

    store.recover_transactions().map_err(|error| {
        OperationalError::from(error).context("cannot prepare shared store for recovery")
    })?;
    fs::create_dir_all(store.root()).map_err(|e| {
        OperationalError::from_source(ErrorKind::Store, e).context("cannot create store")
    })?;
    let mut store_transaction = store.transaction();
    let mut lock = Lockfile::new(&root_digest(project)?).map_err(OperationalError::from)?;
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
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
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
                    .add_alias_dependency(&edge.dependency.to_string(), &target_key)
                    .map_err(|e| e.to_string())?;
            }
            Ok(locked)
        })
        .collect();
    lock.insert_packages(
        locked_packages.map_err(|e| OperationalError::new(ErrorKind::Lockfile, e))?,
    )
    .map_err(|e| e.to_string())?;
    lock.set_roots(resolution.roots.iter().map(|id| {
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
    }))
    .map_err(|e| e.to_string())?;
    lock.set_root_bindings(
        resolution
            .root_bindings
            .iter()
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
    .map_err(|error| error.to_string())?;
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
        edge_list.push(NamedDependencyEdge {
            parent: parent.clone(),
            dependency: NamedDependency {
                name: edge.dependency.clone(),
                child: child.clone(),
            },
        });
    }
    for ((_, name), id) in &resolution.root_bindings {
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
    Ok((
        lock,
        NamedLayoutInput {
            instances,
            root_dependencies: root_deps,
            dependency_edges: edge_list,
        },
        trees,
        store_transaction,
    ))
}

#[cfg(test)]
#[path = "online/tests.rs"]
mod tests;
