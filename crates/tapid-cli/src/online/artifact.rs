use super::*;

pub(super) struct ArtifactFetcher<'a> {
    pub(super) store: &'a Store,
    pub(super) config: &'a crate::registry::RegistryConfig,
    pub(super) transports: BTreeMap<(String, String), HttpsTransport>,
    pub(super) allowed_origins: Vec<String>,
}

impl ArtifactFetcher<'_> {
    pub(super) fn prepare(
        &mut self,
        id: &tapid_registry_client::RegistryPackageId,
        record: &PackageRecord,
        pinned: Option<&LockedPackage>,
        store_transaction: &mut StoreTransaction,
    ) -> Result<(PackageIntegrity, ArtifactDigest, PathBuf), OperationalError> {
        let store = self.store;
        if let Some(package) = pinned {
            let digest: ArtifactDigest = package
                .tree_digest()
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())?;
            match store.verified_tree_snapshot(&digest) {
                Ok(snapshot) => {
                    let _guard = TemporaryTree(snapshot.clone());
                    let tree = store_transaction.stage_verified_tree(&digest, &snapshot)?;
                    return Ok((
                        package
                            .artifact_integrity()
                            .parse()
                            .map_err(|error: tapid_core::DomainError| error.to_string())?,
                        digest,
                        tree,
                    ));
                }
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
                    return Err(
                        OperationalError::from(error).context("locked store tree is invalid")
                    );
                }
            }
        }
        if pinned.is_some()
            && record.registry.registry().is_some()
            && !record.fixture
            && record.artifact.is_empty()
        {
            return Err(OperationalError::new(
                ErrorKind::Lockfile,
                "locked artifact has no pinned archive URL; regenerate tapid.lock with tapid update and review the resulting changes",
            ));
        }
        let bytes = if let Some(bytes) = &record.copied_archive {
            bytes.as_ref().clone()
        } else if record.fixture {
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
                &mut self.transports,
                self.config,
                id.registry
                    .registry()
                    .ok_or("copied artifact cannot use registry transport")?,
                &id.name,
                &self.allowed_origins,
            )?;
            let response = if record.registry.to_string() == JSR {
                JsrRegistry::new(
                    transport,
                    record
                        .registry
                        .registry()
                        .ok_or("copied artifact cannot use registry transport")?
                        .clone(),
                )
                .download_artifact(&record.artifact)
            } else {
                NpmRegistry::new(
                    transport,
                    record
                        .registry
                        .registry()
                        .ok_or("copied artifact cannot use registry transport")?
                        .clone(),
                )
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
        if record
            .registry
            .artifact_digest()
            .is_some_and(|expected| expected != digest(&bytes).as_str())
        {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                "copied artifact digest mismatch",
            ));
        }
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
            copied::archive_format(&record.registry, &bytes),
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
        if pinned.is_some_and(|package| package.tree_digest() != tree_digest.as_str()) {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                format!("locked tree digest mismatch for {id}"),
            ));
        }
        let tree = store_transaction
            .stage_verified_tree(&tree_digest, &temp)
            .map_err(OperationalError::from)?;
        Ok((actual, tree_digest, tree))
    }
}
