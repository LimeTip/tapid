//! Fetch exact locked artifacts without consulting registry version metadata.
use super::*;

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

impl LockedSnapshots {
    /// Keep an owned private snapshot alive until activation finishes.
    pub(crate) fn retain(&mut self, tree: PathBuf) {
        self.trees.push(TemporaryTree(tree));
    }
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
                    snapshots.retain(snapshot.clone());
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
mod tests {
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
