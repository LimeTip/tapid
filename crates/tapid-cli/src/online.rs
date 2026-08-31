use base64::{Engine, engine::general_purpose::STANDARD};
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
use tapid_registry_client::{HttpsTransport, JsrRegistry, NpmRegistry, RegistryArtifact};
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
/// Loads a registry fixture from a JSON file and validates that it contains packages.
///
/// # Errors
///
/// Returns an error if the file cannot be read, the JSON is invalid, or the fixture contains no packages.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// let result = fixture(Path::new("missing-registry-fixture.json"));
/// assert!(result.is_err());
/// ```
fn fixture(path: &Path) -> Result<Fixture, String> {
    let f: Fixture = serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid registry fixture: {e}"))?;
    if f.packages.is_empty() {
        return Err("registry fixture contains no packages".into());
    }
    Ok(f)
}

/// Fetches package metadata from a supported registry and converts it into package records.
///
/// # Parameters
///
/// * `allow_missing_integrity` permits npm metadata entries without an integrity value.
///
/// # Returns
///
/// A list of package records derived from the registry metadata, or an error describing
/// an unsupported registry or metadata fetch failure.
///
/// # Examples
///
/// ```no_run
/// # let transport: HttpsTransport = unimplemented!();
/// # let registry: RegistryOrigin = unimplemented!();
/// # let name: PackageName = unimplemented!();
/// let records = remote_records(&transport, &registry, &name, false)?;
/// # Ok::<(), String>(())
/// ```
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
            fixture: false,
        })
        .collect())
}

/// Filters package metadata to versions with parseable dependency names and requirements.
///
/// Versions containing dependency syntax that cannot be parsed are excluded.
///
/// # Examples
///
/// ```
/// let versions = usable_versions(Vec::new());
/// assert!(versions.is_empty());
/// ```
fn usable_versions(packages: Vec<PackageRecord>) -> Vec<PackageVersionMetadata> {
    let mut versions = Vec::new();
    for package in packages {
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

type PackageRecordKey = (String, String, String);
type ResolvedRecords = (Resolution, BTreeMap<PackageRecordKey, PackageRecord>);

/// Resolves dependencies incrementally by fetching metadata only for packages required to continue resolution.
///
/// Metadata is fetched at most once for each registry and package pair. Returns the resolved graph and
/// fetched package records, or an error if resolution or metadata retrieval fails.
///
/// # Examples
///
/// ```rust,ignore
/// let (resolution, records) = resolve_with_fetch(&roots, |registry, name| {
///     fetch_package_records(registry, name)
/// )?;
/// ```
///
/// # Returns
///
/// A resolved dependency graph and the package records fetched during resolution.
fn resolve_with_fetch<F>(roots: &[Dependency], mut fetch: F) -> Result<ResolvedRecords, String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    let mut fetched = BTreeSet::<(String, String)>::new();
    let mut records = BTreeMap::<(String, String, String), PackageRecord>::new();
    let mut metadata_by_registry = BTreeMap::<String, Vec<PackageVersionMetadata>>::new();

    loop {
        let metadata = metadata_by_registry
            .iter()
            .map(|(registry, packages)| {
                let registry = registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                RegistryMetadata::normalize(registry, packages.clone())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;

        match resolve_graph(roots, &metadata, ResolutionOptions::default()) {
            Ok(resolution) => return Ok((resolution, records)),
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
                let registry: RegistryOrigin = registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let name: PackageName = name
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let packages = fetch(&registry, &name)?;
                for package in &packages {
                    records.insert(
                        (
                            package.registry.to_string(),
                            package.name.to_string(),
                            package.version.to_string(),
                        ),
                        package.clone(),
                    );
                }
                metadata_by_registry
                    .entry(registry.to_string())
                    .or_default()
                    .extend(usable_versions(packages));
            }
            Err(error) => return Err(format!("resolution failed: {error}")),
        }
    }
}

/// Resolves project dependencies, fetches and verifies their package artifacts, and builds lockfile and layout data.
///
/// # Examples
///
/// ```no_run
/// # let project = std::path::Path::new(".");
/// # let manifest: PackageManifest = unimplemented!();
/// # let store: Store = unimplemented!();
/// let (lockfile, layout, trees) = resolve_and_fetch(
///     project,
///     &manifest,
///     &store,
///     None,
///     false,
/// )?;
/// # let _: (Lockfile, LayoutInput, std::collections::BTreeMap<String, std::path::PathBuf>) =
/// #     (lockfile, layout, trees);
/// # Ok::<(), String>(())
/// ```
///
/// # Errors
///
/// Returns an error if dependency metadata or artifacts cannot be read, fetched, parsed, verified, extracted, or stored.
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
    let empty_platform = tapid_core::PlatformContext::new(None, None, None).unwrap();
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
    for id in &resolution.selected {
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
            id.version.clone(),
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
                id.version.clone(),
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
        .map(|(locked, _, id)| {
            let mut locked = locked.clone();
            for edge in resolution
                .dependencies
                .iter()
                .filter(|edge| edge.parent == *id)
            {
                let target = &edge.child;
                let target_key = LockfilePackageKey::new(
                    target.registry.clone(),
                    target.name.clone(),
                    target.version.clone(),
                    &empty_peer,
                    &empty_platform,
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
    fn explicit_registry_prefixes_are_mapped_safely() {
        let (r, n) = dep_parts("jsr:@std/path").unwrap();
        assert_eq!(r.to_string(), JSR);
        assert_eq!(n.to_string(), "@std/path");
        let (r, n) = dep_parts("npm:foo").unwrap();
        assert_eq!(r.to_string(), NPM);
        assert_eq!(n.to_string(), "foo");
    }

    /// Creates a package record for `framer-motion` with an optional `popmotion` dependency.
    
    ///
    
    /// # Examples
    
    ///
    
    /// ```
    
    /// let package = record("1.0.0", Some("^2.0.0"));
    
    /// ```
    fn record(version: &str, dependency_requirement: Option<&str>) -> PackageRecord {
        named_record(
            "framer-motion",
            version,
            &dependency_requirement
                .map(|requirement| vec![("popmotion", requirement)])
                .unwrap_or_default(),
        )
    }

    /// Creates an npm package record with the specified version and dependencies.
    ///
    /// # Examples
    ///
    /// ```
    /// let record = named_record("example", "1.0.0", &[]);
    /// assert_eq!(record.name.to_string(), "example");
    /// assert_eq!(record.version.to_string(), "1.0.0");
    /// ```
    ///
    /// `dependencies` contains package names paired with their version requirements.
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
            fixture: false,
        }
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
