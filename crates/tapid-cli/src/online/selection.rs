//! Complete metadata selections and separate profile-scoped source preparation.
use super::*;
use tapid_lockfile::{
    NativeDeclaration, NativeDeclarationKind, NativeDependency, NativeDependencyKind,
    NativeSelectedPackage, NativeSelection, NativeSourceEvidence,
};

// Keep unsupported activation contracts ahead of archive access and store
// recovery, not only ahead of linker planning. Complete selection production
// remains available independently of materialization.
fn validate_native_profile_layout(selection: &NativeSelection) -> Result<(), OperationalError> {
    if selection.packages().values().any(|package| {
        matches!(
            package.source_evidence,
            NativeSourceEvidence::Workspace { .. }
        )
    }) || selection
        .materialized_declarations(tapid_lockfile::InstallProfile::default())
        .any(|edge| edge.workspace.is_some())
    {
        return Err(OperationalError::new(
            ErrorKind::Materialization,
            "native profile workspace layout integration is required",
        ));
    }
    Ok(())
}

/// Complete selection and separately verified materialized sources. This is not a
/// legacy lock and cannot be activated without layout/lifecycle preparation.
#[allow(dead_code)]
pub(crate) struct PreparedNativeProfile {
    pub(crate) selection: NativeSelection,
    pub(super) materialized_keys: BTreeSet<String>,
    pub(super) sources: BTreeMap<String, PreparedNativeSource>,
    pub(crate) transaction: StoreTransaction,
}

impl PreparedNativeProfile {
    /// A verified, invocation-local graph for lifecycle discovery. Never serialize
    /// this materialized view over the complete accepted selection.
    pub(crate) fn lifecycle_sources(
        &self,
        root_digest: &str,
    ) -> Result<Lockfile, OperationalError> {
        let mut lock = Lockfile::new(root_digest)?;
        let profile = tapid_lockfile::InstallProfile::default();
        let mut packages = Vec::new();
        for (encoded, source) in &self.sources {
            let key: LockfilePackageKey = encoded.parse()?;
            let mut package = LockedPackage::new_with_context_and_provenance(
                key.source
                    .package_source()
                    .ok_or("expected artifact source")?
                    .as_str(),
                key.name.as_str(),
                &key.version.to_string(),
                &source.integrity.to_string(),
                &source.digest.to_string(),
                (&key.peer_context()?, &key.platform_context()?),
                tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
            )?;
            for (name, target) in self.selection.materialized_dependencies(encoded, profile)? {
                package.add_alias_dependency(name, target)?;
            }
            packages.push(package);
        }
        lock.insert_packages(packages)?;
        lock.set_roots(
            self.selection
                .materialized_declarations(profile)
                .map(|edge| &edge.target),
        )?;
        lock.set_root_bindings(
            self.selection
                .materialized_declarations(profile)
                .map(|edge| (edge.name.clone(), edge.target.clone()))
                .collect(),
        )?;
        Ok(lock)
    }
    /// Build linker inputs from verified sources without converting the complete
    /// selection into a legacy lock. Lifecycle preparation still precedes activation.
    #[allow(dead_code)]
    pub(crate) fn layout(
        &self,
        profile: tapid_lockfile::InstallProfile,
    ) -> Result<PreparedLayout, OperationalError> {
        let retained = self.selection.materialized_keys(profile)?;
        if retained != self.materialized_keys {
            return Err(OperationalError::new(
                ErrorKind::Materialization,
                "prepared sources do not match requested profile",
            ));
        }
        // Workspace paths must be planned with member generations and local links;
        // never pass them through the registry-only source layout.
        validate_native_profile_layout(&self.selection)?;
        if self.sources.keys().cloned().collect::<BTreeSet<_>>() != retained {
            return Err(OperationalError::new(
                ErrorKind::Materialization,
                "native profile requires every retained verified source",
            ));
        }
        let mut instances = Vec::new();
        let mut keys = BTreeMap::new();
        let mut trees = BTreeMap::new();
        for encoded in &retained {
            let identity: LockfilePackageKey = encoded.parse()?;
            let source = &self.sources[encoded];
            let instance = PackageInstance {
                id: tapid_core::PackageInstanceId::from_source(
                    identity
                        .source
                        .package_source()
                        .ok_or("expected native artifact source")?,
                    identity.name.clone(),
                    identity.version.clone(),
                ),
                peer_context: identity.peer_context()?,
                platform_context: identity.platform_context()?,
                tree: VerifiedTreeReference::new(&source.digest.to_string(), &source.tree)
                    .map_err(|error| {
                        OperationalError::from_source(ErrorKind::Materialization, error)
                    })?,
            };
            keys.insert(encoded.clone(), InstanceKey::from(&instance));
            trees.insert(encoded.clone(), source.tree.clone());
            instances.push(instance);
        }
        let binding = |name: &str, target: &str| -> Result<NamedDependency, OperationalError> {
            Ok(NamedDependency {
                name: name
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?,
                child: keys
                    .get(target)
                    .cloned()
                    .ok_or_else(|| format!("missing prepared target {target}"))?,
            })
        };
        let root_dependencies = self
            .selection
            .materialized_declarations(profile)
            .map(|edge| binding(&edge.name, &edge.target))
            .collect::<Result<Vec<_>, _>>()?;
        let mut dependency_edges = Vec::new();
        for parent in &retained {
            for (name, target) in self.selection.materialized_dependencies(parent, profile)? {
                dependency_edges.push(NamedDependencyEdge {
                    parent: keys[parent].clone(),
                    dependency: binding(name, target)?,
                });
            }
        }
        Ok(PreparedLayout {
            input: NamedLayoutInput {
                instances,
                root_dependencies,
                dependency_edges,
            },
            trees,
            workspace: WorkspaceInstallPlan::default(),
        })
    }
}

#[allow(dead_code)]
pub(super) struct PreparedNativeSource {
    pub(super) integrity: PackageIntegrity,
    pub(super) digest: ArtifactDigest,
    pub(super) tree: PathBuf,
}

/// Replay source verification without modifying the complete accepted selection.
/// The fetch boundary must provide the exact pinned archive (including offline cache
/// lookup); mutable installation receipts are deliberately not accepted here.
#[allow(dead_code)]
pub(super) fn prepare_native_replay(
    selection: &NativeSelection,
    config: &crate::registry::RegistryConfig,
    store: &Store,
    profile: tapid_lockfile::InstallProfile,
    mut fetch: impl FnMut(&LockfilePackageKey, &str) -> Result<Vec<u8>, OperationalError>,
) -> Result<PreparedNativeProfile, OperationalError> {
    validate_native_profile_layout(selection)?;
    let materialized_keys = selection.materialized_keys(profile)?;
    // Check all identities before archive access or store mutation, even omitted ones.
    for key in selection.packages().keys() {
        let key: LockfilePackageKey = key.parse()?;
        if !locked::locked_platform_matches(&key)? {
            return Err(OperationalError::new(
                ErrorKind::Lockfile,
                "native selection target differs from current platform",
            ));
        }
        if let Some(registry) = key.source.registry()
            && registry.as_str() != JSR
            && config.origin_for_name(&key.name)? != *registry
        {
            return Err(OperationalError::new(
                ErrorKind::RegistryConfiguration,
                "native selection registry route differs",
            ));
        }
    }
    store.recover_transactions()?;
    fs::create_dir_all(store.root())
        .map_err(|error| OperationalError::from_source(ErrorKind::Store, error))?;
    let mut transaction = store.transaction();
    let mut fetcher = artifact::ArtifactFetcher {
        store,
        config,
        transports: BTreeMap::new(),
        allowed_origins: config.configured_origins(),
    };
    let mut sources = BTreeMap::new();
    for key in &materialized_keys {
        let identity: LockfilePackageKey = key.parse()?;
        let evidence = &selection.packages()[key].source_evidence;
        let (url, expected, pinned) = match evidence {
            NativeSourceEvidence::Pending {
                artifact_url,
                artifact_integrity,
            } => (artifact_url.as_str(), artifact_integrity.as_str(), None),
            NativeSourceEvidence::Verified { package } => (
                package
                    .artifact_url()
                    .ok_or("native replay requires pinned archive URL")?,
                package.artifact_integrity(),
                Some(package.as_ref()),
            ),
            NativeSourceEvidence::Workspace { .. } => continue,
        };
        let record = PackageRecord {
            complete_dependency_kinds: true,
            git_reference: None,
            copied_archive: None,
            dist_tags: BTreeSet::new(),
            registry: identity
                .source
                .package_source()
                .ok_or("expected artifact source")?,
            name: identity.name.clone(),
            version: identity.version.clone(),
            integrity: Some(
                expected
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?,
            ),
            artifact: url.to_owned(),
            dependencies: BTreeMap::new(),
            peer_dependencies: BTreeMap::new(),
            optional_peer_dependencies: BTreeSet::new(),
            optional_dependencies: BTreeMap::new(),
            platform: PackagePlatform::unrestricted(),
            fixture: false,
        };
        let id = tapid_registry_client::RegistryPackageId {
            registry: record.registry.clone(),
            name: record.name.clone(),
            version: record.version.clone(),
        };
        let (integrity, digest, tree) =
            fetcher.prepare_with_archive(&id, &record, pinned, &mut transaction, || {
                fetch(&identity, url).map(Some)
            })?;
        validate_native_source_identity(&id, &tree)?;
        sources.insert(
            key.clone(),
            PreparedNativeSource {
                integrity,
                digest,
                tree,
            },
        );
    }
    Ok(PreparedNativeProfile {
        selection: selection.clone(),
        materialized_keys,
        sources,
        transaction,
    })
}

fn validate_native_source_identity(
    id: &tapid_registry_client::RegistryPackageId,
    tree: &Path,
) -> Result<(), OperationalError> {
    let root = crate::filesystem::tree::package_content_root(tree)?;
    let bytes = fs::read_to_string(root.join("package.json"))
        .map_err(|error| OperationalError::from_source(ErrorKind::Archive, error))?;
    let manifest = PackageManifest::parse(&bytes)
        .map_err(|error| OperationalError::from_source(ErrorKind::Archive, error))?;
    if manifest.name() != &id.name || manifest.version() != id.version {
        return Err(OperationalError::new(
            ErrorKind::Integrity,
            "native archive package identity mismatch",
        )
        .for_package(id.into()));
    }
    Ok(())
}

impl graph::ResolvedGraph {
    /// Validate the entire selection, then fetch only its enabled closure. Source
    /// receipts remain separate so omitted selections never need invented trees.
    #[allow(dead_code)]
    pub(super) fn prepare_native_profile(
        &self,
        project: &Path,
        manifest: &PackageManifest,
        config: &crate::registry::RegistryConfig,
        store: &Store,
        profile: tapid_lockfile::InstallProfile,
    ) -> Result<PreparedNativeProfile, OperationalError> {
        let selection = self.native_selection(project, manifest, config)?;
        validate_native_profile_layout(&selection)?;
        let materialized_keys = selection.materialized_keys(profile)?;
        store.recover_transactions()?;
        fs::create_dir_all(store.root())
            .map_err(|error| OperationalError::from_source(ErrorKind::Store, error))?;
        let mut transaction = store.transaction();
        let mut fetcher = artifact::ArtifactFetcher {
            store,
            config,
            transports: BTreeMap::new(),
            allowed_origins: config.configured_origins(),
        };
        let mut sources = BTreeMap::new();
        for id in &self.resolution.selected {
            let key = self.native_package_key(id)?;
            if !materialized_keys.contains(&key) {
                continue;
            }
            let record = &self.records[&(
                id.registry.to_string(),
                id.name.to_string(),
                id.version.to_string(),
            )];
            let (integrity, digest, tree) = fetcher
                .prepare(id, record, None, &mut transaction)
                .map_err(|error| error.for_package(id.into()))?;
            validate_native_source_identity(id, &tree)?;
            sources.insert(
                key,
                PreparedNativeSource {
                    integrity,
                    digest,
                    tree,
                },
            );
        }
        Ok(PreparedNativeProfile {
            selection,
            materialized_keys,
            sources,
            transaction,
        })
    }

    fn native_package_key(
        &self,
        id: &tapid_registry_client::RegistryPackageId,
    ) -> Result<String, OperationalError> {
        let record = &self.records[&(
            id.registry.to_string(),
            id.name.to_string(),
            id.version.to_string(),
        )];
        let platform = record
            .platform
            .selected_context(std::env::consts::OS, std::env::consts::ARCH, current_libc())
            .map_err(|error| OperationalError::new(ErrorKind::Resolution, error))?;
        let peer = self
            .resolution
            .peer_contexts
            .get(id)
            .cloned()
            .unwrap_or_default();
        Ok(LockfilePackageKey::from_source(
            id.registry.clone(),
            id.name.clone(),
            id.version.clone(),
            &peer,
            &platform,
        )
        .to_string())
    }

    // Kept internal until install/replay can consume pending evidence atomically.
    #[allow(dead_code)]
    pub(super) fn native_selection(
        &self,
        project: &Path,
        manifest: &PackageManifest,
        config: &crate::registry::RegistryConfig,
    ) -> Result<NativeSelection, OperationalError> {
        let invalid = |message: &str| OperationalError::new(ErrorKind::Resolution, message);
        if self
            .workspace_manifests
            .values()
            .any(|manifest| !manifest.peer_dependencies().is_empty())
        {
            return Err(invalid(
                "native selection producer does not yet support workspace peer evidence",
            ));
        }
        let mut keys = BTreeMap::new();
        let mut packages = BTreeMap::new();
        for id in &self.resolution.selected {
            let record = &self.records[&(
                id.registry.to_string(),
                id.name.to_string(),
                id.version.to_string(),
            )];
            if !record.complete_dependency_kinds {
                return Err(invalid(
                    "legacy metadata lacks complete dependency kind evidence",
                ));
            }
            if id.registry.registry().is_none() {
                return Err(invalid(
                    "pending native selections require registry sources",
                ));
            }
            let key = self.native_package_key(id)?;
            packages.insert(
                key.clone(),
                NativeSelectedPackage {
                    source_evidence: NativeSourceEvidence::Pending {
                        artifact_url: record.artifact.clone(),
                        artifact_integrity: record
                            .integrity
                            .as_ref()
                            .ok_or_else(|| {
                                invalid("pending native selection requires declared integrity")
                            })?
                            .to_string(),
                    },
                    dependencies: BTreeMap::new(),
                    peer_providers: BTreeMap::new(),
                },
            );
            keys.insert(id.clone(), key);
        }
        for edge in &self.resolution.dependencies {
            let record = &self.records[&(
                edge.parent.registry.to_string(),
                edge.parent.name.to_string(),
                edge.parent.version.to_string(),
            )];
            let kind = if record
                .optional_dependencies
                .contains_key(edge.dependency.as_str())
            {
                NativeDependencyKind::Optional
            } else if record.dependencies.contains_key(edge.dependency.as_str()) {
                NativeDependencyKind::Required
            } else {
                return Err(invalid(
                    "selected dependency lacks declaration kind evidence",
                ));
            };
            packages
                .get_mut(&keys[&edge.parent])
                .expect("selected parent")
                .dependencies
                .insert(
                    edge.dependency.to_string(),
                    NativeDependency {
                        kind,
                        target: keys[&edge.child].clone(),
                    },
                );
        }
        for (id, context) in &self.resolution.peer_contexts {
            for (name, version) in context.entries() {
                // Native peers are supported only through accepted direct-root providers.
                // A version match alone is insufficient: preserve the routed exact binding.
                let registry = config.registry_for_dependency(&id.registry, name)?;
                let provider = self
                    .resolution
                    .root_bindings
                    .get(&(registry, name.clone()))
                    .filter(|provider| provider.name == *name && provider.version == *version)
                    .ok_or_else(|| invalid("missing exact direct-root peer provider"))?;
                packages
                    .get_mut(&keys[id])
                    .expect("selected consumer")
                    .peer_providers
                    .insert(name.to_string(), keys[provider].clone());
            }
        }
        let mut workspace_digests = BTreeMap::new();
        let mut workspace_targets = BTreeMap::new();
        for member in &self.workspace_locked {
            let key = member.key();
            let identity: LockfilePackageKey = key.parse()?;
            let source = identity
                .source
                .workspace()
                .ok_or_else(|| invalid("missing workspace source"))?
                .clone();
            workspace_digests.insert(
                source.path().to_owned(),
                member.manifest_digest().to_owned(),
            );
            workspace_targets.insert(identity.name.to_string(), key.clone());
            packages.insert(
                key,
                NativeSelectedPackage {
                    source_evidence: NativeSourceEvidence::Workspace { source },
                    dependencies: BTreeMap::new(),
                    peer_providers: BTreeMap::new(),
                },
            );
        }
        let mut declarations = Vec::new();
        for (workspace, manifest) in std::iter::once((None, manifest)).chain(
            self.workspace_manifests
                .iter()
                .map(|(path, manifest)| (Some(path.clone()), manifest)),
        ) {
            for (kind, name, range) in manifest.effective_install_declarations() {
                let (_, local) = dep_parts(name)?;
                let target = if let Some(key) = workspace_targets.get(name) {
                    key.clone()
                } else {
                    let requirement = range
                        .parse::<Requirement>()
                        .map_err(|error| invalid(&error.to_string()))?;
                    let registry = registry_for_requirement(config, name, &requirement)?;
                    let target = self
                        .resolution
                        .root_bindings
                        .get(&(registry, local.clone()))
                        .ok_or_else(|| invalid("missing accepted declaration binding"))?;
                    keys[target].clone()
                };
                let kind = match kind {
                    tapid_manifest::DependencyKind::Dependencies => NativeDeclarationKind::Required,
                    tapid_manifest::DependencyKind::DevDependencies => NativeDeclarationKind::Dev,
                    tapid_manifest::DependencyKind::OptionalDependencies => {
                        NativeDeclarationKind::Optional
                    }
                    tapid_manifest::DependencyKind::PeerDependencies => {
                        unreachable!("effective install declaration")
                    }
                };
                declarations.push(NativeDeclaration {
                    workspace: workspace.clone(),
                    name: local.to_string(),
                    kind,
                    target,
                });
            }
        }
        NativeSelection::new(
            "0".to_owned(),
            "0".to_owned(),
            root_digest(project)?,
            workspace_digests,
            declarations,
            packages,
        )
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_preparation_verifies_retained_sources_without_publishing() {
        let project = tapid_test_support::TempProject::new("profile-retained-source").unwrap();
        let text = r#"{"name":"app","version":"1.0.0","dependencies":{"shared":"1"},"devDependencies":{"tool":"1"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        let archives: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/npm-import/registry.json"
        ))
        .unwrap();
        let archive = archives["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|package| package["name"] == "shared" && package["version"] == "1.0.0")
            .unwrap();
        let bytes = STANDARD
            .decode(
                archive["artifact"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64:")
                    .unwrap(),
            )
            .unwrap();
        let fixture = serde_json::json!({"packages":[
            {"registry":NPM,"name":"shared","version":"1.0.0", "integrity":integrity(&bytes).to_string(),"artifact":"https://registry.npmjs.org/shared.tgz"},
            {"registry":NPM,"name":"tool","version":"1.0.0", "integrity":integrity(b"missing").to_string(),"artifact":"https://registry.npmjs.org/tool.tgz"}
        ]});
        let fixture = project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let manifest = PackageManifest::parse(text).unwrap();
        let config = crate::registry::RegistryConfig::default();
        let mut graph = graph::resolve_package_graph(
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
        // Supply deterministic bytes through the existing in-memory archive seam;
        // the complete selection still records the canonical registry archive pin.
        let record_key = (NPM.to_owned(), "shared".to_owned(), "1.0.0".to_owned());
        graph.records.get_mut(&record_key).unwrap().copied_archive =
            Some(std::sync::Arc::new(bytes));
        let store = Store::new(project.path().join("store"));
        let prepared = graph
            .prepare_native_profile(
                project.path(),
                &manifest,
                &config,
                &store,
                tapid_lockfile::InstallProfile::production(),
            )
            .unwrap();
        assert_eq!(prepared.sources.len(), 1);
        assert_eq!(prepared.materialized_keys.len(), 1);
        let (key, receipt) = prepared.sources.first_key_value().unwrap();
        assert_eq!(
            key.parse::<LockfilePackageKey>().unwrap().name.as_str(),
            "shared"
        );
        assert!(receipt.tree.join("package/package.json").is_file());
        assert_eq!(
            canonical_tree_digest(&receipt.tree).unwrap(),
            receipt.digest.to_string()
        );
        assert_eq!(
            receipt.integrity.to_string(),
            archive["integrity"].as_str().unwrap()
        );
        assert!(matches!(
            store.verified_tree_snapshot(&receipt.digest),
            Err(tapid_store::IngestError::TreeMissing(_))
        ));
        let layout = prepared
            .layout(tapid_lockfile::InstallProfile::production())
            .unwrap();
        assert_eq!(layout.input.instances.len(), 1);
        assert_eq!(layout.input.root_dependencies.len(), 1);
        assert_eq!(layout.input.root_dependencies[0].name.as_str(), "shared");
        assert_eq!(prepared.selection.packages().len(), 2);
        let activation =
            crate::filesystem::activation::ActivationLock::acquire(project.path()).unwrap();
        let stage = crate::application::install::stage_native_profile(
            project.path(),
            &prepared,
            tapid_lockfile::InstallProfile::production(),
            &activation,
            |_| {},
        )
        .unwrap();
        assert!(stage.join("node_modules/shared/package.json").is_file());
        assert!(!stage.join("node_modules/tool").exists());
        crate::filesystem::installed_profile::validate_profile(
            &stage,
            &prepared.selection,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            activation.owner_line(),
        )
        .unwrap();
        fs::remove_dir_all(stage).unwrap();
        assert!(
            crate::application::install::stage_native_profile(
                project.path(),
                &prepared,
                tapid_lockfile::InstallProfile::default(),
                &activation,
                |_| {},
            )
            .is_err()
        );
        fs::write(
            receipt.tree.join("package/package.json"),
            b"tampered after preparation",
        )
        .unwrap();
        let error = crate::application::install::stage_native_profile(
            project.path(),
            &prepared,
            tapid_lockfile::InstallProfile::production(),
            &activation,
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("tree changed"), "{error}");
        assert!(!project.path().join("node_modules").exists());
        drop(activation);
        let selection = prepared.selection.clone();
        let lock_bytes = selection.to_json().unwrap();
        project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
        drop(prepared);
        let mut fetches = 0;
        let replayed = prepare_native_replay(
            &selection,
            &config,
            &store,
            tapid_lockfile::InstallProfile::production(),
            |key, url| {
                fetches += 1;
                assert_eq!(key.name.as_str(), "shared");
                assert_eq!(url, "https://registry.npmjs.org/shared.tgz");
                Ok(graph.records[&record_key]
                    .copied_archive
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .clone())
            },
        )
        .unwrap();
        assert_eq!(fetches, 1);
        assert_eq!(replayed.sources.len(), 1);
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes.as_bytes()
        );
        drop(replayed);
        let error = prepare_native_replay(
            &selection,
            &config,
            &store,
            tapid_lockfile::InstallProfile::production(),
            |_, _| Ok(b"forged receipt cannot authorize this archive".to_vec()),
        )
        .err()
        .expect("replay must reverify the pinned archive");
        assert!(error.to_string().contains("integrity mismatch"), "{error}");
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes.as_bytes()
        );
        let mut wire: serde_json::Value = serde_json::from_str(&lock_bytes).unwrap();
        let old_key = selection
            .materialized_keys(tapid_lockfile::InstallProfile::production())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let new_key = old_key.replace("shared", "impostor");
        let package = wire["packages"]
            .as_object_mut()
            .unwrap()
            .remove(&old_key)
            .unwrap();
        wire["packages"]
            .as_object_mut()
            .unwrap()
            .insert(new_key.clone(), package);
        for declaration in wire["declarations"].as_array_mut().unwrap() {
            if declaration["target"] == old_key {
                declaration["target"] = new_key.clone().into();
            }
        }
        let wrong_identity = NativeSelection::from_json(&wire.to_string()).unwrap();
        let error = prepare_native_replay(
            &wrong_identity,
            &config,
            &store,
            tapid_lockfile::InstallProfile::production(),
            |_, _| {
                Ok(graph.records[&record_key]
                    .copied_archive
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .clone())
            },
        )
        .err()
        .expect("integrity alone cannot authorize a different package identity");
        assert!(error.to_string().contains("identity mismatch"), "{error}");
        graph.records.get_mut(&record_key).unwrap().copied_archive =
            Some(std::sync::Arc::new(b"tampered".to_vec()));
        let error = graph
            .prepare_native_profile(
                project.path(),
                &manifest,
                &config,
                &store,
                tapid_lockfile::InstallProfile::production(),
            )
            .err()
            .expect("retained integrity failure must fail closed");
        assert!(error.to_string().contains("integrity mismatch"), "{error}");
        assert!(!project.path().join("node_modules").exists());
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes.as_bytes()
        );
    }

    #[test]
    fn native_verified_replay_reuses_validated_store_without_archive_access() {
        let project = tapid_test_support::TempProject::new("native-verified-offline").unwrap();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/npm-import/registry.json"
        ))
        .unwrap();
        let archive = fixture["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|package| package["name"] == "shared" && package["version"] == "1.0.0")
            .unwrap();
        let bytes = STANDARD
            .decode(
                archive["artifact"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64:")
                    .unwrap(),
            )
            .unwrap();
        let key = LockfilePackageKey::new(
            NPM.parse().unwrap(),
            "shared".parse().unwrap(),
            "1.0.0".parse().unwrap(),
            &Default::default(),
            &tapid_core::PlatformContext::new(None, None, None).unwrap(),
        )
        .to_string();
        let url = format!("{NPM}/shared.tgz");
        let mut packages = BTreeMap::new();
        packages.insert(
            key.clone(),
            NativeSelectedPackage {
                source_evidence: NativeSourceEvidence::Pending {
                    artifact_url: url.clone(),
                    artifact_integrity: archive["integrity"].as_str().unwrap().into(),
                },
                dependencies: BTreeMap::new(),
                peer_providers: BTreeMap::new(),
            },
        );
        let pending = NativeSelection::new(
            "0".into(),
            "0".into(),
            format!("sha256-{}", "a".repeat(64)),
            BTreeMap::new(),
            vec![NativeDeclaration {
                workspace: None,
                name: "shared".into(),
                kind: NativeDeclarationKind::Required,
                target: key.clone(),
            }],
            packages,
        )
        .unwrap();
        let store = Store::new(project.path().join("store"));
        let config = crate::registry::RegistryConfig::default();
        let full = tapid_lockfile::InstallProfile::default();
        let prepared =
            prepare_native_replay(&pending, &config, &store, full, |_, _| Ok(bytes.clone()))
                .unwrap();
        let source = &prepared.sources[&key];
        let mut locked = LockedPackage::new_with_provenance(
            NPM,
            "shared",
            "1.0.0",
            &source.integrity.to_string(),
            &source.digest.to_string(),
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        locked.set_artifact_url(&url).unwrap();
        let mut wire: serde_json::Value =
            serde_json::from_str(&pending.to_json().unwrap()).unwrap();
        wire["packages"][&key]["sourceEvidence"] =
            serde_json::to_value(NativeSourceEvidence::Verified {
                package: Box::new(locked),
            })
            .unwrap();
        let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
        prepared.transaction.publish().unwrap().commit().unwrap();
        let before = selection.to_json().unwrap();
        let replayed = prepare_native_replay(&selection, &config, &store, full, |_, _| {
            Err("offline archive unavailable".into())
        })
        .unwrap();
        assert_eq!(replayed.sources.len(), 1);
        assert_eq!(selection.to_json().unwrap(), before);
        drop(replayed);

        let cold_store = Store::new(project.path().join("cold-store"));
        let mut fetches = 0;
        let cold = prepare_native_replay(&selection, &config, &cold_store, full, |_, requested| {
            fetches += 1;
            assert_eq!(requested, url);
            Ok(bytes.clone())
        })
        .unwrap();
        assert_eq!(fetches, 1);
        let digest = cold.sources[&key].digest.clone();
        drop(cold);
        let error = prepare_native_replay(&selection, &config, &cold_store, full, |_, _| {
            Ok(b"tampered".to_vec())
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains("integrity mismatch"), "{error}");

        let cached_tree = store.verified_tree_path(&digest).unwrap();
        fs::write(
            cached_tree.join("package/package.json"),
            b"tampered cached tree",
        )
        .unwrap();
        let error = prepare_native_replay(&selection, &config, &store, full, |_, _| {
            panic!("a corrupt verified cache must fail closed before archive fallback")
        })
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("locked store tree is invalid"),
            "{error}"
        );
        assert_eq!(selection.to_json().unwrap(), before);
    }

    #[test]
    fn cold_production_preparation_never_reads_omitted_artifacts() {
        let project = tapid_test_support::TempProject::new("profile-cold-artifact-access").unwrap();
        let text = r#"{"name":"app","version":"1.0.0","devDependencies":{"tool":"1"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        // This URL is a valid pin, but fixture artifact access treats it as a local
        // filename that does not exist. Any attempted read fails deterministically.
        let fixture = serde_json::json!({"packages":[{
            "registry":NPM,"name":"tool","version":"1.0.0",
            "integrity":integrity(b"unavailable").to_string(),
            "artifact":"https://registry.npmjs.org/tool.tgz"
        }]});
        let fixture = project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let manifest = PackageManifest::parse(text).unwrap();
        let config = crate::registry::RegistryConfig::default();
        let graph = graph::resolve_package_graph(
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
        let store = Store::new(project.path().join("store"));
        let prepared = graph
            .prepare_native_profile(
                project.path(),
                &manifest,
                &config,
                &store,
                tapid_lockfile::InstallProfile::production(),
            )
            .unwrap();
        let wire: serde_json::Value =
            serde_json::from_str(&prepared.selection.to_json().unwrap()).unwrap();
        assert_eq!(wire["packages"].as_object().unwrap().len(), 1);
        assert!(prepared.sources.is_empty());
        assert!(prepared.materialized_keys.is_empty());
        assert!(!project.path().join("node_modules").exists());
        assert!(!project.path().join("tapid.lock").exists());
        drop(prepared);
        let error = graph
            .prepare_native_profile(
                project.path(),
                &manifest,
                &config,
                &store,
                tapid_lockfile::InstallProfile::default(),
            )
            .err()
            .expect("full preparation must attempt the missing artifact");
        assert!(
            error.to_string().contains("cannot read artifact"),
            "{error}"
        );
    }

    #[test]
    fn native_profile_layout_transitions_preserve_selection_and_rollback() {
        use crate::filesystem::activation::{ActivationLock, activate_node_modules_with_preflight};
        let project = tapid_test_support::TempProject::new("native-profile-transitions").unwrap();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/npm-import/registry.json"
        ))
        .unwrap();
        let mut packages = BTreeMap::new();
        let mut archives = BTreeMap::new();
        let mut selected = Vec::new();
        for version in ["1.0.0", "2.0.0"] {
            let archive = fixture["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|package| package["name"] == "shared" && package["version"] == version)
                .unwrap();
            let key = LockfilePackageKey::new(
                NPM.parse().unwrap(),
                "shared".parse().unwrap(),
                version.parse().unwrap(),
                &Default::default(),
                &tapid_core::PlatformContext::new(None, None, None).unwrap(),
            )
            .to_string();
            archives.insert(
                key.clone(),
                STANDARD
                    .decode(
                        archive["artifact"]
                            .as_str()
                            .unwrap()
                            .strip_prefix("base64:")
                            .unwrap(),
                    )
                    .unwrap(),
            );
            packages.insert(
                key.clone(),
                NativeSelectedPackage {
                    source_evidence: NativeSourceEvidence::Pending {
                        artifact_url: format!("{NPM}/shared-{version}.tgz"),
                        artifact_integrity: archive["integrity"].as_str().unwrap().into(),
                    },
                    dependencies: BTreeMap::new(),
                    peer_providers: BTreeMap::new(),
                },
            );
            selected.push(key);
        }
        let selection = NativeSelection::new(
            "0".into(),
            "0".into(),
            format!("sha256-{}", "a".repeat(64)),
            BTreeMap::new(),
            vec![
                NativeDeclaration {
                    workspace: None,
                    name: "runtime-alias".into(),
                    kind: NativeDeclarationKind::Required,
                    target: selected[0].clone(),
                },
                NativeDeclaration {
                    workspace: None,
                    name: "tool".into(),
                    kind: NativeDeclarationKind::Dev,
                    target: selected[1].clone(),
                },
            ],
            packages,
        )
        .unwrap();
        let bytes = selection.to_json().unwrap();
        project.write("tapid.lock", bytes.as_bytes()).unwrap();
        let config = crate::registry::RegistryConfig::default();
        let store = Store::new(project.path().join("store"));
        for profile in [
            tapid_lockfile::InstallProfile::default(),
            tapid_lockfile::InstallProfile::production(),
            tapid_lockfile::InstallProfile::default(),
        ] {
            let mut fetched = Vec::new();
            let prepared = prepare_native_replay(&selection, &config, &store, profile, |key, _| {
                fetched.push(key.to_string());
                Ok(archives[&key.to_string()].clone())
            })
            .unwrap();
            assert_eq!(fetched.len(), if profile.include_dev { 2 } else { 1 });
            let activation = ActivationLock::acquire(project.path()).unwrap();
            let stage = crate::application::install::stage_native_profile(
                project.path(),
                &prepared,
                profile,
                &activation,
                |_| {},
            )
            .unwrap();
            if !profile.include_dev {
                assert!(
                    activate_node_modules_with_preflight(
                        project.path(),
                        &stage,
                        &activation,
                        false,
                        || Err("injected failure".into())
                    )
                    .is_err()
                );
                assert!(
                    project
                        .path()
                        .join("node_modules/tool/package.json")
                        .is_file()
                );
            }
            activate_node_modules_with_preflight(
                project.path(),
                &stage,
                &activation,
                false,
                || Ok(()),
            )
            .unwrap();
            let installed = crate::filesystem::installed_profile::validate_profile(
                project.path(),
                &selection,
                &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                activation.owner_line(),
            )
            .unwrap();
            assert_eq!(installed, profile);
            let manifest: serde_json::Value = serde_json::from_slice(
                &fs::read(
                    project
                        .path()
                        .join("node_modules/runtime-alias/package.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(manifest["version"], "1.0.0");
            assert_eq!(
                project.path().join("node_modules/tool").exists(),
                profile.include_dev
            );
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                bytes.as_bytes()
            );
        }
    }
}
