use crate::{
    application::outcome::{ErrorKind, OperationalError},
    filesystem::{
        activation::ActivationLock,
        read::{ReadError, read_bounded_file},
    },
    registry::RegistryConfig,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tapid_lockfile::Lockfile;
use tapid_manifest::Workspace;

const MAX_PROJECT_FILE: usize = 32 * 1024 * 1024;
const MAX_MEMBERS: usize = 4096;
const MAX_EXPANSION: usize = 65536;

/// Retained existing-state coordination plus bounded project observations.
/// Consumers must still authenticate and recheck the complete live layout,
/// lifecycle provenance and runtime before launch. No recovery is performed.
pub(crate) struct ProjectEvidence {
    pub root: PathBuf,
    pub workspace: Workspace,
    pub lock: Lockfile,
    imported: Option<tapid_lockfile::ImportedNpmLockfile>,
    pub config: RegistryConfig,
    pub store: tapid_store::StoreReadGuard,
    observations: BTreeMap<PathBuf, Option<Vec<u8>>>,
    _activation: Vec<ActivationLock>,
}

fn failure(kind: ErrorKind, message: impl std::fmt::Display) -> OperationalError {
    OperationalError::new(kind, message)
}

fn observe_file(
    path: &Path,
    limit: usize,
    optional: bool,
) -> Result<Option<Vec<u8>>, OperationalError> {
    match read_bounded_file(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(ReadError::Io(error)) if optional && error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling link is evidence of an unsafe input, not absence.
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                _ => Err(failure(ErrorKind::Project, "unsafe optional project input")),
            }
        }
        Err(error) => Err(failure(
            ErrorKind::Project,
            format!("cannot observe {}: {error:?}", path.display()),
        )),
    }
}

impl ProjectEvidence {
    pub(crate) fn run_config(&self) -> Result<tapid_runner::RunConfig, tapid_runner::ConfigError> {
        tapid_runner::RunConfig::parse_toml_bytes(
            self.observations[&self.root.join("tapid.toml")]
                .as_deref()
                .unwrap_or_default(),
        )
    }

    pub(crate) fn select_command(
        &self,
        member: Option<&str>,
        package: Option<&str>,
        command: &str,
    ) -> Result<super::live::LiveCommand, super::selection::SelectionError> {
        if self.imported.is_some() {
            self.select_imported_command(member, package, command)
        } else {
            self.select_native_command(member, package, command)
        }
    }
    /// Reconstruct all evidence immediately before checked execution and retain
    /// the originally selected owner and entry. Coordination does not exclude
    /// malicious host pathname races after this final observation.
    pub(crate) fn revalidate_command(
        &self,
        member: Option<&str>,
        package: Option<&str>,
        command: &str,
        expected: &super::live::LiveCommand,
    ) -> Result<(), super::selection::SelectionError> {
        let current = self.select_command(member, package, command)?;
        if current.selection != expected.selection || current.entry != expected.entry {
            return Err(super::selection::SelectionError::InconsistentInstallation(
                "selected command changed before launch".into(),
            ));
        }
        Ok(())
    }

    /// Verify native-lock materialization, including all owned member layouts.
    /// Authenticates content and lifecycle provenance; execution policy and
    /// runtime binding must still be established before checked launch.
    pub(super) fn verify_native_layout(
        &self,
    ) -> Result<
        (
            tapid_linker::NamedLayoutInput,
            crate::online::WorkspaceInstallPlan,
        ),
        OperationalError,
    > {
        self.revalidate()?;
        if self.imported.is_some() {
            return Err(failure(
                ErrorKind::Lockfile,
                "imported locks require imported-layout verification",
            ));
        }
        let (input, _) =
            super::installed_input(&self.lock, self.workspace.root(), &self.store, &self.config)?;
        let local = crate::online::workspace_materialization_from_discovered(
            &self.root,
            &self.config,
            None,
            &self.workspace,
        )?;
        let mut workspace_plan = crate::online::WorkspaceInstallPlan::from(local.links);
        workspace_plan
            .include_owned_members()
            .map_err(|e| failure(ErrorKind::Materialization, e))?;
        self.verify_layout(&input, &workspace_plan)?;
        Ok((input, workspace_plan))
    }

    pub(super) fn verify_imported_layout(
        &self,
    ) -> Result<
        (
            tapid_linker::NamedLayoutInput,
            crate::online::WorkspaceInstallPlan,
        ),
        OperationalError,
    > {
        self.revalidate()?;
        let imported = self
            .imported
            .as_ref()
            .ok_or_else(|| failure(ErrorKind::Lockfile, "expected an imported lock"))?;
        let observed =
            crate::online::observe_imported_artifacts(imported, &self.store, &self.config)?;
        let local = crate::online::workspace_materialization_from_discovered(
            &self.root,
            &self.config,
            None,
            &self.workspace,
        )?;
        let graph = imported
            .graph()
            .map_err(|e| failure(ErrorKind::Lockfile, e))?;
        let (input, workspace_plan) = crate::online::imported_layout(
            &self.root,
            local.links,
            &graph,
            &observed.input,
            &observed.placements,
        )?;
        self.verify_layout(&input, &workspace_plan)?;
        Ok((input, workspace_plan))
    }

    fn verify_layout(
        &self,
        input: &tapid_linker::NamedLayoutInput,
        workspace_plan: &crate::online::WorkspaceInstallPlan,
    ) -> Result<(), OperationalError> {
        self.verify_lifecycle_provenance()?;
        // Even an unowned member tree can affect Node lookup from mutable
        // workspace source. Never let it escape verification through omission.
        for member in self.workspace.members() {
            let root = member
                .path()
                .parent()
                .ok_or_else(|| failure(ErrorKind::Project, "member has no parent"))?;
            match std::fs::symlink_metadata(root.join("node_modules")) {
                Ok(_) => crate::filesystem::activation::validate_managed_node_modules(root)
                    .map_err(|e| failure(ErrorKind::Materialization, e))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(failure(ErrorKind::Materialization, e)),
            }
        }
        let verify = |project: &Path,
                      input: &tapid_linker::NamedLayoutInput,
                      links: &tapid_linker::WorkspaceLinkPlan| {
            crate::filesystem::activation::validate_managed_node_modules(project)
                .map_err(|e| failure(ErrorKind::Materialization, e))?;
            let plan = tapid_linker::plan_workspace_layout(
                tapid_linker::ManagedRoot::new(project)
                    .map_err(|e| failure(ErrorKind::Materialization, e))?,
                input.clone(),
                links,
                crate::application::replay::current_platform(),
            )
            .map_err(|e| failure(ErrorKind::Materialization, e))?;
            crate::filesystem::tree::verification::verify_materialized_layout(
                project,
                &self.root,
                &plan,
                &input.instances,
                links,
            )
            .map_err(|e| failure(ErrorKind::Materialization, e))
        };
        verify(&self.root, input, &workspace_plan.links)?;
        for (root, member) in &workspace_plan.members {
            verify(root, &member.input, &member.links)?;
        }
        self.revalidate()?;
        Ok(())
    }

    fn verify_lifecycle_provenance(&self) -> Result<(), OperationalError> {
        self.revalidate()?;
        let policy = self.observations[&self.root.join("tapid.lifecycle.toml")]
            .as_deref()
            .map(tapid_runner::DependencyLifecyclePolicy::parse)
            .transpose()
            .map_err(|e| failure(ErrorKind::InvalidRequest, e))?
            .unwrap_or_else(tapid_runner::DependencyLifecyclePolicy::denied);
        crate::application::dependency_scripts::provenance::verify_installed_provenance(
            &self.lock,
            &policy,
            &self.store,
            || {
                tapid_runner::dependency_toolchain_identity()
                    .map_err(|e| failure(ErrorKind::InvalidRequest, e))
            },
        )?;
        self.revalidate()
    }

    /// Select against the same root/member composition used by imported replay.
    /// Manifest declarations restrict eligibility even when npm hoisted another
    /// member's dependencies into the root installation.
    pub(super) fn select_imported_command(
        &self,
        member: Option<&str>,
        package: Option<&str>,
        command: &str,
    ) -> Result<super::live::LiveCommand, super::selection::SelectionError> {
        use super::selection::SelectionError;
        let manifest = self
            .workspace
            .select(member)
            .map_err(SelectionError::InvalidProjectSelection)?;
        let (root_input, workspace_plan) = self
            .verify_imported_layout()
            .map_err(SelectionError::ProjectEvidence)?;
        let project = self
            .workspace
            .select_path(member)
            .map_err(SelectionError::InvalidProjectSelection)?
            .parent()
            .ok_or_else(|| {
                SelectionError::InconsistentInstallation(
                    "selected manifest has no directory".into(),
                )
            })?;
        let (project, input, links) = if let Some(layout) = workspace_plan.members.get(project) {
            (project, &layout.input, &layout.links)
        } else {
            // Imported replay omits member layouts when all of the member's
            // bindings resolve at the root. Do not create one during selection.
            (self.root.as_path(), &root_input, &workspace_plan.links)
        };
        // A member may use a workspace link installed at the root. Preserve the
        // exact link identity and bind it against its actual containing layout.
        let mut eligible_links = links.clone();
        for link in &workspace_plan.links.links {
            if !eligible_links
                .links
                .iter()
                .any(|local| local.name == link.name)
            {
                eligible_links.links.push(link.clone());
            }
        }
        let selection = super::selection::select_manifest_command(
            input,
            &eligible_links,
            manifest,
            package,
            command,
            crate::application::replay::current_platform(),
        )?;
        let (project, input, links) = if matches!(&selection.owner,
            super::selection::CommandOwner::Workspace(link)
                if !links.links.contains(link))
        {
            (self.root.as_path(), &root_input, &workspace_plan.links)
        } else {
            (project, input, links)
        };
        let plan = tapid_linker::plan_workspace_layout(
            tapid_linker::ManagedRoot::new(project).map_err(SelectionError::InvalidMapping)?,
            input.clone(),
            links,
            crate::application::replay::current_platform(),
        )
        .map_err(SelectionError::InvalidMapping)?;
        let selected =
            super::live::bind_live_command(project, &self.root, input, &plan, links, selection)?;
        self.revalidate().map_err(SelectionError::ProjectEvidence)?;
        Ok(selected)
    }

    /// Compose exact root/member selection with complete native-layout evidence.
    /// Retaining `self` coordinates observations; this still grants no launch
    /// authority and does not establish lifecycle approval or runtime identity.
    pub(super) fn select_native_command(
        &self,
        member: Option<&str>,
        package: Option<&str>,
        command: &str,
    ) -> Result<super::live::LiveCommand, super::selection::SelectionError> {
        use super::selection::SelectionError;
        let manifest = self
            .workspace
            .select(member)
            .map_err(SelectionError::InvalidProjectSelection)?;
        let (input, workspace_plan) = self
            .verify_native_layout()
            .map_err(SelectionError::ProjectEvidence)?;
        // Native installation places registry/copied bindings at the root,
        // including member dependencies. Eligibility uses the selected manifest;
        // physical placement and verification continue to use the complete plan.
        let plan = tapid_linker::plan_workspace_layout(
            tapid_linker::ManagedRoot::new(&self.root).map_err(SelectionError::InvalidMapping)?,
            input.clone(),
            &workspace_plan.links,
            crate::application::replay::current_platform(),
        )
        .map_err(SelectionError::InvalidMapping)?;
        let mut bindings = if let Some(member) = member {
            self.lock
                .workspace_packages_typed()
                .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?
                .into_iter()
                .find(|(key, _)| {
                    key.source
                        .workspace()
                        .is_some_and(|source| source.name() == member)
                })
                .ok_or_else(|| {
                    SelectionError::InconsistentInstallation(
                        "missing selected member receipt".into(),
                    )
                })?
                .1
                .dependencies()
                .clone()
        } else if !self.lock.root_bindings().is_empty() {
            self.lock.root_bindings().clone()
        } else {
            // Legacy roots have no alias bindings. Existing replay validation
            // has already rejected forms without one exact reconstruction.
            let mut keys = self
                .lock
                .packages_typed()
                .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>();
            keys.extend(
                self.lock
                    .workspace_packages_typed()
                    .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?
                    .into_iter()
                    .map(|(key, _)| key),
            );
            let roots = crate::application::replay::replay_root_keys_with_config(
                &self.lock,
                self.workspace.root(),
                &keys,
                &self.config,
            )
            .map_err(SelectionError::InconsistentInstallation)?;
            roots
                .iter()
                .map(|target| {
                    let key = target
                        .parse::<tapid_lockfile::LockfilePackageKey>()
                        .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?;
                    Ok((key.name.to_string(), target.clone()))
                })
                .collect::<Result<BTreeMap<_, _>, SelectionError>>()?
        };
        // Workspace roots are represented by roots, independently of immutable
        // root alias bindings. Observation already validated their declarations.
        if member.is_none() {
            for target in self.lock.roots() {
                let key = target
                    .parse::<tapid_lockfile::LockfilePackageKey>()
                    .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?;
                if let Some(source) = key.source.workspace() {
                    bindings.insert(source.name().into(), target.clone());
                }
            }
        }
        let mut allowed = std::collections::BTreeSet::new();
        let mut eligible_links = tapid_linker::WorkspaceLinkPlan::default();
        for (name, target) in bindings {
            let key = target
                .parse::<tapid_lockfile::LockfilePackageKey>()
                .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?;
            if let Some(source) = key.source.package_source() {
                let owner = tapid_linker::InstanceKey {
                    id: tapid_core::PackageInstanceId::from_source(
                        source,
                        key.name.clone(),
                        key.version.clone(),
                    ),
                    peer_context: key
                        .peer_context()
                        .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?,
                    platform_context: key
                        .platform_context()
                        .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?,
                };
                allowed.insert((name, owner));
            } else if let Some(source) = key.source.workspace() {
                let expected = self.root.join(source.path());
                let link = workspace_plan
                    .links
                    .links
                    .iter()
                    .find(|link| {
                        link.name.as_str() == name
                            && link.name.as_str() == source.name()
                            && link.version.to_string() == source.version()
                            && link.source == expected
                    })
                    .ok_or_else(|| {
                        SelectionError::InconsistentInstallation(
                            "workspace binding has no exact managed link".into(),
                        )
                    })?;
                eligible_links.links.push(link.clone());
            }
        }
        let mut eligible_input = input.clone();
        eligible_input
            .root_dependencies
            .retain(|binding| allowed.contains(&(binding.name.to_string(), binding.child.clone())));
        let selection = super::selection::select_manifest_command(
            &eligible_input,
            &eligible_links,
            manifest,
            package,
            command,
            crate::application::replay::current_platform(),
        )?;
        let selected = super::live::bind_live_command(
            &self.root,
            &self.root,
            &input,
            &plan,
            &workspace_plan.links,
            selection,
        )?;
        self.revalidate().map_err(SelectionError::ProjectEvidence)?;
        Ok(selected)
    }

    pub(crate) fn observe(
        project: &Path,
        store: &tapid_store::Store,
    ) -> Result<Self, OperationalError> {
        let root = std::fs::canonicalize(project).map_err(|e| failure(ErrorKind::Project, e))?;
        let mut activation = vec![ActivationLock::acquire_existing(&root)?];
        let mut observations = BTreeMap::new();
        for (name, limit, optional) in [
            ("package.json", tapid_manifest::MAX_MANIFEST_BYTES, false),
            ("tapid.lock", MAX_PROJECT_FILE, false),
            ("tapid.toml", 64 * 1024, true),
            (
                "tapid.lifecycle.toml",
                tapid_runner::config::MAX_CONFIG_BYTES,
                true,
            ),
        ] {
            let path = root.join(name);
            observations.insert(path.clone(), observe_file(&path, limit, optional)?);
        }
        let workspace = Workspace::discover_bounded_with_member_source(
            &root,
            MAX_MEMBERS,
            MAX_EXPANSION,
            |path, source| {
                observations.insert(path.to_path_buf(), Some(source.as_bytes().to_vec()));
            },
        )
        .map_err(|e| failure(ErrorKind::Manifest, e))?;
        // Keep a deterministic order, acquire without waiting, and acquire all
        // project locks before the store guard, as installation does.
        let mut members = workspace
            .members()
            .iter()
            .map(|member| {
                member
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| failure(ErrorKind::Project, "member has no parent"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        members.sort();
        for member in members {
            crate::filesystem::lifecycle_journal::validate_member_root(&root, &member)
                .map_err(|e| failure(ErrorKind::Project, e))?;
            crate::filesystem::activation::validate_no_interrupted_state(&member)?;
            // Root-managed member activation uses the root lock and journal.
            // A separately installed member may also have its own lock;
            // retain it if present, but never initialize one for inspection.
            let path = member.join(".tapid-activation.lock");
            let observed = observe_file(&path, 4096, true)?;
            if observed.is_some() {
                activation.push(ActivationLock::acquire_existing(&member)?);
            }
            observations.insert(path, observed);
        }
        let config = observations[&root.join("tapid.toml")]
            .as_deref()
            .map(RegistryConfig::parse_toml_bytes)
            .transpose()
            .map_err(|e| failure(ErrorKind::RegistryConfiguration, e))?
            .unwrap_or_default();
        let parsed = crate::application::install::parse_project_lock(
            observations[&root.join("tapid.lock")].as_deref().unwrap(),
            &root.join("tapid.lock"),
        )?;
        let store = store.existing_read_guard()?;
        let local = crate::online::workspace_materialization_from_discovered(
            &root, &config, None, &workspace,
        )?;
        let (lock, imported) = match parsed {
            crate::application::install::InstallLock::Native(lock) => (lock, None),
            crate::application::install::InstallLock::Imported(imported) => {
                let manifest = observations[&root.join("package.json")].as_deref().unwrap();
                imported
                    .validate_replay(
                        std::str::from_utf8(manifest)
                            .map_err(|e| failure(ErrorKind::Manifest, e))?,
                        &crate::filesystem::atomic::digest_bytes(manifest),
                    )
                    .map_err(|e| failure(ErrorKind::LockManifestMismatch, e))?;
                imported
                    .validate_workspace_manifests(&crate::online::imported_workspace_manifests(
                        &root, &local,
                    )?)
                    .map_err(|e| failure(ErrorKind::LockManifestMismatch, e))?;
                let observed =
                    crate::online::observe_imported_artifacts(&imported, &store, &config)?;
                (observed.lock, Some(imported))
            }
        };
        if imported.is_none() {
            lock.validate_replay(&crate::filesystem::atomic::digest_bytes(
                observations[&root.join("package.json")].as_deref().unwrap(),
            ))
            .map_err(|e| {
                let kind = if matches!(
                    e,
                    tapid_lockfile::LockfileError::RootManifestDigestMismatch { .. }
                ) {
                    ErrorKind::LockManifestMismatch
                } else {
                    ErrorKind::Lockfile
                };
                failure(kind, e)
            })?;
            crate::application::replay::validate_root_bindings(&lock, workspace.root(), &config)
                .map_err(|e| failure(ErrorKind::Lockfile, e))?;
            if !crate::online::workspace_matches_lock(&local, &lock) {
                return Err(failure(
                    ErrorKind::Lockfile,
                    "workspace receipts differ from current manifests",
                ));
            }
            let dependencies = crate::online::resolved_workspace_registry_dependencies(
                workspace.root(),
                &local,
                &config,
            )?;
            crate::application::replay::validate_workspace_dependency_edges(
                &local,
                &dependencies,
                &lock,
            )
            .map_err(|e| failure(ErrorKind::Lockfile, e))?;
        }
        let evidence = Self {
            root,
            workspace,
            lock,
            imported,
            config,
            store,
            observations,
            _activation: activation,
        };
        evidence.revalidate()?;
        Ok(evidence)
    }

    /// Recheck file bytes and bounded workspace membership, including newly
    /// appearing optional configuration. Does not claim held native file identity.
    pub(super) fn revalidate(&self) -> Result<(), OperationalError> {
        for (path, expected) in &self.observations {
            if observe_file(path, MAX_PROJECT_FILE, expected.is_none())? != *expected {
                return Err(failure(
                    ErrorKind::ProjectChanged,
                    "project inputs changed during inspection",
                ));
            }
        }
        for member in self.workspace.members() {
            let root = member
                .path()
                .parent()
                .ok_or_else(|| failure(ErrorKind::ProjectChanged, "member has no parent"))?;
            crate::filesystem::lifecycle_journal::validate_member_root(&self.root, root)
                .map_err(|e| failure(ErrorKind::ProjectChanged, e))?;
            crate::filesystem::activation::validate_no_interrupted_state(root)?;
        }
        let current = Workspace::discover_bounded(&self.root, MAX_MEMBERS, MAX_EXPANSION)
            .map_err(|e| failure(ErrorKind::ProjectChanged, e))?;
        if current
            .members()
            .iter()
            .map(|m| (m.path(), m.name()))
            .collect::<Vec<_>>()
            != self
                .workspace
                .members()
                .iter()
                .map(|m| (m.path(), m.name()))
                .collect::<Vec<_>>()
        {
            return Err(failure(
                ErrorKind::ProjectChanged,
                "workspace membership changed during inspection",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "project/imported_tests.rs"]
mod imported_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::{TempHome, TempProject};

    #[test]
    fn command_preparation_rejects_revoked_lifecycle_approval_before_layout() {
        let project = TempProject::new("exec-revoked-policy").unwrap();
        let home = TempHome::new("exec-revoked-store").unwrap();
        let source = TempProject::new("exec-revoked-source").unwrap();
        source
            .write(
                "package.json",
                br#"{"name":"demo","version":"1.0.0","scripts":{"postinstall":"echo approved"}}"#,
            )
            .unwrap();
        let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let store = tapid_store::Store::new(home.path().join("store"));
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&digest.parse().unwrap(), source.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let recipe = format!("sha256-{}", "b".repeat(64));
        let attestation = store
            .attest_lifecycle_output(&recipe.parse().unwrap(), &digest.parse().unwrap())
            .unwrap();
        let manifest = br#"{"name":"root","version":"1.0.0","dependencies":{"demo":"1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org", "demo", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", &digest,
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        ).unwrap();
        let key = package.key();
        let mut lock = Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest)).unwrap();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
        lock.set_derived_hooks(
            &key,
            vec![
                tapid_lockfile::DerivedHookOutput::new(
                    "postinstall",
                    &recipe,
                    &digest,
                    &attestation,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let bytes = lock.to_json().unwrap();
        project.write("tapid.lock", bytes.as_bytes()).unwrap();
        drop(ActivationLock::acquire(project.path()).unwrap());
        let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
        let failure = evidence.verify_native_layout().err().unwrap();
        assert!(
            failure.to_string().contains("approval was removed"),
            "{failure}"
        );
        assert_eq!(
            std::fs::read(project.path().join("tapid.lock")).unwrap(),
            bytes.as_bytes()
        );
        assert!(!project.path().join("node_modules").exists());
        assert!(!project.path().join("tapid.lifecycle.toml").exists());
    }

    #[test]
    fn root_coordinated_workspace_observation_never_creates_member_lock_and_rechecks_state() {
        let project = TempProject::new("exec-member-observation").unwrap();
        let home = TempHome::new("exec-member-store").unwrap();
        let manifest = br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"workspace:*"}}"#;
        project.write("package.json", manifest).unwrap();
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"1.0.0","bin":{"local-tool":"cli.js"}}"#,
            )
            .unwrap();
        project
            .write("packages/member/cli.js", b"console.log(1);\n")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                project.path().join("packages/member/cli.js"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        project
            .write(
                "packages/consumer/package.json",
                br#"{"name":"consumer","version":"1.0.0","dependencies":{"member":"workspace:*"}}"#,
            )
            .unwrap();
        let workspace =
            Workspace::discover_bounded(project.path(), MAX_MEMBERS, MAX_EXPANSION).unwrap();
        let local = crate::online::workspace_materialization_from_discovered(
            project.path(),
            &RegistryConfig::default(),
            None,
            &workspace,
        )
        .unwrap();
        let mut lock = Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest)).unwrap();
        let roots = local
            .locked
            .iter()
            .map(|member| member.key())
            .collect::<Vec<_>>();
        lock.insert_graph([], local.locked).unwrap();
        lock.set_roots(roots).unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        let store = tapid_store::Store::new(home.path().join("store"));
        drop(store.read_guard().unwrap());
        drop(ActivationLock::acquire(project.path()).unwrap());
        let member = project.path().join("packages/member");
        project
            .write("packages/member/.tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
        assert!(!member.join(".tapid-activation.lock").exists());
        assert!(ActivationLock::acquire_existing(project.path()).is_err());
        let (input, trees) = super::super::installed_input(
            &evidence.lock,
            evidence.workspace.root(),
            &evidence.store,
            &evidence.config,
        )
        .unwrap();
        let plan = tapid_linker::plan_workspace_layout(
            tapid_linker::ManagedRoot::new(&evidence.root).unwrap(),
            input.clone(),
            &local.links,
            crate::application::replay::current_platform(),
        )
        .unwrap();
        let stage = evidence.root.join(".fixture-stage");
        crate::filesystem::tree::materialize_stage_with_workspace_links(
            &stage,
            &plan,
            &input.instances,
            &trees,
            false,
            &local.links,
            &evidence.root,
            |_, _| {},
        )
        .unwrap();
        std::fs::rename(
            stage.join("node_modules"),
            evidence.root.join("node_modules"),
        )
        .unwrap();
        std::fs::remove_dir(stage).unwrap();
        project
            .write(".tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        let empty = tapid_linker::NamedLayoutInput {
            instances: vec![],
            root_dependencies: vec![],
            dependency_edges: vec![],
        };
        let member_plan = tapid_linker::plan_named_layout(
            tapid_linker::ManagedRoot::new(&member).unwrap(),
            empty,
            crate::application::replay::current_platform(),
        )
        .unwrap();
        crate::filesystem::tree::materialize_stage(
            &member,
            &member_plan,
            &[],
            &BTreeMap::new(),
            false,
        )
        .unwrap();
        evidence.verify_native_layout().unwrap();
        let member_selected = evidence
            .select_native_command(Some("consumer"), Some("member"), "local-tool")
            .unwrap();
        assert_eq!(
            member_selected.entry,
            evidence.root.join("packages/member/cli.js")
        );
        let selected = evidence
            .select_native_command(None, Some("member"), "local-tool")
            .unwrap();
        assert_eq!(selected.entry, evidence.root.join("packages/member/cli.js"));
        assert!(matches!(
            selected.selection.owner,
            super::super::selection::CommandOwner::Workspace(_)
        ));
        assert!(matches!(
            evidence.select_native_command(Some("member"), Some("member"), "local-tool"),
            Err(super::super::selection::SelectionError::InvalidPackageSelection(_))
        ));
        // The provenance deliberately does not authenticate mutable source bytes.
        project
            .write("packages/member/cli.js", b"console.log(2);\n")
            .unwrap();
        evidence
            .select_native_command(None, Some("member"), "local-tool")
            .unwrap();
        project
            .write("packages/member/node_modules/unplanned.js", b"unexpected")
            .unwrap();
        assert!(evidence.verify_native_layout().is_err());
        std::fs::remove_file(member.join("node_modules/unplanned.js")).unwrap();
        evidence.verify_native_layout().unwrap();
        // An independent member operation starting after observation is visible.
        drop(ActivationLock::acquire(&member).unwrap());
        assert!(evidence.revalidate().is_err());
        drop(evidence);
        let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
        assert!(ActivationLock::acquire_existing(&member).is_err());
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"2.0.0"}"#,
            )
            .unwrap();
        assert!(evidence.revalidate().is_err());
        drop(evidence);
        project
            .write(
                "packages/member/.tapid-install-stage-interrupted",
                b"pending",
            )
            .unwrap();
        assert!(ProjectEvidence::observe(project.path(), &store).is_err());
        assert_eq!(
            std::fs::read(member.join(".tapid-install-stage-interrupted")).unwrap(),
            b"pending"
        );
    }

    #[test]
    fn aliased_project_command_is_bound_under_retained_project_and_store_guards() {
        let project = TempProject::new("exec-aliased-project").unwrap();
        let source = TempProject::new("exec-aliased-reference").unwrap();
        let home = TempHome::new("exec-aliased-home").unwrap();
        source
            .write(
                "package.json",
                br#"{"name":"actual","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        source.write("cli.js", b"console.log('tool');\n").unwrap();
        source
            .write("runtime.js", b"module.exports = 1;\n")
            .unwrap();
        let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let member_source = TempProject::new("exec-member-reference").unwrap();
        member_source
            .write(
                "package.json",
                br#"{"name":"other","version":"1.0.0","bin":{"member-tool":"cli.js"}}"#,
            )
            .unwrap();
        member_source
            .write("cli.js", b"console.log('member');\n")
            .unwrap();
        let member_digest = tapid_archive::canonical_tree_digest(member_source.path()).unwrap();
        let store = tapid_store::Store::new(home.path().join("store"));
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&digest.parse().unwrap(), source.path())
            .unwrap();
        transaction
            .stage_verified_tree(&member_digest.parse().unwrap(), member_source.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let manifest =
            br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"alias":"npm:actual@1.0.0"},"optionalDependencies":{"second":"npm:other@1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        project.write("packages/member/package.json", br#"{"name":"member","version":"1.0.0","devDependencies":{"second":"npm:other@1.0.0"}}"#).unwrap();
        let workspace =
            Workspace::discover_bounded(project.path(), MAX_MEMBERS, MAX_EXPANSION).unwrap();
        let local = crate::online::workspace_materialization_from_discovered(
            project.path(),
            &RegistryConfig::default(),
            None,
            &workspace,
        )
        .unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org", "actual", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", &digest,
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        ).unwrap();
        let key = package.key();
        let member_package = tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org", "other", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", &member_digest,
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        ).unwrap();
        let member_key = member_package.key();
        let mut lock = Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest)).unwrap();
        lock.insert_package(package).unwrap();
        lock.insert_package(member_package).unwrap();
        let mut roots = vec![key.clone()];
        for mut member in local.locked.clone() {
            member.add_alias_dependency("second", &member_key).unwrap();
            roots.push(member.key());
            lock.insert_workspace_package(member).unwrap();
        }
        lock.set_roots(roots).unwrap();
        lock.set_root_bindings(BTreeMap::from([("alias".into(), key)]))
            .unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        drop(ActivationLock::acquire(project.path()).unwrap());
        let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
        let (input, trees) = super::super::installed_input(
            &evidence.lock,
            evidence.workspace.root(),
            &evidence.store,
            &evidence.config,
        )
        .unwrap();
        let plan = tapid_linker::plan_workspace_layout(
            tapid_linker::ManagedRoot::new(&evidence.root).unwrap(),
            input.clone(),
            &local.links,
            crate::application::replay::current_platform(),
        )
        .unwrap();
        let stage = evidence.root.join(".fixture-stage");
        crate::filesystem::tree::materialize_stage_with_workspace_links(
            &stage,
            &plan,
            &input.instances,
            &trees,
            false,
            &local.links,
            &evidence.root,
            |_, _| {},
        )
        .unwrap();
        std::fs::rename(
            stage.join("node_modules"),
            evidence.root.join("node_modules"),
        )
        .unwrap();
        std::fs::remove_dir(stage).unwrap();
        project
            .write(".tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        evidence.verify_native_layout().unwrap();
        let bind = || {
            super::super::bind_live_command(
                &evidence.root,
                &evidence.root,
                &input,
                &plan,
                &local.links,
                super::super::selection::select_manifest_command(
                    &input,
                    &Default::default(),
                    evidence.workspace.root(),
                    Some("alias"),
                    "tool",
                    crate::application::replay::current_platform(),
                )
                .unwrap(),
            )
        };
        let selected = evidence
            .select_native_command(None, Some("alias"), "tool")
            .unwrap();
        evidence
            .revalidate_command(None, Some("alias"), "tool", &selected)
            .unwrap();
        let mut redirected = evidence
            .select_native_command(None, Some("alias"), "tool")
            .unwrap();
        redirected.entry = evidence.root.join("node_modules/alias/runtime.js");
        assert!(
            evidence
                .revalidate_command(None, Some("alias"), "tool", &redirected)
                .is_err()
        );
        project
            .write("tapid.lifecycle.toml", b"schema = 1\n")
            .unwrap();
        assert!(
            evidence
                .revalidate_command(None, Some("alias"), "tool", &selected)
                .is_err()
        );
        std::fs::remove_file(project.path().join("tapid.lifecycle.toml")).unwrap();
        evidence
            .revalidate_command(None, Some("alias"), "tool", &selected)
            .unwrap();
        assert!(matches!(
            evidence.select_native_command(Some("unknown"), Some("alias"), "tool"),
            Err(super::super::selection::SelectionError::InvalidProjectSelection(_))
        ));
        assert_eq!(
            selected.entry,
            evidence.root.join("node_modules/alias/cli.js")
        );
        assert!(
            matches!(&selected.selection.owner, super::super::selection::CommandOwner::Authenticated(owner) if owner.id.name.as_str() == "actual")
        );
        let member_command = evidence
            .select_native_command(Some("member"), None, "member-tool")
            .unwrap();
        assert_eq!(member_command.selection.install_name.as_str(), "second");
        assert!(matches!(
            evidence.select_native_command(None, None, "member-tool"),
            Err(super::super::selection::SelectionError::MissingCommand)
        ));

        assert_eq!(
            member_command.entry,
            evidence.root.join("node_modules/second/cli.js")
        );
        assert!(matches!(
            evidence.select_native_command(Some("member"), Some("alias"), "tool"),
            Err(super::super::selection::SelectionError::InvalidPackageSelection(_))
        ));
        assert!(matches!(
            evidence.select_native_command(None, Some("second"), "member-tool"),
            Err(super::super::selection::SelectionError::InvalidPackageSelection(_))
        ));
        assert!(
            !evidence
                .root
                .join("packages/member/.tapid-activation.lock")
                .exists()
        );
        project
            .write(
                "node_modules/alias/runtime.js",
                b"modified runtime dependency",
            )
            .unwrap();
        assert!(bind().is_err(), "the entry alone is insufficient evidence");
        assert!(
            evidence
                .revalidate_command(None, Some("alias"), "tool", &selected)
                .is_err()
        );
        assert!(evidence.verify_native_layout().is_err());
        evidence.revalidate().unwrap();
    }

    #[test]
    fn project_observation_retains_coordination_and_rejects_changed_inputs_without_repair() {
        let project = TempProject::new("exec-project-observation").unwrap();
        let home = TempHome::new("exec-observation-store").unwrap();
        let manifest = br#"{"name":"root","version":"1.0.0"}"#;
        project.write("package.json", manifest).unwrap();
        let lock =
            tapid_lockfile::Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest))
                .unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        let store = tapid_store::Store::new(home.path().join("store"));
        drop(store.read_guard().unwrap());
        assert!(ProjectEvidence::observe(project.path(), &store).is_err());
        assert!(!project.path().join(".tapid-activation.lock").exists());
        drop(crate::filesystem::activation::ActivationLock::acquire(project.path()).unwrap());
        let before = std::fs::read(project.path().join(".tapid-activation.lock")).unwrap();
        let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
        evidence.revalidate().unwrap();
        assert!(
            crate::filesystem::activation::ActivationLock::acquire_existing(project.path())
                .is_err()
        );
        assert!(
            fs4::FileExt::try_lock(&std::fs::File::open(store.root().join(".store.lock")).unwrap())
                .is_err()
        );
        project.write("tapid.toml", b"[registries]\n").unwrap();
        assert!(evidence.revalidate().is_err());
        std::fs::remove_file(project.path().join("tapid.toml")).unwrap();
        evidence.revalidate().unwrap();
        project.write("package.json", b"{}").unwrap();
        assert!(evidence.revalidate().is_err());
        drop(evidence);
        assert_eq!(
            std::fs::read(project.path().join(".tapid-activation.lock")).unwrap(),
            before
        );
        drop(
            crate::filesystem::activation::ActivationLock::acquire_existing(project.path())
                .unwrap(),
        );
    }
}
