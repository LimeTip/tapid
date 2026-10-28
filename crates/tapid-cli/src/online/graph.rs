use super::*;
use crate::application::lifecycle::UpdateRequest;

pub(super) struct ResolvedGraph {
    pub workspace_manifests: BTreeMap<String, PackageManifest>,
    pub omissions: Vec<tapid_lockfile::AcceptedOmission>,
    pub candidate_exclusions: crate::application::outcome::CandidateExclusions,
    pub updated_manifest: Option<PackageManifest>,
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
    update: Option<UpdateRequest<'_>>,
    mut progress: impl FnMut(crate::application::install::Progress),
) -> Result<ResolvedGraph, OperationalError> {
    let WorkspaceRootResolution {
        mut entry_points,
        mut roots,
        direct_root_identities: mut root_registry_identities,
        workspace_root_keys,
        registry_dependencies: mut workspace_registry_dependencies,
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
        manifests: workspace_manifests,
        links: workspace_links,
        locked: workspace_locked,
        members: _,
        registry_dependencies: _,
        peer_dependencies: workspace_peer_dependencies,
        declarations,
    } = workspace;
    let declarations = crate::application::outcome::DiagnosticContext::manifest_requirements(
        manifest,
        &project.join("package.json"),
        None,
    )
    .into_iter()
    .chain(declarations)
    .collect::<Vec<_>>();
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
                    optional_dependencies: p.optional_dependencies.clone(),
                    platform: PackagePlatform {
                        os: p.os.clone(),
                        cpu: p.cpu.clone(),
                        libc: p.libc.clone(),
                    },
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
        update,
    )?;
    let mut copied_records = BTreeMap::new();
    for (dependency, record) in copied_roots {
        root_registry_identities.insert((dependency.registry.clone(), record.name.clone()));
        for (section, map) in [
            (
                tapid_resolver::EntryPointSection::Dependencies,
                manifest.dependencies(),
            ),
            (
                tapid_resolver::EntryPointSection::DevDependencies,
                manifest.dev_dependencies(),
            ),
            (
                tapid_resolver::EntryPointSection::OptionalDependencies,
                manifest.optional_dependencies(),
            ),
        ] {
            if map.contains_key(dependency.name.as_str()) {
                entry_points.push((
                    dependency.clone(),
                    tapid_resolver::EntryPoint {
                        workspace: None,
                        section,
                    },
                ));
            }
        }
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
        refresh_roots: BTreeSet::new(),
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
        let mut bindings = bindings;
        for member in lock.workspace_packages().values() {
            for (local, key) in member.dependencies() {
                bindings.push((local.clone(), key.parse()?));
            }
        }
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
    // Keep metadata shared by root refresh, tagged peers, and saved declarations.
    // Incremental resolution already deduplicates its own retrievals; retaining
    // all of its packuments too would duplicate the largest part of the graph.
    // Peer-only saves may need metadata fetched during resolution because they
    // have no installed root binding, so retain it for that later consumer.
    let resolving = std::cell::Cell::new(false);
    let save_peer_metadata =
        update.is_some_and(|request| {
            request.save.is_some()
                && request.plan.mutations.iter().any(|mutation| {
                    mutation.kind == tapid_manifest::DependencyKind::PeerDependencies
                })
        });
    let mut metadata = BTreeMap::<(PackageSource, PackageName), Vec<PackageRecord>>::new();
    let mut fetch_records = |registry: &PackageSource, name: &PackageName| {
        let key = (registry.clone(), name.clone());
        if let Some(records) = metadata.get(&key) {
            return Ok(records.clone());
        }
        metadata_lookups += 1;
        let records = if registry.registry().is_none() {
            copied_records
                .values()
                .filter(|record| &record.registry == registry && &record.name == name)
                .cloned()
                .collect()
        } else if fixture.is_some() {
            fixture_records
                .values()
                .filter(|package| &package.registry == registry && &package.name == name)
                .cloned()
                .collect()
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
            )?
        };
        if !resolving.get() || save_peer_metadata {
            Ok(metadata.entry(key).or_insert(records).clone())
        } else {
            Ok(records)
        }
    };
    if let Some(request) = update.filter(|request| request.named) {
        update::refresh_selected(
            request,
            &roots,
            &mut preferred,
            &mut seed,
            &mut fetch_records,
        )?;
    }
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
    let mut omissions = Vec::new();
    if roots.iter().all(|root| root.registry.registry().is_some()) {
        let required = entry_points
            .iter()
            .filter(|(_, entry)| {
                entry.section != tapid_resolver::EntryPointSection::OptionalDependencies
            })
            .map(|(dependency, _)| {
                (
                    dependency.registry.clone(),
                    dependency
                        .requirement
                        .package_name(&dependency.name)
                        .clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        let mut absent = BTreeSet::new();
        for (dependency, entry) in &entry_points {
            let actual = dependency.requirement.package_name(&dependency.name);
            let identity = (dependency.registry.clone(), actual.clone());
            if entry.section != tapid_resolver::EntryPointSection::OptionalDependencies
                || required.contains(&identity)
            {
                continue;
            }
            // Preserve compatible locked roots outside an explicit refresh scope.
            // Their existing artifact pin does not require packument availability.
            if preferred
                .roots
                .get(&(
                    dependency.registry.clone(),
                    dependency.name.clone(),
                    actual.clone(),
                ))
                .is_some_and(|version| {
                    seed.iter().any(|record| {
                        record.registry == dependency.registry
                            && &record.name == actual
                            && &record.version == version
                            && dependency.requirement.matches_locked_version(version)
                            && current_platform_matches(&record.platform)
                    })
                })
            {
                continue;
            }
            // Fetch errors remain fatal. Candidate eligibility uses the same typed
            // normalization as resolution, without interpreting display strings.
            let candidates = fetch_records(&dependency.registry, actual)?;
            let matching = candidates
                .iter()
                .filter(|candidate| {
                    dependency
                        .requirement
                        .matches_tagged_version(&candidate.version, &candidate.dist_tags)
                })
                .collect::<Vec<_>>();
            let reason = if candidates.is_empty() {
                Some(tapid_lockfile::OmissionReason::MetadataAbsent)
            } else if !matching.is_empty()
                && matching
                    .iter()
                    .all(|candidate| !current_platform_matches(&candidate.platform))
            {
                Some(tapid_lockfile::OmissionReason::IncompatiblePlatform)
            } else if matching.iter().all(|candidate| {
                let mut candidate = (*candidate).clone();
                resolution::apply_overrides(&mut candidate, &overrides);
                resolution::normalize_record(&candidate).is_err()
            }) {
                Some(tapid_lockfile::OmissionReason::NoUsableCandidate)
            } else {
                None
            };
            if let Some(reason) = reason {
                absent.insert((
                    entry.clone(),
                    dependency.registry.clone(),
                    dependency.name.clone(),
                ));
                omissions.push(tapid_lockfile::AcceptedOmission {
                    original_requirement: None,
                    origin: entry
                        .workspace
                        .as_ref()
                        .map_or(tapid_lockfile::OmissionOrigin::Root, |key| {
                            tapid_lockfile::OmissionOrigin::Workspace { key: key.clone() }
                        }),
                    name: dependency.name.to_string(),
                    requirement: dependency.requirement.raw.clone(),
                    registry: dependency.registry.to_string(),
                    target: None,
                    reason,
                });
            } else {
                seed.extend(candidates);
            }
        }
        entry_points.retain(|(dependency, entry)| {
            !absent.contains(&(
                entry.clone(),
                dependency.registry.clone(),
                dependency.name.clone(),
            ))
        });
        roots = entry_points
            .iter()
            .map(|(dependency, _)| dependency.clone())
            .collect();
        workspace_registry_dependencies.retain(|dependency| {
            !omissions.iter().any(|omission| {
                omission.origin
                    == tapid_lockfile::OmissionOrigin::Workspace {
                        key: dependency.member_key.clone(),
                    }
                    && omission.name == dependency.manifest_name
            })
        });
        root_registry_identities = entry_points
            .iter()
            .filter(|(_, entry)| entry.workspace.is_none())
            .map(|(dependency, _)| {
                (
                    dependency.registry.clone(),
                    dependency
                        .requirement
                        .package_name(&dependency.name)
                        .clone(),
                )
            })
            .collect();
    }
    resolving.set(true);
    let (mut resolution, records, candidate_exclusions) = resolution::resolve_with_preferences(
        &roots,
        |parent, dependency| registry_config.registry_for_dependency(parent, dependency),
        &overrides,
        &mut fetch_records,
        &preferred,
        seed,
        |fetched| progress(crate::application::install::Progress::Metadata(fetched)),
    )
    .map_err(|mut error| {
        if let Some(context) = &mut error.diagnostic {
            context.locate_requirements(&declarations);
            resolution::locate_override_requirements(
                context,
                &declarations,
                manifest,
                &project.join("package.json"),
                &overrides,
            );
        }
        error
    })?;
    // Reattach manifest facts captured before root constraint deduplication.
    let entry_bindings: BTreeSet<_> = entry_points
        .iter()
        .map(|(dependency, _)| (dependency.registry.clone(), dependency.name.clone()))
        .collect();
    resolution.declarations.retain(|edge| {
        edge.parent.is_some()
            || !entry_bindings.contains(&(edge.target.registry.clone(), edge.name.clone()))
    });
    for (dependency, entry_point) in entry_points {
        let target = resolution
            .root_bindings
            .get(&(dependency.registry, dependency.name.clone()))
            .ok_or_else(|| {
                OperationalError::new(
                    ErrorKind::Resolution,
                    "missing selected entry-point binding",
                )
            })?;
        resolution
            .declarations
            .push(tapid_resolver::ResolvedDeclaration {
                parent: None,
                entry_point: Some(entry_point.clone()),
                name: dependency.name,
                requirement: dependency.requirement,
                target: target.clone(),
                kind: if entry_point.section
                    == tapid_resolver::EntryPointSection::OptionalDependencies
                {
                    tapid_resolver::DeclarationKind::Optional
                } else {
                    tapid_resolver::DeclarationKind::Required
                },
            });
    }
    resolution.declarations.sort();
    resolving.set(false);
    validate_workspace_peer_providers(
        &workspace_peer_dependencies,
        &root_registry_identities,
        &resolution,
        &records,
    )?;

    let updated_manifest = update
        .map(|request| {
            update::saved_manifest(
                project,
                request,
                registry_config,
                &resolution,
                &mut fetch_records,
            )
        })
        .transpose()?;
    Ok(ResolvedGraph {
        workspace_manifests,
        omissions,
        candidate_exclusions,
        updated_manifest,
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
    let (_, local) = dep_parts(&dependency.manifest_name)?;
    resolution.root_bindings.get(&(dependency.registry.clone(), local))
        .filter(|id| id.name == dependency.package && records.get(&(id.registry.to_string(), id.name.to_string(), id.version.to_string()))
            .is_some_and(|record| dependency.requirement.matches_tagged_version(&id.version, &record.dist_tags)))
        .ok_or_else(|| OperationalError::new(ErrorKind::Resolution, format!(
            "no selected registry package satisfies workspace member dependency '{}' ({:?})",
            dependency.manifest_name, dependency.requirement
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::lifecycle::{SavePolicy, plan_update};

    #[test]
    fn root_workspace_overlap_retains_required_and_optional_entry_points() {
        for (workspace_required, optional_only) in [(false, false), (true, false), (true, true)] {
            let project = tapid_test_support::TempProject::new("entry-overlap").unwrap();
            let root = if workspace_required {
                r#"{"name":"app","version":"1.0.0","workspaces":["member"],"optionalDependencies":{"shared":"1"}}"#
            } else {
                r#"{"name":"app","version":"1.0.0","dependencies":{"shared":"1"},"optionalDependencies":{"shared":"1"}}"#
            };
            project.write("package.json", root.as_bytes()).unwrap();
            if workspace_required {
                let member = if optional_only {
                    r#"{"name":"member","version":"1.0.0","optionalDependencies":{"shared":"1"}}"#
                } else {
                    r#"{"name":"member","version":"1.0.0","dependencies":{"shared":"1"}}"#
                };
                project
                    .write("member/package.json", member.as_bytes())
                    .unwrap();
            }
            let fixture = serde_json::json!({"packages":[{"registry":NPM,"name":"shared","version":"1.0.0","integrity":integrity(b"shared").to_string(),"artifact":"never-download"}]});
            let fixture = project
                .write("registry.json", fixture.to_string().as_bytes())
                .unwrap();
            let manifest = PackageManifest::parse(root).unwrap();
            let config = crate::registry::RegistryConfig::default();
            let mut graph = resolve_package_graph(
                project.path(),
                &manifest,
                workspace_materialization(project.path(), &config).unwrap(),
                Some(&fixture),
                false,
                &config,
                None,
                None,
                |_| {},
            )
            .unwrap();
            let entries = &graph.resolution.declarations;
            assert_eq!(entries.len(), 2);
            assert_eq!(
                entries
                    .iter()
                    .filter(|edge| edge.kind == tapid_resolver::DeclarationKind::Optional)
                    .count(),
                if optional_only { 2 } else { 1 }
            );
            assert!(entries.iter().all(|edge| edge.entry_point.is_some()));
            let failed = graph.resolution.selected.iter().cloned().collect();
            assert_eq!(
                entries
                    .iter()
                    .filter(|edge| edge.entry_point.as_ref().unwrap().workspace.is_some())
                    .count(),
                usize::from(workspace_required)
            );
            for reverse in [false, true] {
                if reverse {
                    graph.resolution.declarations.reverse();
                }
                let result = graph.resolution.prune_unavailable(&failed);
                if optional_only {
                    let pruned = result.unwrap();
                    assert!(pruned.resolution.selected.is_empty());
                    assert_eq!(pruned.cuts.len(), 2);
                } else {
                    assert!(matches!(
                        result,
                        Err(tapid_resolver::PruneError::RequiredDeclaration(_))
                    ));
                }
            }
            assert!(graph.resolution.prune_unavailable(&BTreeSet::new()).is_ok());
        }
    }

    #[test]
    fn peer_only_save_reuses_metadata_retrieved_for_a_transitive_dependency() {
        let project = tapid_test_support::TempProject::new("graph-peer-save-metadata").unwrap();
        let text = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1"},"peerDependencies":{"child":"^1"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        let fixture = serde_json::json!({"packages":[
            {"registry":NPM,"name":"parent","version":"1.0.0","dependencies":{"child":"^1"},"integrity":integrity(b"parent").to_string(),"artifact":"never-download"},
            {"registry":NPM,"name":"child","version":"1.1.0","integrity":integrity(b"child").to_string(),"artifact":"never-download"}
        ]});
        let fixture = project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let manifest = PackageManifest::parse(text).unwrap();
        let plan = plan_update(&manifest, &["child".into()], false).unwrap();
        let config = crate::registry::RegistryConfig::default();
        let target = project.path().join("package.json");
        let resolved = resolve_package_graph(
            project.path(),
            &manifest,
            workspace_materialization(project.path(), &config).unwrap(),
            Some(&fixture),
            false,
            &config,
            None,
            Some(UpdateRequest {
                plan: &plan,
                target: &target,
                named: true,
                original_root_digest: None,
                save: Some(SavePolicy::Exact),
            }),
            |_| {},
        )
        .unwrap();
        assert_eq!(resolved.resolution.selected.len(), 2);
        assert_eq!(resolved.metadata_lookups, 2);
        let saved = resolved.updated_manifest.unwrap();
        assert_eq!(saved.dependencies()["parent"], "^1");
        assert_eq!(saved.peer_dependencies()["child"], "1.1.0");
    }

    #[test]
    fn named_update_keeps_metadata_from_distinct_sources_separate() {
        let project = tapid_test_support::TempProject::new("graph-metadata-sources").unwrap();
        let text = r#"{"name":"app","version":"1.0.0","dependencies":{"local":"npm:@s/foo@^1","jsr:@s/foo":"^1"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        let fixture = serde_json::json!({"packages":[
            {"registry": NPM, "name":"@s/foo", "version":"1.1.0", "integrity":integrity(b"npm").to_string(), "artifact":"never-download"},
            {"registry": JSR, "name":"@s/foo", "version":"1.2.0", "integrity":integrity(b"jsr").to_string(), "artifact":"never-download"}
        ]});
        let fixture = project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let manifest = PackageManifest::parse(text).unwrap();
        let plan = plan_update(&manifest, &["local".into(), "jsr:@s/foo".into()], false).unwrap();
        let config = crate::registry::RegistryConfig::default();
        let target = project.path().join("package.json");
        let resolved = resolve_package_graph(
            project.path(),
            &manifest,
            workspace_materialization(project.path(), &config).unwrap(),
            Some(&fixture),
            false,
            &config,
            None,
            Some(UpdateRequest {
                plan: &plan,
                target: &target,
                named: true,
                original_root_digest: None,
                save: Some(SavePolicy::Exact),
            }),
            |_| {},
        )
        .unwrap();
        assert_eq!(resolved.metadata_lookups, 2);
        assert_eq!(resolved.resolution.selected.len(), 2);
        for (source, version) in [(NPM, "1.1.0"), (JSR, "1.2.0")] {
            assert!(
                resolved
                    .resolution
                    .selected
                    .iter()
                    .any(|id| id.registry.to_string() == source
                        && id.name.as_str() == "@s/foo"
                        && id.version.to_string() == version)
            );
        }
        let saved = resolved.updated_manifest.unwrap();
        assert_eq!(saved.dependencies()["local"], "npm:@s/foo@1.1.0");
        assert_eq!(saved.dependencies()["jsr:@s/foo"], "1.2.0");
    }

    #[test]
    fn named_update_retrieves_tagged_workspace_peer_metadata_once() {
        let project = tapid_test_support::TempProject::new("graph-metadata-reuse").unwrap();
        let manifest_text = r#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"host":"^1.0.0"},"peerDependencies":{"host":"latest"}}"#;
        project
            .write("package.json", manifest_text.as_bytes())
            .unwrap();
        project
            .write(
                "packages/plugin/package.json",
                br#"{"name":"plugin","version":"1.0.0","peerDependencies":{"host":"latest"}}"#,
            )
            .unwrap();
        let registry_fixture = serde_json::json!({"packages":[{
            "registry": NPM, "name": "host", "version": "1.1.0",
            "dist_tags": ["latest"], "integrity": integrity(b"fixture").to_string(),
            "artifact": "never-download"
        }]});
        project
            .write("registry.json", registry_fixture.to_string().as_bytes())
            .unwrap();
        let manifest = PackageManifest::parse(manifest_text).unwrap();
        let plan = plan_update(&manifest, &["host".into()], false).unwrap();
        let config = crate::registry::RegistryConfig::default();
        let target = project.path().join("package.json");
        let fixture_path = project.path().join("registry.json");
        let resolved = resolve_package_graph(
            project.path(),
            &plan.manifest,
            workspace_materialization(project.path(), &config).unwrap(),
            Some(&fixture_path),
            false,
            &config,
            None,
            Some(UpdateRequest {
                plan: &plan,
                target: &target,
                named: true,
                original_root_digest: None,
                save: Some(SavePolicy::Exact),
            }),
            |_| {},
        )
        .unwrap();

        assert_eq!(resolved.resolution.selected.len(), 1);
        let provider = &resolved.resolution.root_bindings[&(
            NPM.parse::<PackageSource>().unwrap(),
            "host".parse().unwrap(),
        )];
        assert_eq!(provider.name.as_str(), "host");
        assert_eq!(provider.version.to_string(), "1.1.0");
        let saved = resolved.updated_manifest.unwrap();
        assert_eq!(saved.dependencies()["host"], "1.1.0");
        assert_eq!(saved.peer_dependencies()["host"], "1.1.0");
        assert_eq!(
            resolved.metadata_lookups, 1,
            "one provider serves root update, workspace peer validation, and saved declarations"
        );
        for path in ["tapid.lock", "node_modules", ".tapid-activation.lock"] {
            assert!(!project.path().join(path).exists(), "{path}");
        }
    }
}
