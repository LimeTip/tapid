//! Complete metadata selections and separate profile-scoped source preparation.
use super::*;
use tapid_lockfile::{
    NativeDeclaration, NativeDeclarationKind, NativeDependency, NativeDependencyKind,
    NativeSelectedPackage, NativeSelection, NativeSourceEvidence,
};

/// Complete selection and separately verified materialized sources. This is not a
/// legacy lock and cannot be activated without layout/lifecycle preparation.
#[allow(dead_code)]
pub(super) struct PreparedNativeProfile {
    pub(super) selection: NativeSelection,
    pub(super) materialized_keys: BTreeSet<String>,
    pub(super) sources: BTreeMap<String, PreparedNativeSource>,
    pub(super) transaction: StoreTransaction,
}

#[allow(dead_code)]
pub(super) struct PreparedNativeSource {
    pub(super) integrity: PackageIntegrity,
    pub(super) digest: ArtifactDigest,
    pub(super) tree: PathBuf,
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
        drop(prepared);
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
        assert!(!project.path().join("tapid.lock").exists());
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
}
