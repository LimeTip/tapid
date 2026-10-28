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
    profile: tapid_lockfile::InstallProfile,
    policy: &tapid_runner::DependencyLifecyclePolicy,
    store_root: Option<&Path>,
    offline: bool,
    fixture: Option<&Path>,
    progress: impl FnMut(Progress),
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
    // Only materialized sources reach the shared lifecycle verifier. It checks
    // exact approvals and authenticated cached outputs without executing hooks.
    let config = crate::registry::RegistryConfig::load(project)
        .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
    let store = Store::new(match store_root {
        Some(root) => root.to_owned(),
        None => default_store_root().map_err(|e| OperationalError::new(ErrorKind::Store, e))?,
    });
    let prepared = online::prepare_native_locked(
        selection, &store, &config, fixture, project, offline, profile,
    )?;
    activate(
        session,
        project,
        manifest_path,
        original_manifest,
        Some(original_lock),
        prepared,
        profile,
        policy,
        &store,
        true,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn fresh(
    session: &mut InstallSession,
    project: &Path,
    manifest_path: &Path,
    original_manifest: &[u8],
    manifest: &PackageManifest,
    profile: tapid_lockfile::InstallProfile,
    policy: &tapid_runner::DependencyLifecyclePolicy,
    store_root: Option<&Path>,
    fixture: Option<&Path>,
    mut progress: impl FnMut(Progress),
) -> Result<(usize, bool), OperationalError> {
    let config = crate::registry::RegistryConfig::load(project)
        .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
    let store = Store::new(match store_root {
        Some(root) => root.to_owned(),
        None => default_store_root().map_err(|e| OperationalError::new(ErrorKind::Store, e))?,
    });
    let (prepared, exclusions) = online::prepare_native_install(
        project,
        manifest,
        &store,
        fixture,
        &config,
        profile,
        &mut progress,
    )?;
    session.candidate_exclusions = exclusions;
    activate(
        session,
        project,
        manifest_path,
        original_manifest,
        None,
        prepared,
        profile,
        policy,
        &store,
        false,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn activate(
    session: &mut InstallSession,
    project: &Path,
    manifest_path: &Path,
    original_manifest: &[u8],
    original_lock: Option<&[u8]>,
    mut prepared: online::PreparedNativeProfile,
    profile: tapid_lockfile::InstallProfile,
    policy: &tapid_runner::DependencyLifecyclePolicy,
    store: &Store,
    replayed: bool,
    mut progress: impl FnMut(Progress),
) -> Result<(usize, bool), OperationalError> {
    let digest = online::root_digest(project)?;
    let activation = session.lock.as_ref().expect("project lock acquired");
    session.journal = Some(
        crate::filesystem::lifecycle_journal::LifecycleJournal::begin(
            project,
            manifest_path,
            activation.owner_line(),
            original_manifest,
            original_lock,
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
    let mut lock = prepared.lifecycle_sources(&digest, profile)?;
    let mut layout = prepared.layout(profile)?;
    let mut snapshots = Vec::new();
    // Replay authenticates existing outputs without executing hooks; fresh
    // preparation stages approved outputs in the source store transaction.
    // This invocation-local graph never replaces the complete selection.
    super::super::dependency_scripts::apply(
        project,
        policy,
        Default::default(),
        &mut session.hook_stages,
        &mut lock,
        &mut layout.input,
        &mut layout.trees,
        store,
        if replayed {
            None
        } else {
            Some(&mut prepared.transaction)
        },
        activation,
        &mut snapshots,
        &mut session.outcome,
    )?;
    if !replayed {
        let mut evidence = BTreeMap::new();
        for (key, package) in lock.packages() {
            let mut package = package.clone();
            let url = match &prepared.selection.packages()[key].source_evidence {
                tapid_lockfile::NativeSourceEvidence::Pending { artifact_url, .. } => artifact_url,
                tapid_lockfile::NativeSourceEvidence::Verified { package } => package
                    .artifact_url()
                    .ok_or("native registry source has no artifact URL")?,
                tapid_lockfile::NativeSourceEvidence::Workspace { .. } => {
                    return Err("native lifecycle preparation requires registry sources".into());
                }
            };
            package.set_artifact_url(url)?;
            evidence.insert(key.clone(), package);
        }
        prepared.selection.record_verified_sources(evidence)?;
    }
    let selection = &prepared.selection;
    session.mutated = true;
    if !replayed {
        let lock_path = project.join("tapid.lock");
        let json = selection.to_json()?;
        session.lock_backup = crate::filesystem::atomic::replace_lockfile(&lock_path, &json)
            .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?;
        session.outcome.changed_files.push(lock_path);
        crate::filesystem::activation::test_crash_at("lockfile_replaced");
    }
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
    crate::filesystem::atomic::discard_lockfile_backup(session.lock_backup.as_deref())
        .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?;
    session.lock_backup = None;
    journal
        .finish()
        .map_err(|e| OperationalError::new(ErrorKind::Transaction, e))?;
    Ok((layout.input.instances.len(), replayed))
}
