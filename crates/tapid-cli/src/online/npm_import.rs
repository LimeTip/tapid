//! Verify pinned npm artifacts and compose the existing install transaction.
use super::*;
use crate::application::outcome::{DiagnosticContext, DiagnosticPackage};
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
    let current_digest = root_digest(project)?;
    imported
        .validate_replay(&manifest, &current_digest)
        .map_err(|source| {
            let mut error = OperationalError::from_source(ErrorKind::LockManifestMismatch, source);
            if imported.root_manifest_digest() != current_digest {
                error = error.with_diagnostic(DiagnosticContext::comparison(
                    "root_manifest_digest",
                    Some("sha256"),
                    imported.root_manifest_digest(),
                    &current_digest,
                ));
            }
            error.at_manifest(&project.join("package.json"))
        })?;
    let workspace = workspace_materialization(project, registry_config)?;
    imported
        .validate_workspace_manifests(&imported_workspace_manifests(project, &workspace)?)
        .map_err(|e| OperationalError::new(ErrorKind::LockManifestMismatch, e.to_string()))?;
    let graph = imported
        .graph()
        .map_err(|e| OperationalError::new(ErrorKind::Lockfile, e.to_string()))?;
    let selected = graph
        .selected_paths(std::env::consts::OS, std::env::consts::ARCH, current_libc())
        .map_err(|e| OperationalError::new(ErrorKind::Lockfile, e.to_string()))?;
    for path in &selected {
        let package = &graph.packages[path];
        let identity = DiagnosticPackage {
            source: package.registry.to_string(),
            name: package.name.to_string(),
            version: Some(package.version.to_string()),
        };
        let origin = registry_config
            .origin_for_name(&package.name)
            .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
        if origin != package.registry {
            return Err(OperationalError::new(
                ErrorKind::RegistryConfiguration,
                format!(
                    "registry identity mismatch for package {} at {path}: selected {origin}, imported {}",
                    package.name, package.registry
                ),
            ).with_diagnostic(DiagnosticContext::comparison("registry_identity", None, origin, &package.registry)).for_package(identity));
        }
    }
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
        let identity = DiagnosticPackage {
            source: package.registry.to_string(),
            name: package.name.to_string(),
            version: Some(package.version.to_string()),
        };
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
            .verified_artifact(path)
            .filter(|receipt| {
                receipt.resolved == package.resolved
                    && receipt.integrity == package.integrity.to_string()
            })
            .map(|receipt| {
                let digest = receipt.tree_digest.as_str();
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
                        )
                        .with_diagnostic(DiagnosticContext::comparison(
                            "artifact_integrity",
                            Some("sha512"),
                            &package.integrity,
                            record
                                .integrity
                                .as_ref()
                                .map(|value| {
                                    value
                                        .parse::<PackageIntegrity>()
                                        .map(|value| value.to_string())
                                        .unwrap_or_else(|_| "invalid_integrity".into())
                                })
                                .unwrap_or_else(|| "missing".into()),
                        ))
                        .for_package(identity));
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
                    )
                    .with_diagnostic(DiagnosticContext::comparison(
                        "artifact_integrity",
                        Some("sha512"),
                        &package.integrity,
                        integrity(&bytes),
                    ))
                    .for_package(identity));
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
            let observed = artifact_manifest["name"]
                .as_str()
                .and_then(|value| value.parse::<PackageName>().ok())
                .zip(
                    artifact_manifest["version"]
                        .as_str()
                        .and_then(|value| value.parse::<PackageVersion>().ok()),
                )
                .map(|(name, version)| format!("{name}@{version}"))
                .unwrap_or_else(|| "invalid_package_identity".into());
            return Err(OperationalError::new(
                ErrorKind::Integrity,
                format!("archive identity disagrees with imported selection at {path}"),
            )
            .with_diagnostic(DiagnosticContext::comparison(
                "artifact_identity",
                None,
                format!("{}@{}", package.name, package.version),
                observed,
            ))
            .for_package(identity));
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
            let key = LockfilePackageKey::from_source(
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
        let encoded = LockfilePackageKey::from_source(
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
        .record_verified_trees(receipts.clone())
        .map_err(|e| e.to_string())?;
    let artifact_receipts = imported
        .graph()
        .map_err(|e| e.to_string())?
        .packages
        .into_iter()
        .filter_map(|(path, package)| {
            receipts.get(&path).map(|tree_digest| {
                (
                    path,
                    tapid_lockfile::ImportedNpmArtifactReceipt {
                        tree_digest: tree_digest.clone(),
                        resolved: package.resolved,
                        integrity: package.integrity.to_string(),
                    },
                )
            })
        })
        .collect();
    imported
        .record_verified_artifacts(artifact_receipts)
        .map_err(|e| e.to_string())?;
    let instances: Vec<_> = instances.into_values().collect();
    let mut plan = WorkspaceInstallPlan::from(workspace.links);
    plan.include_owned_members()?;
    for (member, points) in &graph.member_roots {
        let roots = &points.dependencies;
        let local_roots: Vec<_> = roots
            .iter()
            .filter(|(name, path)| {
                *path != &format!("node_modules/{name}") && selected.contains(*path)
            })
            .map(|(name, path)| {
                Ok(NamedDependency {
                    name: name
                        .parse()
                        .map_err(|e: tapid_core::DomainError| e.to_string())?,
                    child: keys[path].clone(),
                })
            })
            .collect::<Result<_, String>>()?;
        let links = WorkspaceLinkPlan {
            links: graph
                .workspace_links
                .iter()
                .filter(|(path, _)| path.starts_with(&format!("{member}/node_modules/")))
                .map(|(path, target)| {
                    let source = &graph.workspaces[target];
                    Ok(tapid_linker::WorkspaceLink {
                        name: source["name"]
                            .as_str()
                            .expect("name")
                            .parse()
                            .map_err(|e: tapid_core::DomainError| e.to_string())?,
                        version: source["version"]
                            .as_str()
                            .expect("version")
                            .parse()
                            .map_err(|e: tapid_core::DomainError| e.to_string())?,
                        source: project.join(target),
                        target: project.join(path),
                    })
                })
                .collect::<Result<_, String>>()?,
        };
        if !local_roots.is_empty()
            || !links.links.is_empty()
            || plan.members.contains_key(&project.join(member))
        {
            plan.members.insert(
                project.join(member),
                WorkspaceMemberInstall {
                    input: pinned_layout(
                        roots
                            .iter()
                            .filter(|(_, path)| selected.contains(*path))
                            .map(|(name, path)| {
                                Ok(NamedDependency {
                                    name: name
                                        .parse()
                                        .map_err(|e: tapid_core::DomainError| e.to_string())?,
                                    child: keys[path].clone(),
                                })
                            })
                            .collect::<Result<_, String>>()?,
                        &instances,
                        &edges,
                    ),
                    links,
                },
            );
        }
    }
    plan.links.links.retain(|link| {
        graph
            .workspace_links
            .contains_key(&format!("node_modules/{}", link.name))
    });
    Ok((
        lock,
        pinned_layout(roots, &instances, &edges),
        trees,
        transaction,
        plan,
    ))
}

/// Read the same contained workspace sources used by native installs.
/// Digests also detect edits between discovery and import publication.
pub(crate) fn imported_workspace_manifests(
    project: &Path,
    workspace: &WorkspaceMaterialization,
) -> Result<BTreeMap<String, (String, String)>, String> {
    let canonical_project = fs::canonicalize(project)
        .map_err(|error| format!("cannot resolve install project root: {error}"))?;
    let project = canonical_project.as_path();
    let mut manifests = BTreeMap::new();
    for (source, _) in workspace.members.values() {
        let member_root = project.join(source.path());
        for ancestor in member_root
            .ancestors()
            .take_while(|ancestor| *ancestor != project)
        {
            match fs::symlink_metadata(ancestor.join("node_modules")) {
                Ok(metadata) => {
                    if metadata.file_type().is_dir()
                        && workspace
                            .members
                            .values()
                            .any(|(source, _)| project.join(source.path()) == ancestor)
                    {
                        crate::filesystem::activation::validate_managed_node_modules(ancestor)?;
                        continue;
                    }
                    return Err(format!(
                        "/packages/{}: node_modules: remove the unmanaged member or ancestor node_modules tree before importing or replaying workspace selections",
                        source.path().replace('/', "~1")
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let path = project.join(source.path()).join("package.json");
        if !fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_file()
        {
            return Err("workspace package.json must be a regular, non-symlink file".into());
        }
        let canonical = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        if !canonical.starts_with(project) {
            return Err("workspace manifest escapes project root".into());
        }
        let text = fs::read_to_string(&canonical).map_err(|e| e.to_string())?;
        let current_digest = digest(text.as_bytes()).to_string();
        let key = LockfilePackageKey::workspace(source.clone()).to_string();
        if !workspace
            .locked
            .iter()
            .any(|member| member.key() == key && member.manifest_digest() == current_digest)
        {
            return Err("workspace member manifest changed during discovery; retry".into());
        }
        manifests.insert(source.path().to_owned(), (text, current_digest));
    }
    Ok(manifests)
}

/// Each managed directory contains only the registry graph reachable from its entry points.
fn pinned_layout(
    roots: Vec<NamedDependency>,
    instances: &[PackageInstance],
    edges: &[NamedDependencyEdge],
) -> NamedLayoutInput {
    let mut reachable = BTreeSet::new();
    let mut pending: Vec<_> = roots.iter().map(|root| root.child.clone()).collect();
    while let Some(key) = pending.pop() {
        if !reachable.insert(key.clone()) {
            continue;
        }
        pending.extend(
            edges
                .iter()
                .filter(|edge| edge.parent == key)
                .map(|edge| edge.dependency.child.clone()),
        );
    }
    NamedLayoutInput {
        instances: instances
            .iter()
            .filter(|instance| reachable.contains(&InstanceKey::from(*instance)))
            .cloned()
            .collect(),
        root_dependencies: roots,
        dependency_edges: edges
            .iter()
            .filter(|edge| reachable.contains(&edge.parent))
            .cloned()
            .collect(),
    }
}
