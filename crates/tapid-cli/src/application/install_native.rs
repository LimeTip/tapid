//! Replay complete selections without replacing them with a materialized lock.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn replay(
    session: &mut InstallSession,
    project: &Path,
    manifest_path: &Path,
    original_manifest: &[u8],
    original_lock: &[u8],
    selection: &tapid_lockfile::NativeSelection,
    policy: &tapid_runner::DependencyLifecyclePolicy,
    store_root: Option<&Path>,
    offline: bool,
    fixture: Option<&Path>,
    mut progress: impl FnMut(Progress),
) -> Result<(usize, bool), OperationalError> {
    let digest = online::root_digest(project)?;
    // Member activation is not yet supported here. Never guess a missing member
    // snapshot, or treat workspace trees as registry artifacts.
    selection.validate_replay(&digest, &BTreeMap::new())?;
    for key in selection.packages().keys() {
        if key
            .parse::<tapid_lockfile::LockfilePackageKey>()?
            .source
            .registry()
            .is_none()
        {
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                "native selection replay currently requires registry-only sources",
            ));
        }
    }
    if !policy.approvals().is_empty() || selection.packages().values().any(|package| {
        matches!(&package.source_evidence, tapid_lockfile::NativeSourceEvidence::Verified { package }
            if !package.derived_hooks().is_empty())
    }) {
        return Err(OperationalError::new(ErrorKind::InvalidRequest,
            "native selection lifecycle output integration is required before replaying approved or derived hooks"));
    }
    let config = crate::registry::RegistryConfig::load(project)
        .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
    let store = Store::new(match store_root {
        Some(root) => root.to_owned(),
        None => default_store_root().map_err(|e| OperationalError::new(ErrorKind::Store, e))?,
    });
    let prepared =
        online::prepare_native_locked(selection, &store, &config, fixture, project, offline)?;
    let activation = session.lock.as_ref().expect("project lock acquired");
    session.journal = Some(
        crate::filesystem::lifecycle_journal::LifecycleJournal::begin(
            project,
            manifest_path,
            activation.owner_line(),
            original_manifest,
            Some(original_lock),
        )
        .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?,
    );
    let journal = session
        .journal
        .as_mut()
        .expect("native replay journal created");
    journal
        .set_store_root(store.root())
        .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?;
    let profile = tapid_lockfile::InstallProfile::default();
    let mut lock = prepared.lifecycle_sources(&digest)?;
    crate::application::replay::validate_root_bindings(
        &lock,
        &read_manifest(&project.join("package.json"))?,
        &config,
    )?;
    let mut layout = prepared.layout(profile)?;
    let mut snapshots = Vec::new();
    // Discovery and default-denied hook reporting use the established lifecycle
    // path. This invocation-local graph is never written to tapid.lock.
    super::super::dependency_scripts::apply(
        project,
        policy,
        Default::default(),
        &mut session.hook_stages,
        &mut lock,
        &mut layout.input,
        &mut layout.trees,
        &store,
        None,
        activation,
        &mut snapshots,
        &mut session.outcome,
    )?;
    session.mutated = true;
    let publication = prepared
        .transaction
        .publish_for_lifecycle(&journal.coordinator_path())?;
    crate::filesystem::activation::test_crash_at("store_published");
    layout.rebase(&publication);
    let stage = stage_layout(
        project,
        project,
        &layout.input,
        &layout.trees,
        &layout.workspace.links,
        true,
        activation,
        &mut progress,
    )?;
    let activation_result = (|| {
        crate::filesystem::installed_profile::stage_profile(
            &stage,
            selection,
            profile,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            activation.owner_line(),
        )
        .map_err(|e| OperationalError::new(ErrorKind::Materialization, e))?;
        crate::filesystem::activation::activate_node_modules_with_preflight(
            project,
            &stage,
            activation,
            true,
            || Ok(()),
        )
        .map_err(|e| OperationalError::new(ErrorKind::Materialization, e))
    })();
    let _ = fs::remove_dir_all(&stage);
    activation_result?;
    session
        .outcome
        .changed_files
        .push(project.join("node_modules"));
    crate::filesystem::activation::test_crash_at("activation_complete");
    journal
        .mark_committed()
        .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?;
    session.committed = true;
    session.outcome.state = ChangeState::Committed;
    crate::filesystem::activation::test_crash_at("commit_decision");
    publication.commit()?;
    journal
        .finish()
        .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?;
    Ok((selection.packages().len(), true))
}
