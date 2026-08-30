use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};
use tapid_archive::{ArchiveFormat, ArchiveLimits, canonical_tree_digest, extract_to};
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName, PackageVersion, RegistryOrigin};
use tapid_linker::{
    DependencyEdge, InstanceKey, LayoutInput, PackageInstance, VerifiedTreeReference,
};
use tapid_lockfile::{LockedPackage, Lockfile, LockfilePackageKey};
use tapid_manifest::PackageManifest;
use tapid_registry_client::{HttpsTransport, JsrRegistry, NpmRegistry, RegistryArtifact};
use tapid_resolver::{
    Dependency, PackageVersionMetadata, RegistryMetadata, Requirement, ResolutionOptions,
    resolve_graph,
};
use tapid_store::Store;

const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";

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
    registry: &RegistryOrigin,
    name: &PackageName,
    allow_missing_integrity: bool,
) -> Result<Vec<PackageRecord>, String> {
    let transport =
        HttpsTransport::standard().map_err(|e| format!("cannot create registry transport: {e}"))?;
    let artifacts: Vec<RegistryArtifact> = if registry.to_string() == JSR {
        JsrRegistry::new(transport, registry.clone()).fetch(&name.to_string())
    } else if registry.to_string() == NPM {
        NpmRegistry::new(transport, registry.clone())
            .fetch_with_options(&name.to_string(), allow_missing_integrity)
    } else {
        return Err(format!("unsupported registry origin: {registry}"));
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
            fixture: false,
        })
        .collect())
}

pub fn resolve_and_fetch(
    project: &Path,
    manifest: &PackageManifest,
    store: &Store,
    fixture_path: Option<&Path>,
    allow_missing_integrity: bool,
) -> Result<(Lockfile, LayoutInput, BTreeMap<String, PathBuf>), String> {
    let fixture = fixture_path.map(fixture).transpose()?;
    fs::create_dir_all(store.root()).map_err(|e| format!("cannot create store: {e}"))?;
    let mut records = BTreeMap::<(String, String, String), PackageRecord>::new();
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
                    "fixture npm metadata for {}@{} is missing dist.integrity; pass --allow-unverified-registry-artifacts for an explicit compatibility exception",
                    name, version
                ));
            }
            records.insert(
                (p.registry.clone(), p.name.clone(), p.version.clone()),
                PackageRecord {
                    registry,
                    name,
                    version,
                    integrity,
                    artifact: p.artifact.clone(),
                    dependencies: p.dependencies.clone(),
                    fixture: true,
                },
            );
        }
    }
    let mut roots = Vec::new();
    for map in [
        manifest.dependencies(),
        manifest.dev_dependencies(),
        manifest.optional_dependencies(),
    ] {
        for (name, range) in map {
            let (registry, package) = dep_parts(name)?;
            roots.push(Dependency::new(
                registry,
                package,
                range.parse::<Requirement>().map_err(|e| e.to_string())?,
            ));
        }
    }
    let mut wanted = VecDeque::from(roots.clone());
    let mut seen = BTreeSet::new();
    let mut metadata_by_registry = BTreeMap::<String, Vec<PackageVersionMetadata>>::new();
    while let Some(dep) = wanted.pop_front() {
        let key = (dep.registry.to_string(), dep.name.to_string());
        if !seen.insert(key.clone()) {
            continue;
        }
        let packages: Vec<PackageRecord> = if let Some(f) = &fixture {
            f.packages
                .iter()
                .filter(|p| p.registry == key.0 && p.name == key.1)
                .map(|p| records[&(p.registry.clone(), p.name.clone(), p.version.clone())].clone())
                .collect()
        } else {
            remote_records(&dep.registry, &dep.name, allow_missing_integrity)?
        };
        if packages.is_empty() {
            // An npm 404 is an empty candidate set. Leave it absent from the
            // resolver metadata so required packages still fail closed during
            // resolution, while stale historical dependencies do not abort
            // discovery before candidate selection.
            continue;
        }
        let mut versions = Vec::new();
        for p in packages {
            let deps = p
                .dependencies
                .iter()
                .map(|(n, r)| {
                    Ok((
                        n.parse::<PackageName>()
                            .map_err(|e: tapid_core::DomainError| e.to_string())?,
                        r.parse()
                            .map_err(|e: tapid_resolver::ResolveError| e.to_string())?,
                    ))
                })
                .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
            for (name, req) in &deps {
                let (registry, package) = dep_parts(&name.to_string())?;
                wanted.push_back(Dependency::new(registry, package, req.clone()));
            }
            versions.push(PackageVersionMetadata {
                name: p.name.clone(),
                version: p.version,
                dependencies: deps,
            });
        }
        metadata_by_registry
            .entry(dep.registry.to_string())
            .or_default()
            .extend(versions);
    }
    let metadata = metadata_by_registry
        .into_iter()
        .map(|(registry, packages)| {
            RegistryMetadata::normalize(registry.parse().unwrap(), packages)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let resolution = resolve_graph(&roots, &metadata, ResolutionOptions::default())
        .map_err(|e| format!("resolution failed: {e}"))?;
    let mut selected = BTreeMap::new();
    for id in resolution.selected {
        selected.insert((id.registry.to_string(), id.name.to_string()), id);
    }
    let mut lock = Lockfile::new(&root_digest(project)?).map_err(|e| e.to_string())?;
    let empty_peer = tapid_core::PeerContext::default();
    let empty_platform = tapid_core::PlatformContext::new(None, None, None).unwrap();
    let mut packages = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut instances = Vec::new();
    for id in selected.values() {
        let key3 = (
            id.registry.to_string(),
            id.name.to_string(),
            id.version.to_string(),
        );
        let record = if let Some(p) = records.get(&key3) {
            p.clone()
        } else {
            let fetched = remote_records(&id.registry, &id.name, allow_missing_integrity)?;
            let p = fetched
                .into_iter()
                .find(|p| p.version == id.version)
                .ok_or_else(|| format!("missing artifact metadata: {id}"))?;
            records.insert(key3.clone(), p.clone());
            p
        };
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
            let transport = HttpsTransport::standard()
                .map_err(|e| format!("cannot create registry transport: {e}"))?;
            let response = if record.registry.to_string() == JSR {
                JsrRegistry::new(transport, record.registry.clone())
                    .download_artifact(&record.artifact)
            } else {
                NpmRegistry::new(transport, record.registry.clone())
                    .download_artifact(&record.artifact)
            }
            .map_err(|e| format!("cannot download {}: {e}", id))?;
            if response.status != 200 {
                return Err(format!("cannot download {}: HTTP {}", id, response.status));
            }
            response.body
        };
        let actual = integrity(&bytes);
        if record
            .integrity
            .as_ref()
            .is_some_and(|expected| expected != &actual)
        {
            return Err(format!("integrity mismatch for {}", id));
        }
        let archive_digest = digest(&bytes);
        let temp = store.root().join(format!(
            ".online-tree-{}-{}",
            std::process::id(),
            id.version
        ));
        let _ = fs::remove_dir_all(&temp);
        extract_to(
            &bytes,
            ArchiveFormat::TarGz,
            &temp,
            ArchiveLimits::default(),
        )
        .map_err(|e| e.to_string())?;
        let tree_digest: ArtifactDigest = canonical_tree_digest(&temp)
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|e: tapid_core::DomainError| e.to_string())?;
        store
            .ingest_archive(
                &bytes,
                &archive_digest,
                &tree_digest,
                ArchiveFormat::TarGz,
                ArchiveLimits::default(),
            )
            .map_err(|e| e.to_string())?;
        let tree = store
            .verified_tree_path(&tree_digest)
            .map_err(|e| e.to_string())?;
        let key = LockfilePackageKey::new(
            id.registry.clone(),
            id.name.clone(),
            id.version,
            &empty_peer,
            &empty_platform,
        )
        .to_string();
        let mut locked = LockedPackage::new_with_context(
            &id.registry.to_string(),
            &id.name.to_string(),
            &id.version.to_string(),
            &actual.to_string(),
            &tree_digest.to_string(),
            &empty_peer,
            &empty_platform,
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
                id.version,
            ),
            peer_context: empty_peer.clone(),
            platform_context: empty_platform.clone(),
            tree: VerifiedTreeReference::new(&tree_digest.to_string(), &tree)
                .map_err(|e| e.to_string())?,
        });
        let _ = fs::remove_dir_all(temp);
    }
    let locked_packages: Result<Vec<_>, String> = packages
        .values()
        .map(|(locked, record, _)| {
            let mut locked = locked.clone();
            for name in record.dependencies.keys() {
                let (registry, package) = dep_parts(name)?;
                let target = selected
                    .get(&(registry.to_string(), package.to_string()))
                    .ok_or_else(|| format!("missing dependency target {name}"))?;
                let target_key = LockfilePackageKey::new(
                    target.registry.clone(),
                    target.name.clone(),
                    target.version,
                    &empty_peer,
                    &empty_platform,
                )
                .to_string();
                locked
                    .add_dependency(&package.to_string(), &target_key)
                    .map_err(|e| e.to_string())?;
            }
            Ok(locked)
        })
        .collect();
    lock.insert_packages(locked_packages?)
        .map_err(|e| e.to_string())?;
    let mut edge_list = Vec::new();
    let mut root_deps = Vec::new();
    for (_, record, id) in packages.values() {
        let instance = instances
            .iter()
            .find(|i| {
                i.id.name == id.name && i.id.version == id.version && i.id.registry == id.registry
            })
            .unwrap();
        for name in record.dependencies.keys() {
            let (r, n) = dep_parts(name)?;
            let target = selected.get(&(r.to_string(), n.to_string())).unwrap();
            let child = instances
                .iter()
                .find(|i| {
                    i.id.name == target.name
                        && i.id.version == target.version
                        && i.id.registry == target.registry
                })
                .unwrap();
            edge_list.push(DependencyEdge {
                parent: InstanceKey::from(instance),
                child: InstanceKey::from(child),
            });
        }
    }
    for root in roots {
        let id = selected
            .get(&(root.registry.to_string(), root.name.to_string()))
            .ok_or_else(|| format!("missing selected root {}", root.name))?;
        let instance = instances
            .iter()
            .find(|i| {
                i.id.name == id.name && i.id.version == id.version && i.id.registry == id.registry
            })
            .unwrap();
        root_deps.push(InstanceKey::from(instance));
    }
    Ok((
        lock,
        LayoutInput {
            instances,
            root_dependencies: root_deps,
            dependency_edges: edge_list,
        },
        trees,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_registry_prefixes_are_mapped_safely() {
        let (r, n) = dep_parts("jsr:@std/path").unwrap();
        assert_eq!(r.to_string(), JSR);
        assert_eq!(n.to_string(), "@std/path");
        let (r, n) = dep_parts("npm:foo").unwrap();
        assert_eq!(r.to_string(), NPM);
        assert_eq!(n.to_string(), "foo");
    }
}
