use super::outcome::{
    ChangeState, ErrorKind, OperationFailure, OperationOutcome, OperationalError, Warning,
};
use crate::commands::manifest::read_manifest_typed as read_manifest;
use crate::filesystem::activation::ActivationLock;
use crate::{online, package_spec};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};
use tapid_linker::{ManagedRoot, NamedLayoutInput, plan_named_layout};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;
use tapid_store::Store;

#[derive(Debug)]
pub(crate) struct InstallReport {
    pub(crate) package_count: usize,
    pub(crate) replayed: bool,
    pub(crate) outcome: OperationOutcome,
}

struct InstallSession {
    journal: Option<crate::filesystem::lifecycle_journal::LifecycleJournal>,
    lock_backup: Option<PathBuf>,
    // Keep the lock through explicit settlement, including error paths.
    lock: Option<ActivationLock>,
    outcome: OperationOutcome,
    mutated: bool,
    committed: bool,
}
impl InstallSession {
    fn new(project: &Path) -> Self {
        Self {
            journal: None,
            lock_backup: None,
            lock: None,
            outcome: OperationOutcome::unchanged(project),
            mutated: false,
            committed: false,
        }
    }

    fn fail(mut self, error: OperationalError) -> OperationFailure {
        let recovery = self.journal.as_mut().map(|journal| journal.settle());
        let mut recovery_error = match recovery {
            Some(Ok(true)) => {
                self.outcome.state = ChangeState::Committed;
                None
            }
            Some(Ok(false)) => {
                self.outcome.state = if self.mutated {
                    ChangeState::RolledBack
                } else {
                    ChangeState::Unchanged
                };
                self.outcome.changed_files.clear();
                None
            }
            Some(Err(recovery)) => {
                self.outcome.state = if self.committed {
                    ChangeState::CommittedCleanupPending
                } else {
                    ChangeState::RecoveryRequired
                };
                Some(OperationalError::new(ErrorKind::Recovery, recovery))
            }
            None => {
                if error.kind == ErrorKind::Recovery {
                    self.outcome.state = ChangeState::RecoveryRequired;
                }
                None
            }
        };
        if let Some(backup) = self.lock_backup.as_deref()
            && recovery_error.is_none()
            && backup.exists()
            && let Err(cleanup) = crate::filesystem::atomic::discard_lockfile_backup(Some(backup))
        {
            self.outcome.state = if matches!(
                self.outcome.state,
                ChangeState::Committed | ChangeState::CommittedCleanupPending
            ) {
                ChangeState::CommittedCleanupPending
            } else {
                ChangeState::RecoveryRequired
            };
            recovery_error = Some(OperationalError::new(ErrorKind::Recovery, cleanup));
        }
        OperationFailure::new(error, self.outcome, recovery_error)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum InstallMode {
    Online,
    Offline,
    Frozen,
}

fn default_store_root_for<F>(platform: &str, mut environment: F) -> Result<PathBuf, String>
where
    F: FnMut(&str) -> Option<OsString>,
{
    fn absolute(value: OsString, variable: &str, platform: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(value);
        let target_absolute = if platform == "windows" {
            path.to_str().is_some_and(|value| {
                let bytes = value.as_bytes();
                (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/'))
                    || value.starts_with("\\\\")
            })
        } else {
            path.to_string_lossy().starts_with('/')
        };
        if !target_absolute {
            return Err(format!("{variable} must contain an absolute path"));
        }
        Ok(path)
    }

    let root = match platform {
        "macos" => absolute(
            environment("HOME").ok_or("HOME is required to locate Tapid's verified store")?,
            "HOME",
            platform,
        )?
        .join("Library/Caches"),
        "windows" => absolute(
            environment("LOCALAPPDATA")
                .ok_or("LOCALAPPDATA is required to locate Tapid's verified store")?,
            "LOCALAPPDATA",
            platform,
        )?,
        _ => {
            if let Some(cache) = environment("XDG_CACHE_HOME") {
                absolute(cache, "XDG_CACHE_HOME", platform)?
            } else {
                absolute(
                    environment("HOME")
                        .ok_or("HOME is required to locate Tapid's verified store")?,
                    "HOME",
                    platform,
                )?
                .join(".cache")
            }
        }
    };
    Ok(root.join("tapid/store"))
}

fn default_store_root() -> Result<PathBuf, String> {
    default_store_root_for(std::env::consts::OS, |name| std::env::var_os(name))
}

pub(crate) fn run(
    project_dir: &Path,
    package: Option<&str>,
    store_root: Option<&Path>,
    mode: InstallMode,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
    report_replay_progress: impl FnMut(usize, usize),
) -> Result<InstallReport, OperationFailure> {
    run_with_manifest(
        project_dir,
        None,
        package,
        store_root,
        mode,
        registry_fixture,
        allow_unverified_registry_artifacts,
        report_replay_progress,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_manifest(
    project_dir: &Path,
    manifest_override: Option<&PackageManifest>,
    package: Option<&str>,
    store_root: Option<&Path>,
    mode: InstallMode,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
    report_replay_progress: impl FnMut(usize, usize),
) -> Result<InstallReport, OperationFailure> {
    run_with_manifest_target(
        project_dir,
        &project_dir.join("package.json"),
        manifest_override,
        package,
        store_root,
        mode,
        registry_fixture,
        allow_unverified_registry_artifacts,
        report_replay_progress,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_manifest_target(
    project_dir: &Path,
    target_manifest_path: &Path,
    manifest_override: Option<&PackageManifest>,
    package: Option<&str>,
    store_root: Option<&Path>,
    mode: InstallMode,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
    report_replay_progress: impl FnMut(usize, usize),
) -> Result<InstallReport, OperationFailure> {
    let mut session = InstallSession::new(project_dir);
    if allow_unverified_registry_artifacts && matches!(mode, InstallMode::Online) {
        session
            .outcome
            .warnings
            .push(Warning::UnverifiedRegistryArtifactsAllowed);
    }
    match perform_install(
        &mut session,
        target_manifest_path,
        manifest_override,
        package,
        store_root,
        mode,
        registry_fixture,
        allow_unverified_registry_artifacts,
        report_replay_progress,
    ) {
        Ok((package_count, replayed)) => Ok(InstallReport {
            package_count,
            replayed,
            outcome: session.outcome,
        }),
        Err(error) => Err(session.fail(error)),
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_install(
    session: &mut InstallSession,
    target_manifest_path: &Path,
    manifest_override: Option<&PackageManifest>,
    package: Option<&str>,
    store_root: Option<&Path>,
    mode: InstallMode,
    registry_fixture: Option<&Path>,
    allow_unverified_registry_artifacts: bool,
    report_replay_progress: impl FnMut(usize, usize),
) -> Result<(usize, bool), OperationalError> {
    let offline = matches!(mode, InstallMode::Offline);
    let frozen = matches!(mode, InstallMode::Frozen);
    if package.is_some() && (offline || frozen) {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "a package argument cannot be used with --offline or --frozen",
        ));
    }
    if allow_unverified_registry_artifacts && (offline || frozen) {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "--allow-unverified-registry-artifacts cannot be used with --offline or --frozen",
        ));
    }
    if manifest_override.is_some() && package.is_some() {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "cannot combine a manifest override with a package argument",
        ));
    }
    let project_dir = fs::canonicalize(&session.outcome.project_dir).map_err(|error| {
        OperationalError::from_source(ErrorKind::Project, error).context(format!(
            "cannot access project directory '{}'",
            session.outcome.project_dir.display()
        ))
    })?;
    if !project_dir.is_dir() {
        return Err(OperationalError::new(
            ErrorKind::Project,
            format!(
                "project directory is not a directory: {}",
                project_dir.display()
            ),
        ));
    }
    session.outcome.project_dir = project_dir.clone();
    let target_candidate = if target_manifest_path.is_absolute() {
        target_manifest_path.to_path_buf()
    } else {
        project_dir.join(target_manifest_path)
    };
    let target_metadata = fs::symlink_metadata(&target_candidate).map_err(|error| {
        OperationalError::from_source(ErrorKind::Manifest, error)
            .context("cannot read manifest: cannot inspect target package.json")
    })?;
    if !target_metadata.file_type().is_file() {
        return Err(OperationalError::new(
            ErrorKind::Manifest,
            "target package.json must be a regular, non-symlink file",
        ));
    }
    let manifest_path = fs::canonicalize(&target_candidate).map_err(|error| {
        OperationalError::from_source(ErrorKind::Manifest, error)
            .context("cannot resolve target package.json")
    })?;
    if !manifest_path.starts_with(&project_dir) {
        return Err(OperationalError::new(
            ErrorKind::Manifest,
            "target package.json must be contained beneath workspace root",
        ));
    }
    let preflight_manifest = read_manifest(&project_dir.join("package.json"))?;
    online::validate_manifest_roots(&project_dir, &preflight_manifest)
        .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?;
    if let Some(updated) = manifest_override {
        online::validate_manifest_roots(&project_dir, updated)
            .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?;
    }
    let lock_path = project_dir.join("tapid.lock");
    if (offline || frozen) && lock_path.is_file() {
        read_lock(&lock_path)?;
    }
    session.lock = Some(ActivationLock::acquire(&project_dir)?);
    let activation_lock = session.lock.as_ref().expect("project lock acquired");
    if activation_lock.recovered {
        session
            .outcome
            .warnings
            .push(Warning::PreviousTransactionRecovered);
    }
    if cfg!(debug_assertions) && std::env::var_os("TAPID_TEST_RECOVER_ONLY").is_some() {
        return Ok((0, false));
    }
    let current_manifest = read_manifest(&manifest_path)?;
    let original_manifest = fs::read(&manifest_path).map_err(|error| {
        OperationalError::from_source(ErrorKind::Transaction, error)
            .context("cannot preserve package.json for recovery")
    })?;
    let original_lock = match fs::read(&lock_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(OperationalError::from_source(ErrorKind::Transaction, error)
                .context("cannot preserve tapid.lock for recovery"));
        }
    };
    if offline || frozen || manifest_override.is_some() || package.is_some() {
        session.journal = Some(
            crate::filesystem::lifecycle_journal::LifecycleJournal::begin(
                &project_dir,
                &manifest_path,
                activation_lock.owner_line(),
                &original_manifest,
                original_lock.as_deref(),
            )
            .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?,
        );
    }
    let manifest = if let Some(updated) = manifest_override {
        updated.clone()
    } else if let Some(spec) = package {
        let (name, requirement) = package_spec::parse(spec);
        current_manifest
            .with_dependency(name, requirement)
            .map_err(|error| {
                OperationalError::from_source(ErrorKind::InvalidRequest, error)
                    .context(format!("cannot add dependency '{spec}'"))
            })?
    } else {
        current_manifest
    };
    if manifest_override.is_some() || package.is_some() {
        let bytes = manifest.to_json();
        if bytes.as_bytes() != original_manifest {
            session.outcome.changed_files.push(manifest_path.clone());
        }
        session.mutated = true;
        fs::write(&manifest_path, bytes).map_err(|error| {
            OperationalError::from_source(ErrorKind::Transaction, error)
                .context("cannot update package.json")
        })?;
        #[cfg(test)]
        outcome_tests::checkpoint(
            "manifest_written",
            &project_dir,
            activation_lock.owner_line(),
        )?;
    }
    let root_manifest_path = project_dir.join("package.json");
    let root_manifest = if manifest_path == root_manifest_path {
        manifest.clone()
    } else {
        read_manifest(&root_manifest_path)?
    };
    let store = Store::new(match store_root {
        Some(path) => path.to_owned(),
        None => {
            default_store_root().map_err(|error| OperationalError::new(ErrorKind::Store, error))?
        }
    });
    if !offline && !frozen {
        let registry_config = crate::registry::RegistryConfig::load(&project_dir)
            .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
        let (lock, mut input, trees, store_transaction, workspace_links) =
            online::resolve_and_fetch(
                &project_dir,
                &root_manifest,
                &store,
                registry_fixture,
                allow_unverified_registry_artifacts,
                &registry_config,
            )?;
        if session.journal.is_none() {
            session.journal = Some(
                crate::filesystem::lifecycle_journal::LifecycleJournal::begin(
                    &project_dir,
                    &manifest_path,
                    activation_lock.owner_line(),
                    &original_manifest,
                    original_lock.as_deref(),
                )
                .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?,
            );
        }
        let journal = session.journal.as_mut().expect("lifecycle journal created");
        journal
            .set_store_root(store.root())
            .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
        let lock_json = lock
            .to_json()
            .map_err(|error| OperationalError::from(error).context("cannot serialize lockfile"))?;
        session.mutated = true;
        let publication = store_transaction
            .publish_for_lifecycle(&journal.coordinator_path())
            .map_err(|error| {
                OperationalError::from(error).context("cannot publish verified store trees")
            })?;
        crate::filesystem::activation::test_crash_at("store_published");
        let trees = trees
            .into_iter()
            .map(|(key, path)| (key, publication.resolve_path(&path)))
            .collect();
        for instance in &mut input.instances {
            instance.tree.root = publication.resolve_path(&instance.tree.root);
        }
        if original_lock.as_deref() != Some(lock_json.as_bytes()) {
            session.outcome.changed_files.push(lock_path.clone());
        }
        let lock_backup = crate::filesystem::atomic::replace_lockfile(&lock_path, &lock_json)
            .map_err(|error| {
                OperationalError::new(ErrorKind::Transaction, error)
                    .context(format!("cannot replace lockfile {}", lock_path.display()))
            })?;
        session.lock_backup = lock_backup.clone();
        crate::filesystem::activation::test_crash_at("lockfile_replaced");
        session
            .outcome
            .changed_files
            .push(project_dir.join("node_modules"));
        if let Err(error) = materialize_install(
            &project_dir,
            input,
            trees,
            workspace_links,
            activation_lock,
            true,
        ) {
            if crate::filesystem::atomic::rollback_lockfile(&lock_path, lock_backup.as_deref())
                .is_ok()
            {
                session.lock_backup = None;
            }
            // The journal's explicit settlement verifies recovery even when these
            // immediate rollback attempts fail.
            let _ = publication.rollback();
            return Err(error);
        }
        crate::filesystem::activation::test_crash_at("activation_complete");
        journal
            .mark_committed()
            .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
        session.committed = true;
        session.outcome.state = ChangeState::Committed;
        crate::filesystem::activation::test_crash_at("commit_decision");
        publication.commit().map_err(|error| {
            OperationalError::from(error).context("cannot finalize verified store transaction")
        })?;
        #[cfg(test)]
        outcome_tests::checkpoint("after_commit", &project_dir, activation_lock.owner_line())?;
        crate::filesystem::atomic::discard_lockfile_backup(lock_backup.as_deref()).map_err(
            |error| {
                OperationalError::new(ErrorKind::Transaction, error)
                    .context("cannot discard lockfile backup")
            },
        )?;
        session.lock_backup = None;
        journal
            .finish()
            .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
        return Ok((lock.packages().len(), false));
    }
    if !lock_path.is_file() {
        return Err(OperationalError::new(
            ErrorKind::LockfileMissing,
            format!(
                "{} install requires tapid.lock: {}",
                if offline { "offline" } else { "frozen" },
                lock_path.display()
            ),
        ));
    }
    let lock = read_lock(&lock_path)?;
    let current_manifest_digest = fs::read(project_dir.join("package.json"))
        .map(|bytes| crate::filesystem::atomic::digest_bytes(&bytes))
        .map_err(|error| {
            OperationalError::from_source(ErrorKind::Manifest, error)
                .context("cannot read root manifest for lockfile replay")
        })?;
    lock.validate_replay(&current_manifest_digest)
        .map_err(|error| {
            OperationalError::from(error)
                .context(format!("invalid lockfile {}", lock_path.display()))
        })?;
    let registry_config = crate::registry::RegistryConfig::load(&project_dir)
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    let workspace = online::workspace_materialization(&project_dir, &registry_config)?;
    let current_workspace = workspace
        .locked
        .iter()
        .map(|package| (package.key(), package.manifest_digest().to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let locked_workspace = lock
        .workspace_packages()
        .iter()
        .map(|(key, package)| (key.clone(), package.manifest_digest().to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if current_workspace != locked_workspace {
        return Err(
            "workspace membership or member manifest changed; regenerate tapid.lock with an online install"
                .into(),
        );
    }
    let workspace_registry_dependencies = online::resolved_workspace_registry_dependencies(
        &root_manifest,
        &workspace,
        &registry_config,
    )?;
    validate_workspace_dependency_edges(&workspace, &workspace_registry_dependencies, &lock)?;
    store.recover_transactions().map_err(|error| {
        OperationalError::from(error).context("cannot prepare shared store for recovery")
    })?;
    let journal = session.journal.as_mut().expect("replay journal created");
    journal
        .set_store_root(store.root())
        .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
    let (input, trees) = crate::application::replay::replay_input(
        &lock,
        &root_manifest,
        &store,
        &registry_config,
        report_replay_progress,
    )?;
    session.mutated = true;
    session
        .outcome
        .changed_files
        .push(project_dir.join("node_modules"));
    materialize_with_lock(
        &project_dir,
        input,
        trees,
        workspace.links,
        true,
        activation_lock,
        true,
    )?;
    journal
        .mark_committed()
        .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
    session.committed = true;
    session.outcome.state = ChangeState::Committed;
    journal
        .finish()
        .map_err(|error| OperationalError::new(ErrorKind::Transaction, error))?;
    Ok((lock.packages().len(), true))
}

fn read_lock(path: &Path) -> Result<Lockfile, OperationalError> {
    let bytes = fs::read_to_string(path).map_err(|error| {
        OperationalError::from_source(ErrorKind::Lockfile, error)
            .context(format!("invalid lockfile {}", path.display()))
    })?;
    Lockfile::from_json(&bytes).map_err(|error| {
        OperationalError::from(error).context(format!("invalid lockfile {}", path.display()))
    })
}

fn validate_workspace_dependency_edges(
    workspace: &online::WorkspaceMaterialization,
    registry_dependencies: &[online::WorkspaceRegistryDependency],
    lock: &Lockfile,
) -> Result<(), String> {
    for current in &workspace.locked {
        let key = current.key();
        let locked = lock
            .workspace_packages()
            .get(&key)
            .ok_or_else(|| format!("lockfile is missing workspace package {key}"))?;
        let mut expected_names = std::collections::BTreeSet::new();
        for (name, target) in current.dependencies() {
            expected_names.insert(name.clone());
            if locked.dependencies().get(name) != Some(target) {
                return Err(format!(
                    "lockfile workspace dependency '{name}' for {key} does not match the current local workspace graph"
                ));
            }
            let target_key = target
                .parse::<tapid_lockfile::LockfilePackageKey>()
                .map_err(|error| error.to_string())?;
            if target_key.source.workspace().is_none()
                || !lock.workspace_packages().contains_key(target)
            {
                return Err(format!(
                    "lockfile workspace dependency '{name}' for {key} does not target a workspace package"
                ));
            }
        }
        for dependency in registry_dependencies
            .iter()
            .filter(|dependency| dependency.member_key == key)
        {
            let name = dependency.manifest_name.clone();
            expected_names.insert(name.clone());
            let target = locked.dependencies().get(&name).ok_or_else(|| {
                format!(
                    "lockfile omits workspace member dependency '{}' from {key}",
                    dependency.manifest_name
                )
            })?;
            let target_key = target
                .parse::<tapid_lockfile::LockfilePackageKey>()
                .map_err(|error| error.to_string())?;
            if target_key.source.registry() != Some(&dependency.registry)
                || target_key.name != dependency.package
                || !dependency.requirement.matches(&target_key.version)
                || !lock.packages().contains_key(target)
            {
                return Err(format!(
                    "lockfile target for workspace member dependency '{}' does not satisfy its registry, name, and version requirement",
                    dependency.manifest_name
                ));
            }
        }
        let actual_names = locked
            .dependencies()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if actual_names != expected_names {
            return Err(format!(
                "lockfile workspace dependency edges for {key} do not match the current member manifest"
            ));
        }
    }
    for peer in &workspace.peer_dependencies {
        let provider_found = match &peer.provider {
            online::WorkspacePeerProvider::Workspace { key, version } => {
                let target_key = key
                    .parse::<tapid_lockfile::LockfilePackageKey>()
                    .map_err(|error| error.to_string())?;
                target_key.version == *version
                    && lock.workspace_packages().contains_key(key)
                    && workspace.locked.iter().any(|member| member.key() == *key)
            }
            online::WorkspacePeerProvider::Registry { registry, package } => {
                lock.roots().iter().any(|key| {
                    key.parse::<tapid_lockfile::LockfilePackageKey>()
                        .ok()
                        .is_some_and(|target| {
                            target.source.registry() == Some(registry)
                                && target.name == *package
                                && peer.requirement.matches(&target.version)
                                && lock.packages().contains_key(key)
                        })
                })
            }
        };
        if !provider_found {
            return Err(format!(
                "lockfile has no provider satisfying workspace member peer dependency '{}' from {}",
                peer.manifest_name, peer.member_key
            ));
        }
    }
    Ok(())
}

fn materialize_install(
    project_dir: &Path,
    input: NamedLayoutInput,
    trees: BTreeMap<String, PathBuf>,
    workspace_links: tapid_linker::WorkspaceLinkPlan,
    activation_lock: &ActivationLock,
    preserve_previous: bool,
) -> Result<(), OperationalError> {
    materialize_with_lock(
        project_dir,
        input,
        trees,
        workspace_links,
        false,
        activation_lock,
        preserve_previous,
    )
}

fn materialize_with_lock(
    project_dir: &Path,
    input: NamedLayoutInput,
    trees: BTreeMap<String, PathBuf>,
    workspace_links: tapid_linker::WorkspaceLinkPlan,
    replayed: bool,
    activation_lock: &ActivationLock,
    preserve_previous: bool,
) -> Result<(), OperationalError> {
    let root = match ManagedRoot::new(project_dir) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(OperationalError::from_source(
                ErrorKind::Materialization,
                error,
            ));
        }
    };
    let platform = crate::application::replay::current_platform();
    let plan = match plan_named_layout(root, input.clone(), platform) {
        Ok(value) => value,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(OperationalError::from_source(
                ErrorKind::Materialization,
                error,
            ));
        }
    };
    let stage = match activation_lock.create_stage(project_dir) {
        Ok(stage) => stage,
        Err(error) => {
            if replayed {
                crate::application::replay::cleanup_replay_snapshots(&trees);
            }
            return Err(OperationalError::new(ErrorKind::Materialization, error));
        }
    };
    let result = crate::filesystem::tree::materialize_stage_with_workspace_links(
        &stage,
        &plan,
        &input.instances,
        &trees,
        replayed,
        &workspace_links,
    )
    .and_then(|_| {
        crate::filesystem::activation::activate_node_modules_with_preflight(
            project_dir,
            &stage,
            activation_lock,
            preserve_previous,
            || {
                crate::filesystem::tree::validate_workspace_links(
                    project_dir,
                    &stage,
                    &workspace_links,
                )
            },
        )
    });
    if replayed {
        crate::application::replay::cleanup_replay_snapshots(&trees);
    }
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&stage);
        return Err(OperationalError::new(ErrorKind::Materialization, error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn environment<'a>(
        entries: &'a [(&'a str, &'a str)],
    ) -> impl FnMut(&str) -> Option<OsString> + 'a {
        move |name| {
            entries
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn default_store_is_outside_a_macos_consumer_project() {
        let store =
            default_store_root_for("macos", environment(&[("HOME", "/Users/example")])).unwrap();

        assert_eq!(
            store,
            PathBuf::from("/Users/example/Library/Caches/tapid/store")
        );
        assert!(!store.starts_with("/Users/example/source/application"));
    }

    #[test]
    fn linux_default_store_prefers_absolute_xdg_cache_home() {
        let store = default_store_root_for(
            "linux",
            environment(&[
                ("XDG_CACHE_HOME", "/cache/example"),
                ("HOME", "/home/example"),
            ]),
        )
        .unwrap();

        assert_eq!(store, PathBuf::from("/cache/example/tapid/store"));
    }

    #[test]
    fn windows_default_store_uses_local_application_data() {
        let store = default_store_root_for(
            "windows",
            environment(&[("LOCALAPPDATA", "C:\\Users\\example\\AppData\\Local")]),
        )
        .unwrap();

        assert_eq!(
            store,
            PathBuf::from("C:\\Users\\example\\AppData\\Local").join("tapid/store")
        );
    }

    #[test]
    fn relative_cache_environment_is_rejected() {
        let error = default_store_root_for(
            "linux",
            environment(&[
                ("XDG_CACHE_HOME", "relative/cache"),
                ("HOME", "/home/example"),
            ]),
        )
        .unwrap_err();

        assert!(error.contains("absolute"));
    }
}

#[cfg(test)]
#[path = "install_outcome_tests.rs"]
mod outcome_tests;
