//! Verify pinned npm artifacts and compose the existing install transaction.
use super::*;
use tapid_lockfile::ImportedNpmLockfile;

pub(crate) fn fetch_imported(
    imported: &mut ImportedNpmLockfile,
    project: &Path,
    store: &Store,
    offline: bool,
    fixture_path: Option<&Path>,
    registry_config: &crate::registry::RegistryConfig,
) -> ResolveAndFetchOutput {
    let manifest = fs::read_to_string(project.join("package.json"))
        .map_err(|e| OperationalError::from_source(ErrorKind::Manifest, e))?;
    imported
        .validate_replay(&manifest, &root_digest(project)?)
        .map_err(|e| OperationalError::new(ErrorKind::LockManifestMismatch, e.to_string()))?;
    let graph = imported
        .graph()
        .map_err(|e| OperationalError::new(ErrorKind::Lockfile, e.to_string()))?;
    let selected = graph
        .selected_paths(std::env::consts::OS, std::env::consts::ARCH, current_libc())
        .map_err(|e| OperationalError::new(ErrorKind::Lockfile, e.to_string()))?;
    let fixture: Option<Fixture> = fixture_path
        .map(|path| {
            fs::read(path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        })
        .transpose()?;
    store
        .recover_transactions()
        .map_err(OperationalError::from)?;
    let mut transaction = store.transaction();
    let mut transports = BTreeMap::new();
    let origins = registry_config.configured_origins();
    let mut locked_by_path = BTreeMap::new();
    let mut keys = BTreeMap::new();
    let mut instances = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut receipts = BTreeMap::new();
    for path in &selected {
        let package = &graph.packages[path];
        let peer = graph.peer_context(path);
        let platform = selected_platform_context_for(
            std::env::consts::OS,
            std::env::consts::ARCH,
            current_libc(),
            &PackagePlatform {
                os: package.os.clone(),
                cpu: package.cpu.clone(),
                libc: package.libc.clone(),
            },
        )?;
        let cached = imported
            .verified_tree(path)
            .map(|digest| {
                let digest = digest
                    .parse::<ArtifactDigest>()
                    .map_err(|e| e.to_string())?;
                store
                    .verified_tree_snapshot(&digest)
                    .map(|tree| (digest, TemporaryTree(tree)))
                    .map_err(|e| e.to_string())
            })
            .transpose();
        let (digest, temp) = match cached {
            Ok(Some(cached)) => cached,
            _ if offline => {
                return Err(OperationalError::new(
                    ErrorKind::Store,
                    format!(
                        "imported package {}@{} has no available verified tree; run tapid install --frozen first",
                        package.name, package.version
                    ),
                ));
            }
            _ => {
                let bytes = if let Some(fixture) = &fixture {
                    let record = fixture
                        .packages
                        .iter()
                        .find(|record| {
                            record.registry == package.registry.as_str()
                                && record.name == package.name.as_str()
                                && record.version == package.version.to_string()
                        })
                        .ok_or_else(|| {
                            format!(
                                "fixture has no pinned artifact for {}@{}",
                                package.name, package.version
                            )
                        })?;
                    if record.integrity.as_deref() != Some(&package.integrity.to_string()) {
                        return Err(OperationalError::new(
                            ErrorKind::Integrity,
                            format!(
                                "fixture integrity disagrees with imported selection at {path}"
                            ),
                        ));
                    }
                    if let Some(encoded) = record.artifact.strip_prefix("base64:") {
                        STANDARD.decode(encoded).map_err(|e| e.to_string())?
                    } else {
                        fs::read(&record.artifact).map_err(|e| e.to_string())?
                    }
                } else {
                    let transport = artifact_transport_for_package(
                        &mut transports,
                        registry_config,
                        &package.registry,
                        &package.name,
                        &origins,
                    )?;
                    NpmRegistry::new(transport, package.registry.clone())
                        .download_artifact(&package.resolved)
                        .map_err(|e| {
                            OperationalError::from_source(ErrorKind::RegistryTransport, e)
                        })?
                        .body
                };
                if integrity(&bytes) != package.integrity {
                    return Err(OperationalError::new(
                        ErrorKind::Integrity,
                        format!(
                            "integrity mismatch for {}@{} at {path}",
                            package.name, package.version
                        ),
                    ));
                }
                let temp = TemporaryTree(store.root().join(format!(
                    ".npm-import-tree-{}-{}",
                    std::process::id(),
                    NEXT_TEMP_TREE_ID.fetch_add(1, Ordering::Relaxed)
                )));
                extract_to(
                    &bytes,
                    ArchiveFormat::TarGz,
                    &temp.0,
                    ArchiveLimits::default(),
                )
                .map_err(|e| OperationalError::from_source(ErrorKind::Archive, e))?;
                let digest: ArtifactDigest = canonical_tree_digest(&temp.0)
                    .map_err(|e| e.to_string())?
                    .parse()
                    .map_err(|e: tapid_core::DomainError| e.to_string())?;
                (digest, temp)
            }
        };
        let artifact_manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(temp.0.join("package/package.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if artifact_manifest["name"].as_str() != Some(package.name.as_str())
            || artifact_manifest["version"].as_str() != Some(&package.version.to_string())
        {
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                format!("archive identity disagrees with imported selection at {path}"),
            ));
        }
        let tree = transaction
            .stage_verified_tree(&digest, &temp.0)
            .map_err(OperationalError::from)?;
        receipts.insert(path.clone(), digest.to_string());
        let mut locked = LockedPackage::new_with_context_and_provenance(
            package.registry.as_str(),
            package.name.as_str(),
            &package.version.to_string(),
            &package.integrity.to_string(),
            &digest.to_string(),
            (&peer, &platform),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .map_err(OperationalError::from)?;
        locked
            .set_artifact_url(&package.resolved)
            .map_err(OperationalError::from)?;
        let key = locked.key();
        let instance = PackageInstance {
            id: tapid_core::PackageInstanceId::new(
                package.registry.clone(),
                package.name.clone(),
                package.version.clone(),
            ),
            peer_context: peer,
            platform_context: platform,
            tree: VerifiedTreeReference::new(&digest.to_string(), &tree)
                .map_err(|e| e.to_string())?,
        };
        keys.insert(path.clone(), InstanceKey::from(&instance));
        // Equal contexts were checked during import. Preserve distinct contexts.
        instances.entry(key.clone()).or_insert(instance);
        trees.insert(key, tree);
        locked_by_path.insert(path.clone(), locked);
    }
    let mut edges = Vec::new();
    let mut packages = BTreeMap::new();
    for (path, mut locked) in locked_by_path {
        let package = &graph.packages[&path];
        for (name, target) in package.dependencies.iter().chain(&package.peers) {
            if !selected.contains(target) {
                continue;
            }
            let target_key = &keys[target];
            let key = LockfilePackageKey::new(
                target_key.id.registry.clone(),
                target_key.id.name.clone(),
                target_key.id.version.clone(),
                &target_key.peer_context,
                &target_key.platform_context,
            )
            .to_string();
            locked
                .add_alias_dependency(name, &key)
                .map_err(OperationalError::from)?;
            edges.push(NamedDependencyEdge {
                parent: keys[&path].clone(),
                dependency: NamedDependency {
                    name: name
                        .parse()
                        .map_err(|e: tapid_core::DomainError| e.to_string())?,
                    child: keys[target].clone(),
                },
            });
        }
        packages.entry(locked.key()).or_insert(locked);
    }
    let mut lock =
        Lockfile::new(imported.root_manifest_digest()).map_err(OperationalError::from)?;
    lock.insert_packages(packages.into_values())
        .map_err(OperationalError::from)?;
    let mut roots = Vec::new();
    let mut bindings = BTreeMap::new();
    for (name, path) in &graph.roots {
        if !selected.contains(path) {
            continue;
        }
        let key = &keys[path];
        let encoded = LockfilePackageKey::new(
            key.id.registry.clone(),
            key.id.name.clone(),
            key.id.version.clone(),
            &key.peer_context,
            &key.platform_context,
        )
        .to_string();
        bindings.insert(name.clone(), encoded);
        roots.push(NamedDependency {
            name: name
                .parse()
                .map_err(|e: tapid_core::DomainError| e.to_string())?,
            child: key.clone(),
        });
    }
    lock.set_roots(bindings.values())
        .map_err(OperationalError::from)?;
    lock.set_root_bindings(bindings)
        .map_err(OperationalError::from)?;
    imported
        .record_verified_trees(receipts)
        .map_err(|e| e.to_string())?;
    Ok((
        lock,
        NamedLayoutInput {
            instances: instances.into_values().collect(),
            root_dependencies: roots,
            dependency_edges: edges,
        },
        trees,
        transaction,
        WorkspaceLinkPlan { links: Vec::new() },
    ))
}
