//! Persistable native selections assembled without artifact or store preparation.
use super::*;

/// Resolve once and retain the complete validated selection for publication.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_selection(
    project: &Path,
    manifest: &PackageManifest,
    workspace: WorkspaceMaterialization,
    fixture: Option<&Path>,
    config: &crate::registry::RegistryConfig,
    previous: Option<&Lockfile>,
    original_root: &PackageManifest,
    root_digest: &str,
    update: Option<crate::application::lifecycle::UpdateRequest<'_>>,
) -> Result<SelectionCandidate, OperationalError> {
    let resolved = graph::resolve_package_graph(
        project,
        manifest,
        workspace,
        fixture,
        false,
        config,
        previous,
        update,
        |_| {},
    )?;
    let selected_root =
        if update.is_some_and(|request| request.target == project.join("package.json")) {
            resolved.updated_manifest.as_ref().unwrap_or(manifest)
        } else {
            manifest
        };
    let saved_root_digest = (selected_root != original_root)
        .then(|| crate::filesystem::atomic::digest_bytes(selected_root.to_json().as_bytes()));
    resolved.into_selection(
        saved_root_digest.as_deref().unwrap_or(root_digest),
        previous,
    )
}

impl graph::ResolvedGraph {
    // Shared persistence rules for fetched and selection-only graphs.
    pub(super) fn assemble_lock(
        &self,
        lock: &mut Lockfile,
        platform_contexts: &BTreeMap<
            tapid_registry_client::RegistryPackageId,
            tapid_core::PlatformContext,
        >,
        packages: Vec<(LockedPackage, tapid_registry_client::RegistryPackageId)>,
    ) -> Result<(), OperationalError> {
        let Self {
            resolution,
            records,
            workspace_registry_dependencies,
            root_registry_identities,
            direct_local_names,
            ..
        } = self;
        let mut workspace_locked = self.workspace_locked.clone();
        let empty_peer = tapid_core::PeerContext::default();
        let mut dependencies_by_parent = BTreeMap::new();
        for edge in &resolution.dependencies {
            dependencies_by_parent
                .entry(edge.parent.clone())
                .or_insert_with(Vec::new)
                .push(edge);
        }
        let locked_packages: Result<Vec<_>, OperationalError> = packages
            .into_iter()
            .map(|(mut locked, id)| {
                for edge in dependencies_by_parent.get(&id).into_iter().flatten() {
                    let target = &edge.child;
                    let target_platform = platform_contexts.get(target).ok_or_else(|| {
                        OperationalError::new(
                            ErrorKind::Resolution,
                            format!("missing platform context for {target}"),
                        )
                    })?;
                    let target_key = LockfilePackageKey::from_source(
                        target.registry.clone(),
                        target.name.clone(),
                        target.version.clone(),
                        resolution.peer_contexts.get(target).unwrap_or(&empty_peer),
                        target_platform,
                    )
                    .to_string();
                    locked
                        .add_alias_dependency(&edge.dependency.to_string(), &target_key)
                        .map_err(OperationalError::from)?;
                }
                Ok(locked)
            })
            .collect();
        for dependency in workspace_registry_dependencies {
            let id = graph::workspace_registry_target(dependency, resolution, records)?;
            let target_platform = platform_contexts
                .get(id)
                .ok_or_else(|| format!("missing platform context for {id}"))?;
            let target_key = LockfilePackageKey::from_source(
                id.registry.clone(),
                id.name.clone(),
                id.version.clone(),
                resolution.peer_contexts.get(id).unwrap_or(&empty_peer),
                target_platform,
            )
            .to_string();
            let member = workspace_locked
                .iter_mut()
                .find(|member| member.key() == dependency.member_key)
                .ok_or("workspace member dependency has no lockfile identity")?;
            let dependency_name = dependency.manifest_name.clone();
            if member
                .dependencies()
                .get(&dependency_name)
                .is_some_and(|existing| existing != &target_key)
            {
                return Err(OperationalError::new(
                    ErrorKind::InvalidRequest,
                    format!(
                        "workspace member '{}' declares ambiguous registry identities for dependency '{}'; refusing to overwrite a lockfile edge",
                        dependency.member_key, dependency_name
                    ),
                ));
            }
            if dependency.requirement.is_alias() {
                member
                    .add_alias_dependency(&dependency_name, &target_key)
                    .map_err(OperationalError::from)?;
            } else {
                member
                    .add_dependency(&dependency_name, &target_key)
                    .map_err(OperationalError::from)?;
            }
        }
        lock.insert_graph(locked_packages?, workspace_locked)
            .map_err(OperationalError::from)?;
        let direct_bindings = resolution
            .root_bindings
            .iter()
            .filter(|((registry, name), id)| {
                direct_local_names.contains(name)
                    && root_registry_identities.contains(&(registry.clone(), id.name.clone()))
            })
            .collect::<Vec<_>>();
        let mut root_keys = direct_bindings
            .iter()
            .map(|(_, id)| *id)
            .map(|id| {
                let platform = platform_contexts
                    .get(id)
                    .expect("selected root platform context");
                LockfilePackageKey::from_source(
                    id.registry.clone(),
                    id.name.clone(),
                    id.version.clone(),
                    resolution.peer_contexts.get(id).unwrap_or(&empty_peer),
                    platform,
                )
                .to_string()
            })
            .collect::<Vec<_>>();
        root_keys.extend(self.workspace_root_keys.iter().cloned());
        root_keys.sort();
        root_keys.dedup();
        lock.set_roots(root_keys).map_err(OperationalError::from)?;
        lock.set_root_bindings(
            direct_bindings
                .iter()
                .map(|((_, name), id)| {
                    let platform = platform_contexts
                        .get(id)
                        .expect("selected root platform context");
                    (
                        name.to_string(),
                        LockfilePackageKey::from_source(
                            id.registry.clone(),
                            id.name.clone(),
                            id.version.clone(),
                            resolution.peer_contexts.get(id).unwrap_or(&empty_peer),
                            platform,
                        )
                        .to_string(),
                    )
                })
                .collect(),
        )
        .map_err(OperationalError::from)?;
        Ok(())
    }
}

pub(crate) struct SelectionCandidate {
    pub lock: Lockfile,
    pub updated_manifest: Option<PackageManifest>,
    pub metadata_lookups: usize,
    pub copied_source_preparations: usize,
    pub candidate_exclusions: crate::application::outcome::CandidateExclusions,
    pub warnings: Vec<crate::application::outcome::Warning>,
}

impl graph::ResolvedGraph {
    /// Consumes the selected graph without fetching artifacts or preparing a store.
    pub(super) fn into_selection(
        self,
        root_digest: &str,
        previous: Option<&Lockfile>,
    ) -> Result<SelectionCandidate, OperationalError> {
        let mut lock = Lockfile::new_selection(root_digest)?;
        let mut platforms = BTreeMap::new();
        let mut packages = Vec::new();
        for id in &self.resolution.selected {
            let record = self
                .records
                .get(&(
                    id.registry.to_string(),
                    id.name.to_string(),
                    id.version.to_string(),
                ))
                .ok_or_else(|| {
                    OperationalError::new(
                        ErrorKind::RegistryMetadata,
                        format!("missing selected artifact metadata: {id}"),
                    )
                })?;
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
            let integrity = record.integrity.as_ref().ok_or_else(|| {
                OperationalError::new(
                    ErrorKind::Integrity,
                    format!("selected artifact lacks integrity: {id}"),
                )
            })?;
            let provenance = if id.registry.registry().is_some() {
                RegistryIntegrityProvenance::RegistryDeclared
            } else {
                RegistryIntegrityProvenance::LocallyComputed
            };
            let mut package = LockedPackage::select_with_context_and_provenance(
                &id.registry.to_string(),
                &id.name.to_string(),
                &id.version.to_string(),
                &integrity.to_string(),
                (&peer, &platform),
                provenance,
            )?;
            if !record.fixture && !record.artifact.is_empty() {
                package.set_artifact_url(&record.artifact)?;
            }
            platforms.insert(id.clone(), platform);
            packages.push((package, id.clone()));
        }
        self.assemble_lock(&mut lock, &platforms, packages)?;
        if let Some(previous) = previous {
            for (key, package) in previous.packages() {
                if lock.packages().contains_key(key)
                    && let Some(receipt) = package.source_tree_receipt()
                {
                    // Attachment checks the complete source/artifact binding. A mismatch leaves it pending.
                    match lock.attach_source_tree_receipt(key, receipt) {
                        Ok(())
                        | Err(tapid_lockfile::LockfileError::InvalidSourceTreeReceipt(_)) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        lock.validate_selection(root_digest)?;
        Ok(SelectionCandidate {
            warnings: Vec::new(),
            copied_source_preparations: self
                .records
                .values()
                .filter(|r| r.copied_archive.is_some())
                .count(),
            lock,
            updated_manifest: self.updated_manifest,
            metadata_lookups: self.metadata_lookups,
            candidate_exclusions: self.candidate_exclusions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_candidate_copied_file_publication_rechecks_accepted_archive() {
        for stale in [false, true] {
            let project =
                tapid_test_support::TempProject::new("selection-copied-publication").unwrap();
            let text = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"file:vendor/parent.tgz"}}"#;
            project.write("package.json", text.as_bytes()).unwrap();
            let fixture_bytes = include_bytes!("../../tests/fixtures/npm-import/registry.json");
            let fixture: serde_json::Value = serde_json::from_slice(fixture_bytes).unwrap();
            let encoded = fixture["packages"][0]["artifact"]
                .as_str()
                .unwrap()
                .strip_prefix("base64:")
                .unwrap();
            let archive = STANDARD.decode(encoded).unwrap();
            project.write("vendor/parent.tgz", &archive).unwrap();
            let fixture = project.write("registry.json", fixture_bytes).unwrap();
            project
                .write("node_modules/sentinel", b"running application")
                .unwrap();
            let target = project.path().join("package.json");
            let mut inputs =
                crate::application::inputs::Inputs::capture(project.path(), &target).unwrap();
            let manifest = PackageManifest::parse(text).unwrap();
            inputs.capture_copied_files(&manifest).unwrap();
            let resolved = graph::resolve_package_graph(
                project.path(),
                &manifest,
                inputs.materialize(&inputs.config, None).unwrap(),
                Some(&fixture),
                false,
                &inputs.config,
                None,
                None,
                |_| {},
            )
            .unwrap();
            let candidate = resolved.into_selection(inputs.root_digest(), None).unwrap();
            let key = &candidate.lock.root_bindings()["parent"];
            let selected = &candidate.lock.packages()[key];
            assert!(key.starts_with("file:vendor/parent.tgz#sha256-"));
            assert_eq!(
                selected.artifact_integrity(),
                integrity(&archive).to_string()
            );
            assert!(selected.source_tree_receipt().is_none());
            if stale {
                project
                    .write("vendor/parent.tgz", b"concurrent archive")
                    .unwrap();
            }
            let result = inputs.publish_selection(&candidate.lock, None);
            if stale {
                assert_eq!(result.unwrap_err().kind, ErrorKind::ProjectChanged);
                assert!(!project.path().join("tapid.lock").exists());
                assert_eq!(
                    fs::read(project.path().join("vendor/parent.tgz")).unwrap(),
                    b"concurrent archive"
                );
            } else {
                result.unwrap();
                assert_eq!(
                    fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
                    candidate.lock.to_json().unwrap()
                );
                assert_eq!(
                    fs::read(project.path().join("vendor/parent.tgz")).unwrap(),
                    archive
                );
            }
            assert_eq!(
                fs::read(project.path().join("node_modules/sentinel")).unwrap(),
                b"running application"
            );
            assert_eq!(fs::read(&target).unwrap(), text.as_bytes());
            assert!(!project.path().join("store").exists());
        }
    }

    #[test]
    fn selection_candidate_retains_only_bound_source_receipts_without_hook_outputs() {
        for mismatch in ["none", "integrity", "url", "provenance", "source"] {
            let project =
                tapid_test_support::TempProject::new("selection-receipt-binding").unwrap();
            let text = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1"}}"#;
            project.write("package.json", text.as_bytes()).unwrap();
            let fixture = project.write("registry.json", serde_json::json!({"packages":[
                {"registry":NPM,"name":"parent","version":"1.2.0","dependencies":{"child":"^2"},"integrity":integrity(b"parent").to_string(),"artifact":"never-download"},
                {"registry":NPM,"name":"child","version":"2.3.0","integrity":integrity(b"child").to_string(),"artifact":"never-download"}
            ]}).to_string().as_bytes()).unwrap();
            let root = root_digest(project.path()).unwrap();
            let source = if mismatch == "source" { JSR } else { NPM };
            let provenance = if mismatch == "provenance" {
                RegistryIntegrityProvenance::LocallyComputed
            } else {
                RegistryIntegrityProvenance::RegistryDeclared
            };
            let archive_integrity = integrity(if mismatch == "integrity" {
                b"changed"
            } else {
                b"parent"
            });
            let mut package = LockedPackage::new_with_provenance(
                source,
                "parent",
                "1.2.0",
                &archive_integrity.to_string(),
                &digest(b"verified source tree").to_string(),
                provenance,
            )
            .unwrap();
            if mismatch == "url" {
                package
                    .set_artifact_url("https://registry.npmjs.org/parent/-/parent-1.2.0.tgz")
                    .unwrap();
            }
            let key = package.key();
            let receipt = package.source_tree_receipt().unwrap();
            let mut previous = Lockfile::new(&root).unwrap();
            previous.insert_package(package).unwrap();
            previous.set_roots([key.clone()]).unwrap();
            previous
                .set_derived_hooks(
                    &key,
                    vec![
                        tapid_lockfile::DerivedHookOutput::new(
                            "postinstall",
                            &digest(b"recipe").to_string(),
                            &digest(b"hook output").to_string(),
                            &format!("hmac-sha256-{}", "a".repeat(64)),
                        )
                        .unwrap(),
                    ],
                )
                .unwrap();
            let config = crate::registry::RegistryConfig::default();
            let resolved = graph::resolve_package_graph(
                project.path(),
                &PackageManifest::parse(text).unwrap(),
                workspace_materialization(project.path(), &config).unwrap(),
                Some(&fixture),
                false,
                &config,
                None,
                None,
                |_| {},
            )
            .unwrap();
            let candidate = resolved.into_selection(&root, Some(&previous)).unwrap();
            let selected = &candidate.lock.packages()["https://registry.npmjs.org|parent@1.2.0|peer=-|platform=os=;cpu=;libc="];
            if mismatch == "none" {
                assert_eq!(selected.source_tree_receipt(), Some(receipt));
            } else {
                assert!(selected.source_tree_receipt().is_none(), "{mismatch}");
            }
            assert!(selected.derived_hooks().is_empty());
            assert!(candidate.lock.packages()["https://registry.npmjs.org|child@2.3.0|peer=-|platform=os=;cpu=;libc="].source_tree_receipt().is_none());
            assert!(candidate.lock.has_pending_receipts());
            assert_eq!(
                Lockfile::from_json(&candidate.lock.to_json().unwrap()).unwrap(),
                candidate.lock
            );
        }
    }

    #[test]
    fn selection_candidate_records_saved_workspace_manifest_without_writing_it() {
        use crate::application::lifecycle::{SavePolicy, UpdateRequest, plan_update};
        let project = tapid_test_support::TempProject::new("selection-workspace-save").unwrap();
        let root = r#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"workspace:*"}}"#;
        let member = r#"{"name":"member","version":"1.0.0","dependencies":{"host":"^1"}}"#;
        project.write("package.json", root.as_bytes()).unwrap();
        let target = project
            .write("packages/member/package.json", member.as_bytes())
            .unwrap()
            .canonicalize()
            .unwrap();
        let fixture = project.write("registry.json", serde_json::json!({"packages":[
            {"registry":NPM,"name":"host","version":"1.4.0","integrity":integrity(b"host").to_string(),"artifact":"never-download"}
        ]}).to_string().as_bytes()).unwrap();
        let plan = plan_update(
            &PackageManifest::parse(member).unwrap(),
            &["host".into()],
            false,
        )
        .unwrap();
        let config = crate::registry::RegistryConfig::default();
        let resolved = graph::resolve_package_graph(
            project.path(),
            &PackageManifest::parse(root).unwrap(),
            workspace_materialization_with_manifest(
                project.path(),
                &config,
                Some((&target, &plan.manifest)),
            )
            .unwrap(),
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
        let candidate = resolved
            .into_selection(&root_digest(project.path()).unwrap(), None)
            .unwrap();
        let saved = candidate.updated_manifest.unwrap();
        assert_eq!(saved.dependencies()["host"], "1.4.0");
        let workspace = candidate.lock.workspace_packages().values().next().unwrap();
        assert_eq!(
            workspace.manifest_digest(),
            digest(saved.to_json().as_bytes()).to_string()
        );
        let host = "https://registry.npmjs.org|host@1.4.0|peer=-|platform=os=;cpu=;libc=";
        assert_eq!(workspace.dependencies()["host"], host);
        assert_eq!(candidate.lock.packages().len(), 1);
        assert_eq!(fs::read(&target).unwrap(), member.as_bytes());
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            root.as_bytes()
        );
        assert!(!project.path().join("tapid.lock").exists());
    }

    #[test]
    fn selection_candidate_persists_exact_graph_without_artifacts_or_store() {
        let project = tapid_test_support::TempProject::new("selection-candidate").unwrap();
        let text = r#"{"name":"app","version":"1.0.0","dependencies":{"alias":"npm:parent@^1"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        project
            .write("node_modules/sentinel", b"running application")
            .unwrap();
        let fixture = serde_json::json!({"packages":[
            {"registry":NPM,"name":"parent","version":"1.2.0","dependencies":{"child":"^2"},"integrity":integrity(b"parent").to_string(),"artifact":"missing-parent.tgz"},
            {"registry":NPM,"name":"child","version":"2.3.0","integrity":integrity(b"child").to_string(),"artifact":"missing-child.tgz"}
        ]});
        let fixture = project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let config = crate::registry::RegistryConfig::default();
        let inputs = crate::application::inputs::Inputs::capture(
            project.path(),
            &project.path().join("package.json"),
        )
        .unwrap();
        let resolved = graph::resolve_package_graph(
            project.path(),
            &PackageManifest::parse(text).unwrap(),
            workspace_materialization(project.path(), &config).unwrap(),
            Some(&fixture),
            false,
            &config,
            None,
            None,
            |_| {},
        )
        .unwrap();
        let candidate = resolved
            .into_selection(&root_digest(project.path()).unwrap(), None)
            .unwrap();
        assert_eq!(candidate.metadata_lookups, 2);
        assert!(candidate.updated_manifest.is_none());
        let lock = candidate.lock;
        let parent = "https://registry.npmjs.org|parent@1.2.0|peer=-|platform=os=;cpu=;libc=";
        let child = "https://registry.npmjs.org|child@2.3.0|peer=-|platform=os=;cpu=;libc=";
        assert_eq!(lock.packages().len(), 2);
        assert_eq!(lock.root_bindings()["alias"], parent);
        assert_eq!(lock.packages()[parent].dependencies()["child"], child);
        assert_eq!(
            lock.packages()[parent].artifact_integrity(),
            integrity(b"parent").to_string()
        );
        assert!(lock.has_pending_receipts());
        assert_eq!(Lockfile::from_json(&lock.to_json().unwrap()).unwrap(), lock);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            text.as_bytes()
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running application"
        );
        for path in ["tapid.lock", "store", ".tapid-activation.lock"] {
            assert!(!project.path().join(path).exists(), "{path}");
        }
        // Publish the same validated graph, with no second metadata inspection.
        // Missing archive fixtures make accidental artifact fetching fail.
        let expected = lock.to_json().unwrap();
        inputs.publish_selection(&lock, None).unwrap();
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            expected
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            text.as_bytes()
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running application"
        );
        assert!(!project.path().join("store").exists());
    }
}
