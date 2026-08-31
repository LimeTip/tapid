use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
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
use tapid_registry_client::{
    HttpsTransport, JsrRegistry, NpmRegistry, PackagePlatform, RegistryArtifact,
};
use tapid_resolver::{
    Dependency, PackageVersionMetadata, RegistryMetadata, Requirement, Resolution,
    ResolutionOptions, ResolveError, resolve_graph,
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

fn integrity_matches(expected: &PackageIntegrity, data: &[u8]) -> bool {
    let Some(encoded) = expected.as_str().strip_prefix("sha512-") else {
        return false;
    };
    let decoded = if encoded.len() == 86 {
        STANDARD_NO_PAD.decode(encoded)
    } else {
        STANDARD.decode(encoded)
    };
    decoded.is_ok_and(|decoded| decoded.as_slice() == Sha512::digest(data).as_slice())
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
    registry: &RegistryOrigin,
    name: &PackageName,
    allow_missing_integrity: bool,
) -> Result<Vec<PackageRecord>, String> {
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
                let requirement = requirement
                    .parse::<Requirement>()
                    .map_err(|error| error.to_string())?;
                Ok((name, requirement))
            })
            .collect::<Result<BTreeMap<PackageName, Requirement>, String>>();
        if let Ok(dependencies) = dependencies {
            versions.push(PackageVersionMetadata {
                name: package.name,
                version: package.version,
                dependencies,
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

type NormalizedRecords = BTreeMap<PackageRecordKey, Option<NormalizedRecord>>;

fn normalize_record(package: &PackageRecord) -> Option<NormalizedRecord> {
    let metadata = usable_versions(vec![package.clone()]).pop()?;
    let optional_dependencies = package
        .optional_dependencies
        .iter()
        .map(|(name, requirement)| Some((name.parse().ok()?, requirement.parse().ok()?)))
        .collect::<Option<BTreeMap<PackageName, Requirement>>>()?;
    Some(NormalizedRecord {
        metadata,
        optional_dependencies,
    })
}

fn resolver_metadata(
    records: &BTreeMap<PackageRecordKey, PackageRecord>,
    normalized: &NormalizedRecords,
) -> Result<Vec<RegistryMetadata>, String> {
    #[cfg(test)]
    RESOLVER_METADATA_BUILD_COUNT.set(RESOLVER_METADATA_BUILD_COUNT.get() + 1);

    let mut by_registry = BTreeMap::<String, Vec<PackageVersionMetadata>>::new();
    let mut candidates = BTreeMap::<(String, String), Vec<&PackageRecord>>::new();
    for (key, package) in records {
        if normalized.get(key).and_then(Option::as_ref).is_none() {
            continue;
        }
        let (registry, name, _) = key;
        candidates
            .entry((registry.clone(), name.clone()))
            .or_default()
            .push(package);
    }
    for (key, package) in records {
        let Some(normalized) = normalized.get(key).and_then(Option::as_ref) else {
            continue;
        };
        let mut metadata = normalized.metadata.clone();
        let registry = package.registry.to_string();
        for (name, requirement) in &normalized.optional_dependencies {
            let name_text = name.to_string();
            let compatible_candidate_exists = candidates
                .get(&(registry.clone(), name_text))
                .is_some_and(|versions| {
                    versions.iter().any(|candidate| {
                        current_platform_matches(&candidate.platform)
                            && requirement.matches(&candidate.version)
                    })
                });
            if compatible_candidate_exists {
                metadata
                    .dependencies
                    .insert(name.clone(), requirement.clone());
            }
        }
        by_registry.entry(registry).or_default().push(metadata);
    }
    by_registry
        .into_iter()
        .map(|(registry, packages)| {
            let registry = registry
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?;
            RegistryMetadata::normalize(registry, packages).map_err(|error| error.to_string())
        })
        .collect()
}

type PackageRecordKey = (String, String, String);
type ResolvedRecords = (Resolution, BTreeMap<PackageRecordKey, PackageRecord>);

#[cfg(test)]
thread_local! {
    static RESOLVER_METADATA_BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn insert_record(
    records: &mut BTreeMap<PackageRecordKey, PackageRecord>,
    normalized: &mut NormalizedRecords,
    package: PackageRecord,
) {
    let key = (
        package.registry.to_string(),
        package.name.to_string(),
        package.version.to_string(),
    );
    normalized.insert(key.clone(), normalize_record(&package));
    records.insert(key, package);
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
fn resolve_with_fetch<F>(roots: &[Dependency], mut fetch: F) -> Result<ResolvedRecords, String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    let mut fetched = BTreeSet::<(String, String)>::new();
    let mut records = BTreeMap::<PackageRecordKey, PackageRecord>::new();
    let mut normalized = NormalizedRecords::new();

    loop {
        let metadata = resolver_metadata(&records, &normalized)?;

        match resolve_graph(roots, &metadata, ResolutionOptions::default()) {
            Ok(resolution) => {
                let optional_frontier = resolution
                    .selected
                    .iter()
                    .filter_map(|parent| {
                        records
                            .get(&(
                                parent.registry.to_string(),
                                parent.name.to_string(),
                                parent.version.to_string(),
                            ))
                            .map(|record| (parent, record))
                    })
                    .flat_map(|(parent, record)| {
                        record.optional_dependencies.keys().filter_map(|name| {
                            let key = (parent.registry.to_string(), name.clone());
                            (!fetched.contains(&key))
                                .then_some((parent.registry.clone(), name.clone()))
                        })
                    })
                    .collect::<BTreeSet<_>>();
                if !optional_frontier.is_empty() {
                    for (registry, name) in optional_frontier {
                        let name: PackageName = name
                            .parse()
                            .map_err(|error: tapid_core::DomainError| error.to_string())?;
                        fetched.insert((registry.to_string(), name.to_string()));
                        report_metadata_progress(fetched.len());
                        for package in fetch(&registry, &name)? {
                            insert_record(&mut records, &mut normalized, package);
                        }
                    }
                    continue;
                }
                return Ok((resolution, records));
            }
            Err(ResolveError::MissingMetadata { packages }) => {
                for (registry, name) in packages {
                    let key = (registry.clone(), name.clone());
                    if !fetched.insert(key) {
                        return Err(format!(
                            "resolution failed: metadata for {registry}:{name} remains unavailable"
                        ));
                    }
                    report_metadata_progress(fetched.len());
                    let registry: RegistryOrigin = registry
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?;
                    let name: PackageName = name
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?;
                    for package in fetch(&registry, &name)? {
                        insert_record(&mut records, &mut normalized, package);
                    }
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
                    return Err(format!("resolution failed: {error}"));
                }
                report_metadata_progress(fetched.len());
                let registry: RegistryOrigin = registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let name: PackageName = name
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let packages = fetch(&registry, &name)?;
                for package in packages {
                    insert_record(&mut records, &mut normalized, package);
                }
            }
            Err(error) => return Err(format!("resolution failed: {error}")),
        }
    }
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
                    "fixture npm metadata for {}@{} is missing dist.integrity; pass --allow-unverified-registry-artifacts for an explicit compatibility exception",
                    name, version
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
                    optional_dependencies: BTreeMap::new(),
                    platform: PackagePlatform::unrestricted(),
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
    let metadata_transport = if fixture.is_none() {
        Some(
            HttpsTransport::standard()
                .map_err(|error| format!("cannot create registry transport: {error}"))?,
        )
    } else {
        None
    };
    let (resolution, mut records) = resolve_with_fetch(&roots, |registry, name| {
        if fixture.is_some() {
            Ok(fixture_records
                .values()
                .filter(|package| &package.registry == registry && &package.name == name)
                .cloned()
                .collect())
        } else {
            remote_records(
                metadata_transport
                    .as_ref()
                    .expect("remote metadata transport"),
                registry,
                name,
                allow_missing_integrity,
            )
        }
    })?;
    let mut lock = Lockfile::new(&root_digest(project)?).map_err(|e| e.to_string())?;
    let empty_peer = tapid_core::PeerContext::default();
    let mut platform_contexts = BTreeMap::new();
    let mut packages = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut instances = Vec::new();
    let artifact_transport = if fixture.is_none() {
        Some(
            HttpsTransport::standard_artifact()
                .map_err(|error| format!("cannot create registry transport: {error}"))?,
        )
    } else {
        None
    };
    let artifact_total = resolution.selected.len();
    for (index, id) in resolution.selected.iter().enumerate() {
        let key3 = (
            id.registry.to_string(),
            id.name.to_string(),
            id.version.to_string(),
        );
        let record = if let Some(p) = records.get(&key3) {
            p.clone()
        } else {
            let fetched = remote_records(
                metadata_transport
                    .as_ref()
                    .expect("remote metadata transport"),
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
            let transport = artifact_transport
                .as_ref()
                .expect("remote artifact transport");
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
            .is_some_and(|expected| !integrity_matches(expected, &bytes))
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
            id.version.clone(),
            &empty_peer,
            &platform_context,
        )
        .to_string();
        let mut locked = LockedPackage::new_with_context(
            &id.registry.to_string(),
            &id.name.to_string(),
            &id.version.to_string(),
            &actual.to_string(),
            &tree_digest.to_string(),
            &empty_peer,
            &platform_context,
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
            peer_context: empty_peer.clone(),
            platform_context,
            tree: VerifiedTreeReference::new(&tree_digest.to_string(), &tree)
                .map_err(|e| e.to_string())?,
        });
        let _ = fs::remove_dir_all(temp);
        let completed = index + 1;
        if artifact_progress_checkpoint(completed, artifact_total) {
            eprintln!("Artifact verification progress: {completed}/{artifact_total}");
        }
    }
    let locked_packages: Result<Vec<_>, String> = packages
        .values()
        .map(|(locked, _, id)| {
            let mut locked = locked.clone();
            for edge in resolution
                .dependencies
                .iter()
                .filter(|edge| edge.parent == *id)
            {
                let target = &edge.child;
                let target_platform = platform_contexts
                    .get(target)
                    .ok_or_else(|| format!("missing platform context for {target}"))?;
                let target_key = LockfilePackageKey::new(
                    target.registry.clone(),
                    target.name.clone(),
                    target.version.clone(),
                    &empty_peer,
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
    lock.insert_packages(locked_packages?)
        .map_err(|e| e.to_string())?;
    lock.set_roots(resolution.roots.iter().map(|id| {
        let platform = platform_contexts
            .get(id)
            .expect("selected root platform context");
        LockfilePackageKey::new(
            id.registry.clone(),
            id.name.clone(),
            id.version.clone(),
            &empty_peer,
            platform,
        )
        .to_string()
    }))
    .map_err(|e| e.to_string())?;
    let mut edge_list = Vec::new();
    let mut root_deps = Vec::new();
    for edge in &resolution.dependencies {
        let parent = instances
            .iter()
            .find(|i| {
                i.id.name == edge.parent.name
                    && i.id.version == edge.parent.version
                    && i.id.registry == edge.parent.registry
            })
            .unwrap();
        let child = instances
            .iter()
            .find(|i| {
                i.id.name == edge.child.name
                    && i.id.version == edge.child.version
                    && i.id.registry == edge.child.registry
            })
            .unwrap();
        edge_list.push(DependencyEdge {
            parent: InstanceKey::from(parent),
            child: InstanceKey::from(child),
        });
    }
    for id in &resolution.roots {
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
    fn artifact_progress_is_emitted_at_bounded_completion_checkpoints() {
        let checkpoints = (1..=625)
            .filter(|completed| artifact_progress_checkpoint(*completed, 625))
            .collect::<Vec<_>>();

        assert_eq!(checkpoints.first(), Some(&1));
        assert_eq!(checkpoints.last(), Some(&625));
        assert!(checkpoints.len() <= 14);
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
    fn metadata_progress_is_emitted_at_bounded_checkpoints() {
        let checkpoints = (1..=612)
            .filter(|fetches| metadata_progress_checkpoint(*fetches))
            .collect::<Vec<_>>();

        assert_eq!(checkpoints.first(), Some(&1));
        assert_eq!(checkpoints.last(), Some(&600));
        assert!(checkpoints.len() <= 13);
    }

    #[test]
    fn padded_and_unpadded_sha512_sri_verify_against_the_same_digest() {
        let bytes = b"archive bytes";
        let padded = integrity(bytes);
        let unpadded: PackageIntegrity = padded.to_string().trim_end_matches('=').parse().unwrap();

        assert!(integrity_matches(&padded, bytes));
        assert!(integrity_matches(&unpadded, bytes));
        assert!(!integrity_matches(&padded, b"different bytes"));
        assert!(!integrity_matches(&unpadded, b"different bytes"));
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
            optional_dependencies: BTreeMap::new(),
            platform: PackagePlatform::unrestricted(),
            fixture: false,
        }
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
    fn empty_historical_dependency_ranges_do_not_hide_usable_versions() {
        let versions = usable_versions(vec![record("3.0.1", Some("")), record("4.0.5", None)]);

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "4.0.5");
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
