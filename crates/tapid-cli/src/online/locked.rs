//! Fetch exact locked artifacts without consulting registry version metadata.
use super::*;

type LockedInstallOutput = Result<
    (
        NamedLayoutInput,
        BTreeMap<String, PathBuf>,
        StoreTransaction,
    ),
    OperationalError,
>;

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
                    let temporary = TemporaryTree(snapshot);
                    return transaction
                        .stage_verified_tree(&tree_digest, &temporary.0)
                        .map_err(OperationalError::from);
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
                    .find(|record| {
                        record.registry == registry.as_str()
                            && record.name == key.name.as_str()
                            && record.version == key.version.to_string()
                    })
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
    Ok((input, trees, transaction))
}
