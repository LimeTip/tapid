//! Selection persistence without installed-layout or verified-store effects.
use super::{
    inputs::Inputs,
    outcome::{ErrorKind, OperationalError},
};
use std::{fs, path::Path};

#[derive(Debug)]
pub(crate) struct SelectionReport {
    pub outcome: super::outcome::OperationOutcome,
    pub package_count: usize,
    pub pending_receipts: usize,
    pub metadata_lookups: usize,
    pub copied_source_preparations: usize,
    pub imported: bool,
    pub candidate_exclusions: super::outcome::CandidateExclusions,
}

/// Settle metadata decisions before reading declarations for a new edit plan.
/// Discovery locates the coordinator; selection is repeated after its recovery.
pub(crate) fn resolve_workspace(
    project: &Path,
    selector: Option<&str>,
) -> Result<super::lifecycle::WorkspaceSelection, OperationalError> {
    let workspace = tapid_manifest::Workspace::discover_typed(project)?;
    let root = workspace
        .root_path()
        .parent()
        .ok_or_else(|| OperationalError::new(ErrorKind::Project, "workspace root has no parent"))?;
    drop(crate::filesystem::activation::ActivationLock::acquire_metadata(root)?);
    super::lifecycle::resolve_workspace(project, selector)
}

/// Persist selections using the same edit plans as installation, without entering activation.
pub(crate) fn run(
    project: &Path,
    target: &Path,
    before: &tapid_manifest::PackageManifest,
    after: &tapid_manifest::PackageManifest,
    fixture: Option<&Path>,
    update: Option<super::lifecycle::UpdateRequest<'_>>,
) -> Result<SelectionReport, super::outcome::OperationFailure> {
    use super::outcome::{ChangeState, OperationFailure, OperationOutcome};
    let project = fs::canonicalize(project).map_err(|e| {
        OperationFailure::unchanged(
            project,
            OperationalError::from_source(ErrorKind::Project, e),
        )
    })?;
    let target = if target.is_absolute() {
        target.to_owned()
    } else {
        project.join(target)
    };
    let result = (|| {
        drop(crate::filesystem::activation::ActivationLock::acquire_metadata(&project)?);
        let inputs = Inputs::capture(&project, &target)?;
        if inputs.manifest(&target) != before {
            return Err(OperationalError::new(
                ErrorKind::ProjectChanged,
                "package.json changed while preparing dependency selections",
            ));
        }
        Ok(inputs)
    })();
    let inputs = result.map_err(|e| OperationFailure::unchanged(&project, e))?;
    let paths = [target.clone(), project.join("tapid.lock")];
    let snapshot = |path: &std::path::PathBuf| {
        crate::filesystem::read::read_bounded_file(path, 32 * 1024 * 1024).ok()
    };
    let previous = paths.iter().map(snapshot).collect::<Vec<_>>();
    let mut outcome = OperationOutcome::unchanged(&project);
    let result = (|| {
        let imported = inputs
            .lock
            .as_deref()
            .map(|bytes| {
                super::install::parse_install_lock(
                    bytes,
                    &project.join("tapid.lock"),
                    false,
                    false,
                    Default::default(),
                )
            })
            .transpose()?;
        if before == after
            && update.is_none()
            && matches!(imported, Some(super::install::InstallLock::Imported(_)))
        {
            let lock = preserve_imported(&project)?;
            let graph = lock
                .graph()
                .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
            return Ok(SelectionReport {
                outcome: OperationOutcome::unchanged(&project),
                package_count: graph.packages.len(),
                pending_receipts: graph
                    .packages
                    .keys()
                    .filter(|path| lock.verified_tree(path).is_none())
                    .count(),
                metadata_lookups: 0,
                copied_source_preparations: 0,
                imported: true,
                candidate_exclusions: Default::default(),
            });
        }
        let candidate = persist_edit(&project, &target, before, after, fixture, update)?;
        let mut selected_outcome = OperationOutcome::unchanged(&project);
        selected_outcome.warnings = candidate.warnings;
        Ok(SelectionReport {
            outcome: selected_outcome,
            package_count: candidate.lock.packages().len(),
            pending_receipts: candidate
                .lock
                .packages()
                .values()
                .filter(|p| p.source_tree_receipt().is_none())
                .count(),
            metadata_lookups: candidate.metadata_lookups,
            copied_source_preparations: candidate.copied_source_preparations,
            imported: false,
            candidate_exclusions: candidate.candidate_exclusions,
        })
    })();
    // Only publication/recovery errors may have changed our outputs. A stale input
    // belongs to the concurrent writer and must not be reported as our own edit.
    if result.is_ok()
        || result
            .as_ref()
            .is_err_and(|e: &OperationalError| e.kind == ErrorKind::Recovery)
    {
        for (path, bytes) in paths.iter().zip(previous) {
            if snapshot(path) != bytes {
                outcome.changed_files.push(path.clone());
            }
        }
        if !outcome.changed_files.is_empty() {
            outcome.state = ChangeState::Committed;
        }
    }
    match result {
        Ok(mut report) => {
            outcome.warnings.append(&mut report.outcome.warnings);
            report.outcome = outcome;
            Ok(report)
        }
        Err(error) => {
            if error.kind == ErrorKind::Recovery
                && crate::filesystem::lifecycle_journal::has_pending(&project).unwrap_or(true)
            {
                outcome.state = ChangeState::RecoveryRequired;
            }
            Err(OperationFailure::with_recovery_paths(
                error, outcome, None, paths,
            ))
        }
    }
}

/// Preserve imported placement and receipts without hydration or native conversion.
/// Explicit dependency edits use native resolution instead of this path.
pub(super) fn preserve_imported(
    project: &Path,
) -> Result<tapid_lockfile::ImportedNpmLockfile, OperationalError> {
    let project = fs::canonicalize(project)
        .map_err(|e| OperationalError::from_source(ErrorKind::Project, e))?;
    let _guard = crate::filesystem::activation::ActivationLock::acquire_metadata(&project)?;
    let target = project.join("package.json");
    let inputs = Inputs::capture(&project, &target)?;
    let bytes = inputs.lock.as_deref().ok_or_else(|| {
        OperationalError::new(ErrorKind::LockfileMissing, "imported lock is missing")
    })?;
    let lock = tapid_lockfile::ImportedNpmLockfile::from_json(
        std::str::from_utf8(bytes)
            .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?,
    )
    .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
    // The digest binds the exact captured bytes; declaration validation uses the
    // equivalent parsed manifest and does not normalize any project output.
    lock.validate_replay(&inputs.manifest(&target).to_json(), inputs.root_digest())
        .map_err(|e| OperationalError::from_source(ErrorKind::LockManifestMismatch, e))?;
    lock.validate_workspace_manifests(&inputs.imported_workspace_manifests()?)
        .map_err(|e| OperationalError::from_source(ErrorKind::LockManifestMismatch, e))?;
    for package in lock
        .graph()
        .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?
        .packages
        .values()
    {
        crate::online::validate_imported_registry(package, &inputs.config)?;
    }
    inputs.recheck()?;
    Ok(lock)
}

// Convenience entrypoint for unchanged native manifests in application tests.
#[allow(dead_code)]
pub(super) fn persist_native(
    project: &Path,
    fixture: Option<&Path>,
) -> Result<crate::online::SelectionCandidate, OperationalError> {
    let project = fs::canonicalize(project)
        .map_err(|e| OperationalError::from_source(ErrorKind::Project, e))?;
    // Recover metadata only before capturing evidence; activation recovery is forbidden.
    drop(crate::filesystem::activation::ActivationLock::acquire_metadata(&project)?);
    let target = project.join("package.json");
    let inputs = Inputs::capture(&project, &target)?;
    let manifest = inputs.manifest(&target).clone();
    persist_edit(&project, &target, &manifest, &manifest, fixture, None)
}

pub(super) fn persist_edit(
    project: &Path,
    target: &Path,
    before: &tapid_manifest::PackageManifest,
    after: &tapid_manifest::PackageManifest,
    fixture: Option<&Path>,
    update: Option<super::lifecycle::UpdateRequest<'_>>,
) -> Result<crate::online::SelectionCandidate, OperationalError> {
    let project = fs::canonicalize(project)
        .map_err(|e| OperationalError::from_source(ErrorKind::Project, e))?;
    drop(crate::filesystem::activation::ActivationLock::acquire_metadata(&project)?);
    let target = if target.is_absolute() {
        target.to_owned()
    } else {
        project.join(target)
    };
    let mut inputs = Inputs::capture(&project, &target)?;
    if inputs.manifest(&target) != before {
        return Err(OperationalError::new(
            ErrorKind::ProjectChanged,
            "package.json changed while preparing dependency selections",
        ));
    }
    let root = project.join("package.json");
    let manifest = if target == root {
        after.clone()
    } else {
        inputs.manifest(&root).clone()
    };
    inputs.capture_copied_files(&manifest)?;
    let previous = inputs
        .lock
        .as_deref()
        .map(|bytes| {
            super::install::parse_install_lock(
                bytes,
                &project.join("tapid.lock"),
                false,
                false,
                Default::default(),
            )
        })
        .transpose()?;
    let previous = match previous {
        Some(super::install::InstallLock::Native(lock)) => Some(lock),
        Some(super::install::InstallLock::Imported(_)) if before != after || update.is_some() => {
            None
        }
        Some(super::install::InstallLock::Imported(_)) => {
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                "matching imported selections must be preserved without native conversion",
            ));
        }
        None => None,
    };
    if let Some(lock) = &previous {
        lock.validate_selection(lock.root_manifest_digest())?;
    }
    let workspace = inputs.materialize(&inputs.config, Some((&target, after)))?;
    let update = update.map(|request| super::lifecycle::UpdateRequest {
        target: &target,
        ..request
    });
    let mut candidate = crate::online::prepare_selection(
        &project,
        &manifest,
        workspace,
        fixture,
        &inputs.config,
        previous.as_ref(),
        inputs.manifest(&root),
        inputs.root_digest(),
        update,
    )?;
    if let (Some(request), Some(previous)) = (update, previous.as_ref()) {
        candidate.warnings =
            super::lifecycle::update_change_warnings(&project, request, previous, &candidate.lock)?;
    }
    let saved = candidate.updated_manifest.as_ref().unwrap_or(after);
    inputs.publish_selection(
        &candidate.lock,
        (saved != before).then_some((target.as_path(), saved)),
    )?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    #[test]
    fn selection_entrypoint_explicit_edit_converts_imported_lock_without_hydration() {
        let project =
            tapid_test_support::TempProject::new("selection-imported-conversion").unwrap();
        let text = include_str!("../../tests/fixtures/npm-import/package.json");
        project.write("package.json", text.as_bytes()).unwrap();
        let imported = tapid_lockfile::ImportedNpmLockfile::import(
            include_str!("../../tests/fixtures/npm-import/package-lock.json"),
            text,
            &crate::filesystem::atomic::digest_bytes(text.as_bytes()),
        )
        .unwrap();
        project
            .write("tapid.lock", imported.to_json().unwrap().as_bytes())
            .unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let before = tapid_manifest::PackageManifest::parse(text).unwrap();
        let plan = crate::application::lifecycle::plan_remove(
            &before,
            &["consumer".into(), "native".into()],
        )
        .unwrap();
        let target = project.path().canonicalize().unwrap().join("package.json");
        let previous_lock = std::fs::read(project.path().join("tapid.lock")).unwrap();
        project
            .write("registry.json", br#"{"packages":[]}"#)
            .unwrap();
        assert!(
            super::persist_edit(
                project.path(),
                &target,
                &before,
                &plan.manifest,
                Some(&fixture),
                None
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(project.path().join("tapid.lock")).unwrap(),
            previous_lock
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let candidate = super::persist_edit(
            project.path(),
            &target,
            &before,
            &plan.manifest,
            Some(&fixture),
            None,
        )
        .unwrap();
        assert_eq!(candidate.lock.packages().len(), 3);
        assert!(
            candidate
                .lock
                .packages()
                .values()
                .all(|p| p.source_tree_receipt().is_none())
        );
        assert!(!candidate.lock.root_bindings().contains_key("consumer"));
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!project.path().join("store").exists());
        let bytes = std::fs::read_to_string(project.path().join("tapid.lock")).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&bytes).unwrap()["lockfileVersion"],
            11
        );
    }
    #[test]
    fn selection_entrypoint_preserves_imported_workspace_with_unowned_installation() {
        let project = tapid_test_support::TempProject::new("selection-imported-workspace").unwrap();
        let root = r#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"*"}}"#;
        let member = r#"{"name":"member","version":"1.0.0"}"#;
        project.write("package.json", root.as_bytes()).unwrap();
        project
            .write("packages/member/package.json", member.as_bytes())
            .unwrap();
        project
            .write(
                "packages/member/node_modules/sentinel",
                b"unowned installation",
            )
            .unwrap();
        let npm = serde_json::json!({"name":"app","version":"1.0.0","lockfileVersion":3,"packages":{
            "":{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"*"}},
            "packages/member":{"name":"member","version":"1.0.0"},
            "node_modules/member":{"resolved":"packages/member","link":true}
        }});
        let mut lock = tapid_lockfile::ImportedNpmLockfile::import(
            &npm.to_string(),
            root,
            &crate::filesystem::atomic::digest_bytes(root.as_bytes()),
        )
        .unwrap();
        lock.bind_workspace_manifests(&std::collections::BTreeMap::from([(
            "packages/member".into(),
            (
                member.into(),
                crate::filesystem::atomic::digest_bytes(member.as_bytes()),
            ),
        )]))
        .unwrap();
        let bytes = lock.to_json().unwrap();
        project.write("tapid.lock", bytes.as_bytes()).unwrap();
        super::preserve_imported(project.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(project.path().join("packages/member/node_modules/sentinel")).unwrap(),
            b"unowned installation"
        );
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"1.0.1"}"#,
            )
            .unwrap();
        assert_eq!(
            super::preserve_imported(project.path()).unwrap_err().kind,
            super::ErrorKind::LockManifestMismatch
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            bytes
        );
    }
    #[test]
    fn selection_entrypoint_preserves_imported_bytes_without_hydration() {
        let project =
            tapid_test_support::TempProject::new("selection-entrypoint-imported").unwrap();
        let text = include_str!("../../tests/fixtures/npm-import/package.json");
        project.write("package.json", text.as_bytes()).unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let imported = tapid_lockfile::ImportedNpmLockfile::import(
            include_str!("../../tests/fixtures/npm-import/package-lock.json"),
            text,
            &crate::filesystem::atomic::digest_bytes(text.as_bytes()),
        )
        .unwrap();
        let bytes = imported.to_json().unwrap();
        project.write("tapid.lock", bytes.as_bytes()).unwrap();
        let preserved = super::preserve_imported(project.path()).unwrap();
        assert_eq!(preserved.to_json().unwrap(), bytes);
        assert_eq!(
            std::fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("package.json")).unwrap(),
            text
        );
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!project.path().join("store").exists());
        assert!(!crate::filesystem::lifecycle_journal::has_pending(project.path()).unwrap());
        project
            .write(
                "tapid.toml",
                b"[registries.default]\nurl = \"https://packages.example.invalid\"\n",
            )
            .unwrap();
        assert_eq!(
            super::preserve_imported(project.path()).unwrap_err().kind,
            super::ErrorKind::RegistryConfiguration
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            bytes
        );
        std::fs::remove_file(project.path().join("tapid.toml")).unwrap();
        project
            .write("package.json", br#"{"name":"changed","version":"1.0.0"}"#)
            .unwrap();
        assert_eq!(
            super::preserve_imported(project.path()).unwrap_err().kind,
            super::ErrorKind::LockManifestMismatch
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            bytes
        );
    }
    #[test]
    fn selection_entrypoint_named_update_saves_exact_and_preserves_other_pins() {
        use crate::application::lifecycle::{SavePolicy, UpdateRequest, plan_update};
        let project = tapid_test_support::TempProject::new("selection-entrypoint-update").unwrap();
        let text =
            r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1","shared":"^1"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let initial = super::persist_native(project.path(), Some(&fixture)).unwrap();
        let mut data: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../tests/fixtures/npm-import/registry.json"
        ))
        .unwrap();
        let packages = data["packages"].as_array_mut().unwrap();
        for name in ["parent", "shared"] {
            let mut record = packages.iter().find(|p| p["name"] == name).unwrap().clone();
            record["version"] = serde_json::json!("1.9.0");
            packages.push(record);
        }
        project
            .write(
                "registry.json",
                serde_json::to_vec(&data).unwrap().as_slice(),
            )
            .unwrap();
        let before = tapid_manifest::PackageManifest::parse(text).unwrap();
        let plan = plan_update(&before, &["parent".into()], false).unwrap();
        let target = project.path().canonicalize().unwrap().join("package.json");
        let candidate = super::persist_edit(
            project.path(),
            &target,
            &before,
            &plan.manifest,
            Some(&fixture),
            Some(UpdateRequest {
                plan: &plan,
                target: &target,
                named: true,
                original_root_digest: None,
                save: Some(SavePolicy::Exact),
            }),
        )
        .unwrap();
        let saved =
            tapid_manifest::PackageManifest::parse(&std::fs::read_to_string(&target).unwrap())
                .unwrap();
        assert_eq!(saved.dependencies()["parent"], "1.9.0");
        assert_eq!(saved.dependencies()["shared"], "^1");
        assert_eq!(
            candidate.lock.root_bindings()["shared"],
            initial.lock.root_bindings()["shared"]
        );
        assert_eq!(
            candidate.lock.root_manifest_digest(),
            crate::filesystem::atomic::digest_bytes(saved.to_json().as_bytes())
        );
        assert!(!project.path().join("node_modules").exists());
    }
    #[test]
    fn selection_entrypoint_unchanged_save_preserves_manifest_bytes() {
        use crate::application::lifecycle::{SavePolicy, UpdateRequest, plan_update};
        let project =
            tapid_test_support::TempProject::new("selection-entrypoint-unchanged-save").unwrap();
        let text = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#;
        project.write("package.json", text.as_bytes()).unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let before = tapid_manifest::PackageManifest::parse(text).unwrap();
        let plan = plan_update(&before, &["parent".into()], false).unwrap();
        let target = project.path().canonicalize().unwrap().join("package.json");
        let candidate = super::persist_edit(
            project.path(),
            &target,
            &before,
            &plan.manifest,
            Some(&fixture),
            Some(UpdateRequest {
                plan: &plan,
                target: &target,
                named: true,
                original_root_digest: None,
                save: Some(SavePolicy::Exact),
            }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert_eq!(
            candidate.lock.root_manifest_digest(),
            crate::filesystem::atomic::digest_bytes(text.as_bytes())
        );
    }
    #[test]
    fn selection_entrypoint_publishes_workspace_edit_without_activation() {
        let project = tapid_test_support::TempProject::new("selection-entrypoint-edit").unwrap();
        let root = br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"workspace:*"}}"#;
        project.write("package.json", root).unwrap();
        let target = project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"1.0.0"}"#,
            )
            .unwrap()
            .canonicalize()
            .unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        project
            .write("packages/member/node_modules/sentinel", b"member-running")
            .unwrap();
        let before =
            tapid_manifest::PackageManifest::parse(&std::fs::read_to_string(&target).unwrap())
                .unwrap();
        let plan = crate::application::lifecycle::plan_add(
            &before,
            &[crate::application::lifecycle::DependencyMutation {
                name: "parent".into(),
                requirement: Some("^1".into()),
                kind: tapid_manifest::DependencyKind::Dependencies,
            }],
        )
        .unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let candidate = super::persist_edit(
            project.path(),
            &target,
            &before,
            &plan.manifest,
            Some(&fixture),
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(project.path().join("package.json")).unwrap(),
            root
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            plan.manifest.to_json()
        );
        assert_eq!(candidate.lock.packages().len(), 2);
        assert!(
            candidate
                .lock
                .packages()
                .values()
                .all(|p| p.source_tree_receipt().is_none())
        );
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert_eq!(
            std::fs::read(project.path().join("packages/member/node_modules/sentinel")).unwrap(),
            b"member-running"
        );
    }
    #[test]
    fn native_selection_failure_preserves_previous_outputs() {
        let project = tapid_test_support::TempProject::new("selection-entrypoint-failure").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1"}}"#;
        project.write("package.json", manifest).unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        super::persist_native(project.path(), Some(&fixture)).unwrap();
        let before = std::fs::read(project.path().join("tapid.lock")).unwrap();
        project
            .write("registry.json", br#"{"packages":[]}"#)
            .unwrap();
        assert!(super::persist_native(project.path(), Some(&fixture)).is_err());
        assert_eq!(
            std::fs::read(project.path().join("tapid.lock")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!crate::filesystem::lifecycle_journal::has_pending(project.path()).unwrap());
        assert!(!project.path().join("store").exists());
    }

    #[test]
    fn native_selection_entrypoint_preserves_installed_application() {
        let project = tapid_test_support::TempProject::new("selection-entrypoint").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1"}}"#;
        project.write("package.json", manifest).unwrap();
        project.write("node_modules/sentinel", b"running").unwrap();
        let fixture = project
            .write(
                "registry.json",
                include_bytes!("../../tests/fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let candidate = super::persist_native(project.path(), Some(&fixture)).unwrap();
        assert!(candidate.updated_manifest.is_none());
        assert!(candidate.metadata_lookups > 0);
        let lock = candidate.lock;
        assert_eq!(lock.packages().len(), 2);
        assert!(
            lock.packages()
                .values()
                .all(|p| p.source_tree_receipt().is_none())
        );
        assert_eq!(
            std::fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read(project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!project.path().join("store").exists());
        assert_eq!(
            tapid_lockfile::Lockfile::from_json(
                &std::fs::read_to_string(project.path().join("tapid.lock")).unwrap()
            )
            .unwrap(),
            lock
        );
    }
}
