use super::*;

pub(super) struct ResolvedGraph {
    pub metadata_lookups: usize,
    pub resolution: Resolution,
    pub records: BTreeMap<PackageRecordKey, PackageRecord>,
    pub workspace_links: WorkspaceLinkPlan,
    pub workspace_locked: Vec<LockedWorkspacePackage>,
    pub workspace_registry_dependencies: Vec<WorkspaceRegistryDependency>,
    pub root_registry_identities: BTreeSet<(PackageSource, PackageName)>,
    pub workspace_root_keys: Vec<String>,
    pub direct_local_names: BTreeSet<PackageName>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_package_graph(
    project: &Path,
    manifest: &PackageManifest,
    workspace: WorkspaceMaterialization,
    fixture_path: Option<&Path>,
    allow_missing_integrity: bool,
    registry_config: &crate::registry::RegistryConfig,
    previous_lock: Option<&Lockfile>,
    mut progress: impl FnMut(crate::application::install::Progress),
) -> Result<ResolvedGraph, OperationalError> {
    let WorkspaceRootResolution {
        mut roots,
        direct_root_identities: mut root_registry_identities,
        workspace_root_keys,
        registry_dependencies: workspace_registry_dependencies,
        overrides,
    } = workspace_root_resolution(manifest, &workspace, registry_config)?;
    let direct_local_names = manifest
        .dependencies()
        .keys()
        .chain(manifest.dev_dependencies().keys())
        .chain(manifest.optional_dependencies().keys())
        .map(|name| dep_parts(name).map(|(_, name)| name))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let WorkspaceMaterialization {
        links: workspace_links,
        locked: workspace_locked,
        members: _,
        registry_dependencies: _,
        peer_dependencies: workspace_peer_dependencies,
    } = workspace;
    let fixture = fixture_path.map(fixture).transpose()?;
    let mut fixture_records = BTreeMap::<(String, String, String), PackageRecord>::new();
    if let Some(f) = &fixture {
        for p in &f.packages {
            let registry: PackageSource =
                p.registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                            .context(format!("invalid fixture registry {}", p.registry))
                    })?;
            let name: PackageName = p.name.parse().map_err(|error: tapid_core::DomainError| {
                OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                    .context(format!("invalid fixture package {}", p.name))
            })?;
            let version: PackageVersion =
                p.version
                    .parse()
                    .map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                            .context(format!("invalid fixture version {}", p.version))
                    })?;
            let integrity = p
                .integrity
                .clone()
                .map(|v| {
                    v.parse().map_err(|error: tapid_core::DomainError| {
                        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                            .context(format!("invalid fixture integrity {v}"))
                    })
                })
                .transpose()?;
            if registry.to_string() == NPM && integrity.is_none() && !allow_missing_integrity {
                return Err(OperationalError::new(
                    ErrorKind::RegistryMetadata,
                    format!(
                        "fixture npm metadata for {name}@{version} is missing dist.integrity; pass --allow-unverified-registry-artifacts for an explicit compatibility exception"
                    ),
                ));
            }
            fixture_records.insert(
                (p.registry.clone(), p.name.clone(), p.version.clone()),
                PackageRecord {
                    git_reference: p.git_ref.clone(),
                    copied_archive: None,
                    dist_tags: p.dist_tags.clone(),
                    registry,
                    name,
                    version,
                    integrity,
                    artifact: p.artifact.clone(),
                    dependencies: p.dependencies.clone(),
                    peer_dependencies: p.peer_dependencies.clone(),
                    optional_peer_dependencies: p.optional_peer_dependencies.clone(),
                    optional_dependencies: BTreeMap::new(),
                    platform: PackagePlatform::unrestricted(),
                    fixture: true,
                },
            );
        }
    }
    let copied_roots = copied::prepare_roots(
        project,
        manifest,
        previous_lock,
        &fixture_records,
        fixture.is_some(),
    )?;
    let mut copied_records = BTreeMap::new();
    for (dependency, record) in copied_roots {
        root_registry_identities.insert((dependency.registry.clone(), record.name.clone()));
        roots.push(dependency);
        copied_records.insert(
            (
                record.registry.to_string(),
                record.name.to_string(),
                record.version.to_string(),
            ),
            record,
        );
    }
    let mut local_identities = BTreeMap::new();
    for root in &roots {
        register_local_root_identity(
            &mut local_identities,
            root.name.clone(),
            format!(
                "{}|{}",
                root.registry,
                root.requirement.package_name(&root.name)
            ),
        )?;
    }
    let configured_origins = registry_config.configured_origins();
    let mut metadata_transports = BTreeMap::<(String, String), HttpsTransport>::new();
    let preferred = previous_lock
        .map(|lock| {
            locked::reusable_locked_packages(lock).map(|packages| {
                packages
                    .into_iter()
                    .filter_map(|(key, _)| {
                        key.source
                            .package_source()
                            .map(|registry| (registry, key.name, key.version))
                    })
                    .collect::<BTreeSet<_>>()
            })
        })
        .transpose()?
        .unwrap_or_default();
    let mut preferred = tapid_resolver::ResolutionPreferences {
        versions: preferred,
        roots: BTreeMap::new(),
    };
    if let Some(lock) = previous_lock {
        let bindings = if lock.root_bindings().is_empty() {
            lock.roots()
                .iter()
                .map(|key| {
                    let key: LockfilePackageKey = key.parse()?;
                    Ok((key.name.to_string(), key))
                })
                .collect::<Result<Vec<_>, OperationalError>>()?
        } else {
            lock.root_bindings()
                .iter()
                .map(|(name, key)| Ok((name.clone(), key.parse()?)))
                .collect::<Result<Vec<_>, OperationalError>>()?
        };
        for (local, key) in bindings {
            if let Some(registry) = key.source.package_source()
                && preferred.versions.contains(&(
                    registry.clone(),
                    key.name.clone(),
                    key.version.clone(),
                ))
                && locked::locked_platform_matches(&key)?
            {
                preferred.roots.insert(
                    (
                        registry.clone(),
                        local
                            .parse()
                            .map_err(|error: tapid_core::DomainError| error.to_string())?,
                        key.name,
                    ),
                    key.version,
                );
            }
        }
    }
    let mut seed = previous_lock
        .map(|lock| locked::locked_records(lock, &fixture_records, fixture.is_some()))
        .transpose()?
        .unwrap_or_default();
    seed.retain(|record| {
        !copied_records.contains_key(&(
            record.registry.to_string(),
            record.name.to_string(),
            record.version.to_string(),
        ))
    });
    seed.extend(copied_records.values().cloned());
    let mut metadata_lookups = 0;
    let mut fetch_records = |registry: &PackageSource, name: &PackageName| {
        metadata_lookups += 1;
        if registry.registry().is_none() {
            return Ok(copied_records
                .values()
                .filter(|record| &record.registry == registry && &record.name == name)
                .cloned()
                .collect());
        }
        if fixture.is_some() {
            Ok(fixture_records
                .values()
                .filter(|package| &package.registry == registry && &package.name == name)
                .cloned()
                .collect())
        } else {
            let registry = registry
                .registry()
                .ok_or("copied artifact metadata is unavailable")?;
            let transport = metadata_transport_for_package(
                &mut metadata_transports,
                registry_config,
                registry,
                name,
                &configured_origins,
            )?;
            remote_records(
                transport,
                registry_config,
                registry,
                name,
                allow_missing_integrity,
            )
        }
    };
    let mut tagged_providers = BTreeSet::new();
    for peer in &workspace_peer_dependencies {
        if peer.requirement.dist_tag().is_some()
            && let WorkspacePeerProvider::Registry { registry, package } = &peer.provider
            && root_registry_identities.contains(&(registry.clone(), package.clone()))
            && tagged_providers.insert((registry.clone(), package.clone()))
        {
            let provider_records = fetch_records(registry, package)?;
            seed.retain(|record| record.registry != *registry || record.name != *package);
            seed.extend(provider_records);
        }
    }
    let (resolution, records) = resolution::resolve_with_preferences(
        &roots,
        |parent, dependency| registry_config.registry_for_dependency(parent, dependency),
        &overrides,
        fetch_records,
        &preferred,
        seed,
        |fetched| progress(crate::application::install::Progress::Metadata(fetched)),
    )?;
    validate_workspace_peer_providers(
        &workspace_peer_dependencies,
        &root_registry_identities,
        &resolution,
        &records,
    )?;

    Ok(ResolvedGraph {
        metadata_lookups,
        resolution,
        records,
        workspace_links,
        workspace_locked,
        workspace_registry_dependencies,
        root_registry_identities,
        workspace_root_keys,
        direct_local_names,
    })
}

pub(super) fn workspace_registry_target<'a>(
    dependency: &WorkspaceRegistryDependency,
    resolution: &'a Resolution,
    records: &BTreeMap<PackageRecordKey, PackageRecord>,
) -> Result<&'a tapid_registry_client::RegistryPackageId, OperationalError> {
    resolution.roots.iter().find(|id| {
        id.registry == dependency.registry && id.name == dependency.package
            && records.get(&(id.registry.to_string(), id.name.to_string(), id.version.to_string()))
                .is_some_and(|record| dependency.requirement.matches_tagged_version(&id.version, &record.dist_tags))
    }).ok_or_else(|| OperationalError::new(ErrorKind::Resolution, format!(
        "no selected registry package satisfies workspace member dependency '{}' ({:?})",
        dependency.manifest_name, dependency.requirement)))
}
