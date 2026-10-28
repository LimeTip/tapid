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
#[allow(dead_code)] // Awaiting execution composition; not launch authority.
pub(super) struct ProjectEvidence {
    pub root: PathBuf,
    pub workspace: Workspace,
    pub lock: Lockfile,
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

#[allow(dead_code)]
impl ProjectEvidence {
    /// Verify native-lock materialization, including all owned member layouts.
    /// This authenticates content only; lifecycle approval and execution policy
    /// must be established separately before any checked launch.
    pub(super) fn verify_native_layout(&self) -> Result<(), OperationalError> {
        self.revalidate()?;
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
        verify(&self.root, &input, &workspace_plan.links)?;
        for (root, member) in &workspace_plan.members {
            verify(root, &member.input, &member.links)?;
        }
        self.revalidate()
    }

    pub(super) fn observe(
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
            if std::fs::symlink_metadata(member.join(".tapid-managed")).is_ok() {
                crate::filesystem::lifecycle_journal::validate_member_root(&root, &member)
                    .map_err(|e| failure(ErrorKind::Project, e))?;
                activation.push(ActivationLock::acquire_existing(&member)?);
            }
        }
        let config = observations[&root.join("tapid.toml")]
            .as_deref()
            .map(RegistryConfig::parse_toml_bytes)
            .transpose()
            .map_err(|e| failure(ErrorKind::RegistryConfiguration, e))?
            .unwrap_or_default();
        let lock = Lockfile::from_json(
            std::str::from_utf8(observations[&root.join("tapid.lock")].as_deref().unwrap())
                .map_err(|e| failure(ErrorKind::Lockfile, e))?,
        )
        .map_err(|e| failure(ErrorKind::Lockfile, e))?;
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
        let local = crate::online::workspace_materialization_from_discovered(
            &root, &config, None, &workspace,
        )?;
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
        let evidence = Self {
            root,
            workspace,
            lock,
            config,
            store: store.existing_read_guard()?,
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
mod tests {
    use super::*;
    use tapid_test_support::{TempHome, TempProject};

    #[test]
    fn workspace_observation_requires_existing_member_coordination_and_rechecks_member_bytes() {
        let project = TempProject::new("exec-member-observation").unwrap();
        let home = TempHome::new("exec-member-store").unwrap();
        let manifest = br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
        project.write("package.json", manifest).unwrap();
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"1.0.0"}"#,
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
        for member in local.locked {
            lock.insert_workspace_package(member).unwrap();
        }
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
        assert!(ProjectEvidence::observe(project.path(), &store).is_err());
        assert!(!member.join(".tapid-activation.lock").exists());
        drop(ActivationLock::acquire(&member).unwrap());
        let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
        assert!(ActivationLock::acquire_existing(&member).is_err());
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
        project
            .write("packages/member/node_modules/unplanned.js", b"unexpected")
            .unwrap();
        assert!(evidence.verify_native_layout().is_err());
        std::fs::remove_file(member.join("node_modules/unplanned.js")).unwrap();
        evidence.verify_native_layout().unwrap();
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
        let store = tapid_store::Store::new(home.path().join("store"));
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&digest.parse().unwrap(), source.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let manifest =
            br#"{"name":"root","version":"1.0.0","dependencies":{"alias":"npm:actual@1.0.0"}}"#;
        project.write("package.json", manifest).unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org", "actual", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", &digest,
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        ).unwrap();
        let key = package.key();
        let mut lock = Lockfile::new(&crate::filesystem::atomic::digest_bytes(manifest)).unwrap();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
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
        let plan = tapid_linker::plan_named_layout(
            tapid_linker::ManagedRoot::new(project.path()).unwrap(),
            input.clone(),
            crate::application::replay::current_platform(),
        )
        .unwrap();
        crate::filesystem::tree::materialize_stage(
            project.path(),
            &plan,
            &input.instances,
            &trees,
            false,
        )
        .unwrap();
        project
            .write(".tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        evidence.verify_native_layout().unwrap();
        let bind = || {
            super::super::bind_live_command(
                project.path(),
                project.path(),
                &input,
                &plan,
                &tapid_linker::WorkspaceLinkPlan::default(),
                Some("alias"),
                "tool",
            )
        };
        let selected = bind().unwrap();
        assert_eq!(
            selected.entry,
            project.path().join("node_modules/alias/cli.js")
        );
        assert_eq!(selected.selection.owner.id.name.as_str(), "actual");
        project
            .write(
                "node_modules/alias/runtime.js",
                b"modified runtime dependency",
            )
            .unwrap();
        assert!(bind().is_err(), "the entry alone is insufficient evidence");
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
