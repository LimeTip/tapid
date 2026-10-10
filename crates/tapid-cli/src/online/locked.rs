//! Verification and hydration of the graph already selected by a lockfile.
use super::*;

pub(crate) fn validate_locked_routes(
    lock: &Lockfile,
    config: &crate::registry::RegistryConfig,
) -> Result<(), OperationalError> {
    for (key, _) in lock.packages_typed()? {
        let registry = key.source.registry().ok_or("expected registry identity")?;
        if registry.as_str() != JSR && config.origin_for_name(&key.name)? != *registry {
            return Err(OperationalError::new(
                ErrorKind::RegistryConfiguration,
                format!("registry identity mismatch for locked package {}", key.name),
            ));
        }
        let context = crate::context::parse_platform(&key.platform_context)?;
        let constraints = PackagePlatform {
            os: context.os.into_iter().collect(),
            cpu: context.cpu.into_iter().collect(),
            libc: context.libc.into_iter().collect(),
        };
        if !current_platform_matches(&constraints) {
            return Err(OperationalError::new(
                ErrorKind::Lockfile,
                format!(
                    "locked package {} targets a different platform; regenerate the lock for this target",
                    key.name
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn hydrate_locked(
    lock: &Lockfile,
    store: &Store,
    config: &crate::registry::RegistryConfig,
    fixture_path: Option<&Path>,
) -> Result<Option<StoreTransaction>, OperationalError> {
    let mut transports = BTreeMap::new();
    let allowed_origins = config.configured_origins();
    // Load fixtures only if an archive is actually missing.
    let mut fixture_packages = None;
    hydrate_with_fetch(lock, store, |key, url| {
        let registry = key.source.registry().ok_or("expected registry identity")?;
        if let Some(path) = fixture_path {
            if fixture_packages.is_none() {
                fixture_packages = Some(fixture(path)?.packages);
            }
            let record = fixture_packages
                .as_ref()
                .unwrap()
                .iter()
                .find(|p| {
                    p.registry == registry.as_str()
                        && p.name == key.name.as_str()
                        && p.version == key.version.to_string()
                })
                .ok_or_else(|| {
                    OperationalError::new(
                        ErrorKind::RegistryMetadata,
                        "fixture has no exact locked artifact",
                    )
                })?;
            if let Some(encoded) = record.artifact.strip_prefix("base64:") {
                STANDARD.decode(encoded).map_err(|error| {
                    OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                })
            } else {
                fs::read(&record.artifact).map_err(|error| {
                    OperationalError::from_source(ErrorKind::RegistryTransport, error)
                })
            }
        } else {
            let transport = artifact_transport_for_package(
                &mut transports,
                config,
                registry,
                &key.name,
                &allowed_origins,
            )?;
            let response = if registry.as_str() == JSR {
                JsrRegistry::new(transport, registry.clone()).download_artifact(url)
            } else {
                NpmRegistry::new(transport, registry.clone()).download_artifact(url)
            }
            .map_err(|error| OperationalError::from_source(ErrorKind::RegistryTransport, error))?;
            if response.status != 200 {
                return Err(OperationalError::new(
                    ErrorKind::RegistryTransport,
                    format!("cannot download locked artifact: HTTP {}", response.status),
                ));
            }
            Ok(response.body)
        }
    })
}

fn hydrate_with_fetch(
    lock: &Lockfile,
    store: &Store,
    mut fetch: impl FnMut(&LockfilePackageKey, &str) -> Result<Vec<u8>, OperationalError>,
) -> Result<Option<StoreTransaction>, OperationalError> {
    let mut transaction = store.transaction();
    let mut staged = BTreeSet::new();
    for (key, package) in lock.packages_typed()? {
        let digest: ArtifactDigest = package
            .tree_digest()
            .parse()
            .map_err(|error: tapid_core::DomainError| error.to_string())?;
        match store.verified_tree_path(&digest) {
            Ok(_) => continue,
            Err(tapid_store::IngestError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
                    && !store
                        .root()
                        .join("trees")
                        .join(digest.as_str())
                        .try_exists()
                        .map_err(|error| {
                            OperationalError::from_source(ErrorKind::Store, error)
                        })? => {}
            Err(error) => {
                return Err(OperationalError::from(error).context("locked store tree is invalid"));
            }
        }
        if package.registry_integrity_declared() != Some(true) {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                "locked artifact lacks registry-declared integrity provenance",
            ));
        }
        let url = package.artifact_url().ok_or_else(|| {
            OperationalError::new(
                ErrorKind::Lockfile,
                "locked artifact has no pinned archive URL",
            )
        })?;
        let bytes = fetch(&key, url)?;
        let expected: PackageIntegrity = package
            .artifact_integrity()
            .parse()
            .map_err(|error: tapid_core::DomainError| error.to_string())?;
        if !integrity_matches(&expected, &integrity(&bytes)) {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                format!("integrity mismatch for locked package {}", key.name),
            ));
        }
        fs::create_dir_all(store.root())
            .map_err(|error| OperationalError::from_source(ErrorKind::Store, error))?;
        let temp_id = NEXT_TEMP_TREE_ID.fetch_add(1, Ordering::Relaxed);
        let temp = store.root().join(format!(
            ".online-tree-{}-{temp_id}-locked",
            std::process::id()
        ));
        let _guard = TemporaryTree(temp.clone());
        extract_to(
            &bytes,
            ArchiveFormat::TarGz,
            &temp,
            ArchiveLimits::default(),
        )
        .map_err(|error| OperationalError::from_source(ErrorKind::Archive, error))?;
        let actual = canonical_tree_digest(&temp)
            .map_err(|error| OperationalError::from_source(ErrorKind::Archive, error))?;
        if actual != digest.as_str() {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                format!("locked tree digest mismatch for {}", key.name),
            ));
        }
        if staged.insert(digest.to_string()) {
            transaction
                .stage_verified_tree(&digest, &temp)
                .map_err(OperationalError::from)?;
        }
    }
    Ok((!staged.is_empty()).then_some(transaction))
}

pub(super) fn locked_records(
    lock: &Lockfile,
    fixture_records: &BTreeMap<PackageRecordKey, PackageRecord>,
    using_fixture: bool,
) -> Result<Vec<PackageRecord>, OperationalError> {
    let mut identities = BTreeSet::new();
    lock.packages_typed()?
        .into_iter()
        .filter(|(_, package)| package.registry_integrity_declared() != Some(false))
        .map(|(key, package)| {
            let registry = key
                .source
                .registry()
                .cloned()
                .ok_or("expected registry identity")?;
            if !identities.insert((registry.clone(), key.name.clone(), key.version.clone())) {
                return Err(OperationalError::new(ErrorKind::Lockfile, format!(
                    "multiple locked contexts for {}@{} cannot be reused during changed-graph resolution; request an explicit update",
                    key.name, key.version)));
            }
            let platform = crate::context::parse_platform(&key.platform_context)?;
            let dependencies = package
                .dependencies()
                .iter()
                .map(|(name, target)| {
                    let target: LockfilePackageKey = target.parse()?;
                    let requirement = if name == target.name.as_str() {
                        target.version.to_string()
                    } else {
                        format!("npm:{}@{}", target.name, target.version)
                    };
                    Ok((name.clone(), requirement))
                })
                .collect::<Result<BTreeMap<_, _>, OperationalError>>()?;
            let peers = crate::context::parse_peer(&key.peer_context)?
                .entries()
                .iter()
                .map(|(name, version)| (name.to_string(), version.to_string()))
                .collect();
            let fixture_key = (
                registry.to_string(),
                key.name.to_string(),
                key.version.to_string(),
            );
            Ok(PackageRecord {
                registry,
                name: key.name,
                version: key.version,
                integrity: Some(
                    package
                        .artifact_integrity()
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?,
                ),
                artifact: if using_fixture {
                    fixture_records
                        .get(&fixture_key)
                        .map(|record| record.artifact.clone())
                        .unwrap_or_default()
                } else {
                    package.artifact_url().unwrap_or_default().to_owned()
                },
                dependencies,
                peer_dependencies: peers,
                optional_peer_dependencies: BTreeSet::new(),
                optional_dependencies: BTreeMap::new(),
                platform: PackagePlatform {
                    os: platform.os.into_iter().collect(),
                    cpu: platform.cpu.into_iter().collect(),
                    libc: platform.libc.into_iter().collect(),
                },
                fixture: using_fixture,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_graph_seeding_rejects_multiple_contexts_for_one_exact_version() {
        let digest = format!("sha256-{}", "0".repeat(64));
        let integrity = format!("sha512-{}", STANDARD.encode([0; 64]));
        let mut lock = Lockfile::new(&digest).unwrap();
        let empty = tapid_core::PeerContext::default();
        let bound = empty
            .clone()
            .with("react".parse().unwrap(), "18.2.0".parse().unwrap());
        for peer in [&empty, &bound] {
            lock.insert_package(
                LockedPackage::new_with_context_and_provenance(
                    NPM,
                    "plugin",
                    "1.0.0",
                    &integrity,
                    &digest,
                    (
                        peer,
                        &tapid_core::PlatformContext::new(None, None, None).unwrap(),
                    ),
                    RegistryIntegrityProvenance::RegistryDeclared,
                )
                .unwrap(),
            )
            .unwrap();
        }
        let failure = locked_records(&lock, &BTreeMap::new(), false)
            .err()
            .expect("ambiguous context reuse must fail");
        assert_eq!(failure.kind, ErrorKind::Lockfile);
    }
}

fn matches_locked_identity(record: &FixturePackage, key: &LockfilePackageKey) -> bool {
    key.source
        .registry()
        .is_some_and(|registry| record.registry == registry.as_str())
        && record.name == key.name.as_str()
        && record.version == key.version.to_string()
}

pub(crate) fn validate_locked_artifact_sources(
    lock: &Lockfile,
    registry_fixture: Option<&Path>,
) -> Result<(), OperationalError> {
    let packages = lock.packages_typed().map_err(OperationalError::from)?;
    let missing_urls = packages
        .iter()
        .filter(|(_, package)| package.artifact_url().is_none())
        .collect::<Vec<_>>();
    if missing_urls.is_empty() {
        return Ok(());
    }
    let path = registry_fixture.ok_or_else(|| OperationalError::new(
        ErrorKind::Lockfile,
        "ci requires download URLs for every locked registry package, including with --offline; regenerate tapid.lock with tapid install and review the resulting changes",
    ))?;
    let fixture =
        fixture(path).map_err(|error| OperationalError::new(ErrorKind::RegistryMetadata, error))?;
    for (key, _) in missing_urls {
        if !fixture
            .packages
            .iter()
            .any(|record| matches_locked_identity(record, key))
        {
            return Err(OperationalError::new(
                ErrorKind::RegistryMetadata,
                "fixture lacks exact locked artifact for a package without a download URL",
            ));
        }
    }
    Ok(())
}

type LockedInstallOutput = Result<
    (
        NamedLayoutInput,
        BTreeMap<String, PathBuf>,
        StoreTransaction,
        LockedSnapshots,
    ),
    OperationalError,
>;

/// Retain private verified snapshots through activation without republishing cached trees.
pub(crate) struct LockedSnapshots {
    trees: Vec<TemporaryTree>,
}

pub(crate) fn prepare_locked_install(
    lock: &Lockfile,
    manifest: &PackageManifest,
    store: &Store,
    registry_config: &crate::registry::RegistryConfig,
    registry_fixture: Option<&Path>,
    report_progress: impl FnMut(usize, usize),
) -> LockedInstallOutput {
    store.cleanup_stale_replay_snapshots().map_err(|error| {
        OperationalError::from(error).context("cannot recover stale replay snapshots")
    })?;
    let mut transaction = store.transaction();
    let mut snapshots = LockedSnapshots { trees: Vec::new() };
    let mut fixtures = None;
    let mut transports = BTreeMap::new();
    let allowed_origins = registry_config.configured_origins();
    let (input, trees) = crate::application::replay::replay_input_with_tree_source(
        lock,
        manifest,
        registry_config,
        report_progress,
        false,
        |key, package| {
            let tree_digest: ArtifactDigest =
                package
                    .tree_digest()
                    .parse()
                    .map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::Lockfile, error)
                    })?;
            match store.verified_tree_snapshot(&tree_digest) {
                Ok(snapshot) => {
                    snapshots.trees.push(TemporaryTree(snapshot.clone()));
                    return Ok(snapshot);
                }
                Err(tapid_store::IngestError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(OperationalError::from(error)),
            }
            let registry = key.source.registry().ok_or_else(|| {
                OperationalError::new(
                    ErrorKind::Lockfile,
                    "registry package has no registry origin",
                )
            })?;
            // Identity checks do not retrieve credentials or perform network work.
            if registry.as_str() != JSR
                && registry_config
                    .origin_for_name(&key.name)
                    .map_err(|error| {
                        OperationalError::new(ErrorKind::RegistryConfiguration, error)
                    })?
                    != *registry
            {
                return Err(OperationalError::new(
                    ErrorKind::RegistryConfiguration,
                    "locked registry identity differs from configured route",
                ));
            }
            let bytes = if let Some(path) = registry_fixture {
                if fixtures.is_none() {
                    fixtures = Some(fixture(path).map_err(|error| {
                        OperationalError::new(ErrorKind::RegistryMetadata, error)
                    })?);
                }
                let record = fixtures
                    .as_ref()
                    .expect("fixture loaded")
                    .packages
                    .iter()
                    .find(|record| matches_locked_identity(record, key))
                    .ok_or_else(|| {
                        OperationalError::new(
                            ErrorKind::RegistryMetadata,
                            "fixture lacks exact locked artifact",
                        )
                    })?;
                if let Some(encoded) = record.artifact.strip_prefix("base64:") {
                    STANDARD.decode(encoded).map_err(|error| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                    })?
                } else {
                    fs::read(&record.artifact).map_err(|error| {
                        OperationalError::from_source(ErrorKind::RegistryTransport, error)
                    })?
                }
            } else {
                let url = package.artifact_url().ok_or_else(|| OperationalError::new(ErrorKind::Lockfile, "locked artifact URL is missing; regenerate tapid.lock with tapid install before using an empty store"))?;
                let transport = artifact_transport_for_package(
                    &mut transports,
                    registry_config,
                    registry,
                    &key.name,
                    &allowed_origins,
                )?;
                let response = NpmRegistry::new(transport, registry.clone())
                    .download_artifact(url)
                    .map_err(|error| {
                        OperationalError::from_source(ErrorKind::RegistryTransport, error)
                    })?;
                response.body
            };
            let expected: PackageIntegrity = package.artifact_integrity().parse().map_err(
                |error: tapid_core::DomainError| {
                    OperationalError::from_source(ErrorKind::Lockfile, error)
                },
            )?;
            if !integrity_matches(&expected, &integrity(&bytes)) {
                return Err(OperationalError::new(
                    ErrorKind::Integrity,
                    "locked artifact integrity mismatch",
                ));
            }
            let temp_id = NEXT_TEMP_TREE_ID.fetch_add(1, Ordering::Relaxed);
            let temporary = TemporaryTree(
                store
                    .root()
                    .join(format!(".ci-tree-{}-{temp_id}", std::process::id())),
            );
            extract_to(
                &bytes,
                ArchiveFormat::TarGz,
                &temporary.0,
                ArchiveLimits::default(),
            )
            .map_err(|error| OperationalError::from_source(ErrorKind::Archive, error))?;
            transaction
                .stage_verified_tree(&tree_digest, &temporary.0)
                .map_err(OperationalError::from)
        },
    )?;
    Ok((input, trees, transaction, snapshots))
}

#[cfg(test)]
mod ci_tests {
    use super::*;

    #[test]
    fn cached_locked_trees_retain_snapshots_without_transaction_copies() {
        for commit in [false, true] {
            let project = tapid_test_support::TempProject::new("ci-cached-snapshot").unwrap();
            let raw_manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"foo":"1.0.0"}}"#;
            let manifest = PackageManifest::parse(raw_manifest).unwrap();
            project
                .write(
                    "source/package.json",
                    br#"{"name":"foo","version":"1.0.0"}"#,
                )
                .unwrap();
            let source = project.path().join("source");
            let tree_digest: ArtifactDigest =
                canonical_tree_digest(&source).unwrap().parse().unwrap();
            let store = Store::new(project.path().join("store"));
            store.activate_verified_tree(&tree_digest, &source).unwrap();
            let mut package = LockedPackage::new_with_provenance(
                NPM,
                "foo",
                "1.0.0",
                &integrity(b"archive").to_string(),
                tree_digest.as_str(),
                RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap();
            package
                .set_artifact_url("https://registry.npmjs.org/foo/-/foo-1.0.0.tgz")
                .unwrap();
            let mut lock = Lockfile::new(&digest(raw_manifest.as_bytes()).to_string()).unwrap();
            let package_key = package.key();
            lock.insert_package(package).unwrap();
            lock.set_roots([package_key]).unwrap();
            let (input, trees, transaction, snapshots) = prepare_locked_install(
                &lock,
                &manifest,
                &store,
                &crate::registry::RegistryConfig::default(),
                None,
                |_, _| {},
            )
            .unwrap();
            let snapshot = trees.values().next().unwrap().clone();
            assert!(
                snapshot
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("replay-tree-"),
                "cached tree was recopied into a transaction: {}",
                snapshot.display()
            );
            assert_eq!(input.instances[0].tree.root, snapshot);
            assert!(
                fs::read_dir(store.root().join(".staging"))
                    .unwrap()
                    .all(|entry| !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("transaction-tree-"))
            );
            let publication = transaction.publish().unwrap();
            assert_eq!(publication.resolve_path(&snapshot), snapshot);
            assert!(snapshot.join("package.json").is_file());
            if commit {
                publication.commit().unwrap();
            } else {
                publication.rollback().unwrap();
            }
            assert!(snapshot.join("package.json").is_file());
            assert!(store.verified_tree_path(&tree_digest).is_ok());
            drop(snapshots);
            assert!(!snapshot.exists());
            // A later cache miss must also release snapshots already acquired.
            lock.insert_package(
                LockedPackage::new_with_provenance(
                    NPM,
                    "zzz",
                    "1.0.0",
                    &integrity(b"missing").to_string(),
                    &format!("sha256-{}", "0".repeat(64)),
                    RegistryIntegrityProvenance::RegistryDeclared,
                )
                .unwrap(),
            )
            .unwrap();
            assert!(
                prepare_locked_install(
                    &lock,
                    &manifest,
                    &store,
                    &crate::registry::RegistryConfig::default(),
                    None,
                    |_, _| {}
                )
                .is_err()
            );
            assert!(
                fs::read_dir(store.root().join(".staging"))
                    .unwrap()
                    .all(|entry| !entry.unwrap().path().join("tree").exists())
            );
        }
    }
}
