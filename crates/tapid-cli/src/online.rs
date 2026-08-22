use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::{collections::{BTreeMap, BTreeSet, VecDeque}, fs, path::{Path, PathBuf}};
use tapid_archive::{canonical_tree_digest, extract_to, ArchiveFormat, ArchiveLimits};
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName, PackageVersion, RegistryOrigin};
use tapid_linker::{DependencyEdge, InstanceKey, LayoutInput, PackageInstance, VerifiedTreeReference};
use tapid_lockfile::{LockedPackage, Lockfile, LockfilePackageKey};
use tapid_manifest::PackageManifest;
use tapid_registry_client::{HttpResponse, HttpTransport, NpmRegistry};
use tapid_resolver::{resolve_graph, Dependency, PackageVersionMetadata, RegistryMetadata, Requirement, ResolutionOptions};
use tapid_store::Store;

const NPM: &str = "https://registry.npmjs.org";
const JSR: &str = "https://jsr.io";

#[derive(Debug, Deserialize, Clone)]
struct FixturePackage {
    registry: String,
    name: String,
    version: String,
    #[serde(default)] integrity: Option<String>,
    artifact: String,
    #[serde(default)] dependencies: BTreeMap<String, String>,
}
#[derive(Debug, Deserialize)]
struct Fixture { packages: Vec<FixturePackage> }

struct FixtureTransport { packages: BTreeMap<(String, String, String), FixturePackage> }
impl HttpTransport for FixtureTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, tapid_registry_client::TransportError> {
        let item = self.packages.values().find(|p| {
            url.contains(&format!("/{}/", p.name.trim_start_matches('@'))) || url.ends_with(&format!("/{}", p.name))
        }).ok_or_else(|| tapid_registry_client::TransportError::Http(format!("fixture has no response for {url}")))?;
        let bytes = fs::read(&item.artifact).map_err(|e| tapid_registry_client::TransportError::Http(e.to_string()))?;
        Ok(HttpResponse { status: 200, content_type: Some("application/octet-stream".into()), body: bytes })
    }
}

fn digest(data: &[u8]) -> ArtifactDigest {
    let mut h = Sha256::new(); h.update(data);
    format!("sha256-{}", hex::encode(h.finalize())).parse().expect("sha256 digest")
}
fn integrity(data: &[u8]) -> PackageIntegrity {
    let mut h = Sha512::new(); h.update(data);
    format!("sha512-{}", STANDARD.encode(h.finalize())).parse().expect("sha512 integrity")
}
fn root_digest(project: &Path) -> Result<String, String> {
    let data = fs::read(project.join("package.json")).map_err(|e| e.to_string())?;
    Ok(digest(&data).to_string())
}
fn dep_parts(name: &str) -> Result<(RegistryOrigin, PackageName), String> {
    let (origin, raw) = if let Some(v) = name.strip_prefix("jsr:") { (JSR, v) }
        else if let Some(v) = name.strip_prefix("npm:") { (NPM, v) }
        else { (NPM, name) };
    Ok((origin.parse().map_err(|e: tapid_core::DomainError| e.to_string())?, raw.parse().map_err(|e: tapid_core::DomainError| e.to_string())?))
}
fn fixture(path: &Path) -> Result<Fixture, String> {
    let f: Fixture = serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| format!("invalid registry fixture: {e}"))?;
    if f.packages.is_empty() { return Err("registry fixture contains no packages".into()); }
    Ok(f)
}

pub fn resolve_and_fetch(
    project: &Path,
    manifest: &PackageManifest,
    store: &Store,
    fixture_path: Option<&Path>,
) -> Result<(Lockfile, LayoutInput, BTreeMap<String, PathBuf>), String> {
    let fixture = fixture_path.map(fixture).transpose()?;
    fs::create_dir_all(store.root()).map_err(|e| format!("cannot create store: {e}"))?;
    let mut records = BTreeMap::<(String, String, String), FixturePackage>::new();
    if let Some(f) = &fixture {
        for p in &f.packages {
            let _: RegistryOrigin = p.registry.parse().map_err(|_| format!("invalid fixture registry {}", p.registry))?;
            let _: PackageName = p.name.parse().map_err(|_| format!("invalid fixture package {}", p.name))?;
            let _: PackageVersion = p.version.parse().map_err(|_| format!("invalid fixture version {}", p.version))?;
            records.insert((p.registry.clone(), p.name.clone(), p.version.clone()), p.clone());
        }
    }
    let mut roots = Vec::new();
    for map in [manifest.dependencies(), manifest.dev_dependencies(), manifest.optional_dependencies()] {
        for (name, range) in map { let (registry, package) = dep_parts(name)?; roots.push(Dependency::new(registry, package, range.parse::<Requirement>().map_err(|e| e.to_string())?)); }
    }
    let mut wanted = VecDeque::from(roots.clone());
    let mut seen = BTreeSet::new();
    let mut metadata_by_registry = BTreeMap::<String, Vec<PackageVersionMetadata>>::new();
    while let Some(dep) = wanted.pop_front() {
        let key = (dep.registry.to_string(), dep.name.to_string());
        if !seen.insert(key.clone()) { continue; }
        let packages = if let Some(f) = &fixture {
            f.packages.iter().filter(|p| p.registry == key.0 && p.name == key.1).cloned().collect::<Vec<_>>()
        } else { fetch_remote(&dep.registry, &dep.name)? };
        if packages.is_empty() { return Err(format!("missing dependency metadata: {}:{}", dep.registry, dep.name)); }
        let mut versions = Vec::new();
        for p in packages {
            let version: PackageVersion = p.version.parse().map_err(|e: tapid_core::DomainError| e.to_string())?;
            let deps = p.dependencies.iter().map(|(n,r)| Ok((n.parse::<PackageName>().map_err(|e: tapid_core::DomainError| e.to_string())?, r.parse().map_err(|e: tapid_resolver::ResolveError| e.to_string())?))).collect::<Result<BTreeMap<PackageName, Requirement>,String>>()?;
            for (name, req) in &deps { let (registry, package) = dep_parts(&name.to_string())?; wanted.push_back(Dependency::new(registry, package, req.clone())); }
            versions.push(PackageVersionMetadata { name: dep.name.clone(), version, dependencies: deps.into_iter().map(|(n,r)| (n,r)).collect() });
        }
        metadata_by_registry.entry(dep.registry.to_string()).or_default().extend(versions);
    }
    let metadata = metadata_by_registry.into_iter().map(|(registry, packages)| RegistryMetadata::normalize(registry.parse().unwrap(), packages).map_err(|e| e.to_string())).collect::<Result<Vec<_>,_>>()?;
    let resolution = resolve_graph(&roots, &metadata, ResolutionOptions::default()).map_err(|e| format!("resolution failed: {e}"))?;
    let mut selected = BTreeMap::new();
    for id in resolution.selected { selected.insert((id.registry.to_string(), id.name.to_string()), id); }
    let mut lock = Lockfile::new(&root_digest(project)?).map_err(|e| e.to_string())?;
    let empty_peer = tapid_core::PeerContext::default();
    let empty_platform = tapid_core::PlatformContext::new(None,None,None).unwrap();
    let mut packages = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut instances = Vec::new();
    for id in selected.values() {
        let record = if let Some(p) = records.get(&(id.registry.to_string(), id.name.to_string(), id.version.to_string())) { p.clone() }
            else { return Err(format!("missing artifact metadata: {id}")); };
        let bytes = if let Some(encoded) = record.artifact.strip_prefix("base64:") { STANDARD.decode(encoded).map_err(|e| format!("invalid artifact encoding: {e}"))? } else { fs::read(&record.artifact).map_err(|e| format!("cannot read artifact {}: {e}", record.artifact))? };
        let expected_integrity = record.integrity.clone().map(|value| value.parse::<PackageIntegrity>().map_err(|e| format!("invalid integrity for {}: {e}", id)).and_then(|value| { if value != integrity(&bytes) { Err(format!("integrity mismatch for {}", id)) } else { Ok(value.to_string()) } })).transpose()?;
        let archive_digest = digest(&bytes);
        let temp = store.root().join(format!(".online-tree-{}-{}", std::process::id(), id.version));
        let _ = fs::remove_dir_all(&temp);
        extract_to(&bytes, ArchiveFormat::TarGz, &temp, ArchiveLimits::default()).map_err(|e| e.to_string())?;
        let tree_digest: ArtifactDigest = canonical_tree_digest(&temp).map_err(|e| e.to_string())?.parse().map_err(|e: tapid_core::DomainError| e.to_string())?;
        let got = store.ingest_archive(&bytes, &archive_digest, &tree_digest, ArchiveFormat::TarGz, ArchiveLimits::default()).map_err(|e| e.to_string())?;
        let tree = store.verified_tree_path(&tree_digest).map_err(|e| e.to_string())?;
        let key = tapid_lockfile::LockfilePackageKey::new(id.registry.clone(), id.name.clone(), id.version, &empty_peer, &empty_platform).to_string();
        let mut locked = LockedPackage::new_with_context(&id.registry.to_string(), &id.name.to_string(), &id.version.to_string(), &expected_integrity.unwrap_or_else(|| integrity(&bytes).to_string()), &tree_digest.to_string(), &empty_peer, &empty_platform).map_err(|e| e.to_string())?;
        if record.artifact.starts_with("https://") { locked.set_artifact_url(&record.artifact).map_err(|e| e.to_string())?; }
        packages.insert(key.clone(), (locked, record, id.clone(), tree.clone()));
        trees.insert(key, tree.clone());
        let _ = got;
        let instance = PackageInstance { id: tapid_core::PackageInstanceId::new(id.registry.clone(), id.name.clone(), id.version), peer_context: empty_peer.clone(), platform_context: empty_platform.clone(), tree: VerifiedTreeReference::new(&tree_digest.to_string(), &tree).map_err(|e| e.to_string())? };
        instances.push(instance);
        let _ = fs::remove_dir_all(temp);
    }
    let _keys: BTreeMap<_,_> = packages.iter().map(|(k,(_,_,id,_))| (k.clone(), LockfilePackageKey::new(id.registry.clone(), id.name.clone(), id.version, &empty_peer, &empty_platform).to_string())).collect();
    let mut pending: Vec<_> = packages.keys().cloned().collect();
    while !pending.is_empty() {
        let mut progressed = false;
        let mut next = Vec::new();
        for key in pending {
            let (mut locked, record, _, _) = packages.get(&key).cloned().unwrap();
            let mut ok = true;
            for (name, _) in &record.dependencies { let (registry, package) = dep_parts(name)?; let target = selected.get(&(registry.to_string(), package.to_string())).ok_or_else(|| format!("missing dependency target {name}"))?; let target_key = LockfilePackageKey::new(target.registry.clone(), target.name.clone(), target.version, &empty_peer, &empty_platform).to_string(); if !packages.contains_key(&target_key) { ok=false; break; } locked.add_dependency(&package.to_string(), &target_key).map_err(|e| e.to_string())?; }
            if !ok { next.push(key); continue; }
            lock.insert_package(locked).map_err(|e| e.to_string())?; progressed=true;
        }
        if !progressed { return Err("dependency graph contains an unmaterializable cycle".into()); }
        pending=next;
    }
    let mut edge_list = Vec::new();
    let mut root_deps = Vec::new();
    let mut by_id = BTreeMap::new();
    for (key, (_, record, id, _)) in &packages { let instance = instances.iter().find(|i| i.id.name == id.name && i.id.version == id.version && i.id.registry == id.registry).unwrap(); by_id.insert((id.registry.to_string(), id.name.to_string()), InstanceKey::from(instance)); for name in record.dependencies.keys() { let (r,n)=dep_parts(name)?; let target=selected.get(&(r.to_string(),n.to_string())).unwrap(); let child=instances.iter().find(|i| i.id.name==target.name && i.id.version==target.version && i.id.registry==target.registry).unwrap(); edge_list.push(DependencyEdge{parent:InstanceKey::from(instance),child:InstanceKey::from(child)}); } let _=key; }
    for root in roots { let id=selected.get(&(root.registry.to_string(),root.name.to_string())).ok_or_else(|| format!("missing selected root {}",root.name))?; let instance=instances.iter().find(|i| i.id.name==id.name && i.id.version==id.version && i.id.registry==id.registry).unwrap(); root_deps.push(InstanceKey::from(instance)); }
    Ok((lock, LayoutInput { instances, root_dependencies: root_deps, dependency_edges: edge_list }, trees))
}

fn fetch_remote(registry: &RegistryOrigin, name: &PackageName) -> Result<Vec<FixturePackage>, String> {
    let _ = (registry, name);
    Err("online install unavailable: tapid-registry-client does not expose dependency maps for NpmRegistry::fetch/JsrRegistry::fetch; JsrRegistry also does not expose artifact integrity".into())
}

#[allow(dead_code)]
fn _client_types_are_linked<T: HttpTransport>(transport: T, origin: RegistryOrigin, name: &str) {
    let _ = NpmRegistry::new(transport, origin).fetch(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_install_fails_closed_when_registry_contract_lacks_dependencies() {
        let registry: RegistryOrigin = NPM.parse().unwrap();
        let name: PackageName = "example".parse().unwrap();
        assert_eq!(
            fetch_remote(&registry, &name).unwrap_err(),
            "online install unavailable: tapid-registry-client does not expose dependency maps for NpmRegistry::fetch/JsrRegistry::fetch; JsrRegistry also does not expose artifact integrity"
        );
    }
}
