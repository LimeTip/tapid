//! Shared dependency input evidence and publication of validated native selections.
//! Capturing and rechecking are read-only; publication explicitly owns a metadata transaction.
use super::{
    install::InstallMode,
    outcome::{ErrorKind, OperationalError},
};
use crate::filesystem::{
    atomic::digest_bytes,
    read::{ReadError, read_bounded_file},
};
use crate::online;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use tapid_lockfile::Lockfile;
use tapid_manifest::{PackageManifest, Workspace};

pub(crate) struct Inputs {
    pub workspace: Workspace,
    pub lock: Option<Vec<u8>>,
    manifests: BTreeMap<PathBuf, PackageManifest>,
    files: BTreeMap<PathBuf, Option<String>>,
    registry: Value,
    pub policy: tapid_runner::DependencyLifecyclePolicy,
    pub config: crate::registry::RegistryConfig,
    root: PathBuf,
    copied_files: BTreeMap<String, String>,
}

fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, OperationalError> {
    match read_bounded_file(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(ReadError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(OperationalError::new(
            ErrorKind::InvalidData,
            format!("cannot inspect {}: {e:?}", path.display()),
        )),
    }
}
fn changed() -> OperationalError {
    OperationalError::new(
        ErrorKind::ProjectChanged,
        "dependency inputs changed during inspection; retry against current inputs",
    )
}
fn routing(config: &crate::registry::RegistryConfig) -> Value {
    let mut value = config.inspection();
    value
        .as_object_mut()
        .expect("registry projection")
        .remove("credentials");
    value
}
impl Inputs {
    pub fn capture(project: &Path, target: &Path) -> Result<Self, OperationalError> {
        let root = fs::canonicalize(project)
            .map_err(|e| OperationalError::from_source(ErrorKind::Manifest, e))?;
        let mut files = BTreeMap::new();
        let mut manifests = BTreeMap::new();
        for path in [root.join("package.json"), target.to_owned()] {
            let bytes = read(&path, tapid_manifest::MAX_MANIFEST_BYTES)?.ok_or_else(changed)?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|e| OperationalError::from_source(ErrorKind::Manifest, e))?;
            manifests.insert(
                path.clone(),
                PackageManifest::parse(text)
                    .map_err(|e| OperationalError::from_source(ErrorKind::Manifest, e))?,
            );
            files.insert(path, Some(digest_bytes(&bytes)));
        }
        let workspace =
            Workspace::discover_bounded_with_member_source(&root, 100_000, 4096, |path, text| {
                files
                    .entry(path.to_owned())
                    .or_insert_with(|| Some(digest_bytes(text.as_bytes())));
            })
            .map_err(|e| OperationalError::from_source(ErrorKind::InvalidData, e))?;
        if workspace.root() != &manifests[&root.join("package.json")] {
            return Err(changed());
        }
        let lock = read(&root.join("tapid.lock"), 32 * 1024 * 1024)
            .map_err(|error| OperationalError::new(ErrorKind::Lockfile, error))?;
        files.insert(root.join("tapid.lock"), lock.as_deref().map(digest_bytes));
        let policy = super::dependency_scripts::load_policy(&root)?;
        let config = crate::registry::RegistryConfig::load(&root)
            .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
        let policy_bytes = read(&root.join("tapid.lifecycle.toml"), 1024 * 1024)?;
        files.insert(
            root.join("tapid.lifecycle.toml"),
            policy_bytes.as_deref().map(digest_bytes),
        );
        let result = Self {
            workspace,
            lock,
            manifests,
            files,
            registry: routing(&config),
            config,
            policy,
            root,
            copied_files: BTreeMap::new(),
        };
        result.recheck()?;
        Ok(result)
    }
    pub fn materialize(
        &self,
        config: &crate::registry::RegistryConfig,
        replacement: Option<(&Path, &PackageManifest)>,
    ) -> Result<online::WorkspaceMaterialization, OperationalError> {
        let workspace = online::workspace_materialization_from_discovered(
            &self.root,
            config,
            replacement,
            &self.workspace,
        )?;
        for package in &workspace.locked {
            let key: tapid_lockfile::LockfilePackageKey = package.key().parse()?;
            let tapid_lockfile::LockfilePackageSource::Workspace(source) = key.source else {
                unreachable!("workspace package")
            };
            let path = self.root.join(source.path()).join("package.json");
            if replacement.is_none_or(|(target, _)| target != path)
                && self.files.get(&path).and_then(|d| d.as_deref())
                    != Some(package.manifest_digest())
            {
                return Err(changed());
            }
        }
        self.recheck()?;
        Ok(workspace)
    }
    pub fn manifest(&self, path: &Path) -> &PackageManifest {
        &self.manifests[path]
    }
    /// Import validation needs declarations and exact captured digests, without
    /// replay's installed-layout ownership checks or another filesystem read.
    pub fn imported_workspace_manifests(
        &self,
    ) -> Result<BTreeMap<String, (String, String)>, OperationalError> {
        self.workspace
            .members()
            .iter()
            .map(|member| {
                let relative = member
                    .path()
                    .parent()
                    .and_then(|p| p.strip_prefix(&self.root).ok())
                    .and_then(|p| p.to_str())
                    .ok_or_else(changed)?;
                let digest = self
                    .files
                    .get(member.path())
                    .and_then(|d| d.clone())
                    .ok_or_else(changed)?;
                Ok((
                    relative.replace('\\', "/"),
                    (member.manifest().to_json(), digest),
                ))
            })
            .collect()
    }
    pub fn root_digest(&self) -> &str {
        self.files[&self.root.join("package.json")]
            .as_deref()
            .expect("root source")
    }
    pub fn recheck(&self) -> Result<(), OperationalError> {
        if crate::filesystem::lifecycle_journal::has_pending(&self.root).map_err(|_| changed())? {
            return Err(changed());
        }
        self.recheck_sources()
    }
    fn recheck_sources(&self) -> Result<(), OperationalError> {
        if fs::canonicalize(&self.root).map_err(|_| changed())? != self.root {
            return Err(changed());
        }
        for (relative, expected) in &self.copied_files {
            let bytes = online::copied_file_bytes(&self.root, relative).map_err(|_| changed())?;
            if digest_bytes(&bytes) != *expected {
                return Err(changed());
            }
        }
        for (path, expected) in &self.files {
            let bytes = read(path, 32 * 1024 * 1024).map_err(|_| changed())?;
            if bytes.as_deref().map(digest_bytes) != *expected {
                return Err(changed());
            }
        }
        let mut members = BTreeMap::new();
        let workspace = Workspace::discover_bounded_with_member_source(
            &self.root,
            100_000,
            4096,
            |path, text| {
                members.insert(path.to_owned(), Some(digest_bytes(text.as_bytes())));
            },
        )
        .map_err(|_| changed())?;
        if workspace.root() != self.workspace.root()
            || workspace
                .members()
                .iter()
                .map(|m| m.path())
                .collect::<BTreeSet<_>>()
                != self
                    .workspace
                    .members()
                    .iter()
                    .map(|m| m.path())
                    .collect::<BTreeSet<_>>()
            || members.iter().any(|(p, d)| self.files.get(p) != Some(d))
            || routing(&crate::registry::RegistryConfig::load(&self.root).map_err(|_| changed())?)
                != self.registry
            || super::dependency_scripts::load_policy(&self.root)
                .map_err(|_| changed())?
                .digest()
                != self.policy.digest()
        {
            return Err(changed());
        }
        Ok(())
    }
    /// Capture accepted file declarations before source preparation. Rechecking
    /// uses the same containment rules as preparation, including every ancestor.
    pub fn capture_copied_files(
        &mut self,
        manifest: &PackageManifest,
    ) -> Result<(), OperationalError> {
        for raw in manifest
            .dependencies()
            .values()
            .chain(manifest.dev_dependencies().values())
            .chain(manifest.optional_dependencies().values())
        {
            if let Some(online::CopiedDeclaration::File(relative)) = online::copied_declaration(raw)
                .map_err(|e| OperationalError::new(ErrorKind::InvalidRequest, e))?
            {
                let bytes = online::copied_file_bytes(&self.root, &relative)
                    .map_err(|e| OperationalError::new(ErrorKind::InvalidData, e))?;
                let digest = digest_bytes(&bytes);
                if self
                    .copied_files
                    .get(&relative)
                    .is_some_and(|old| old != &digest)
                {
                    return Err(changed());
                }
                self.copied_files.insert(relative, digest);
            }
        }
        self.recheck()
    }

    /// Publish an already validated selection, without resolving or entering
    /// store/layout recovery. Proposed manifest bytes remain separate from the
    /// captured original bytes used by the stale-input check and journal.
    pub fn publish_selection(
        &self,
        lock: &Lockfile,
        replacement: Option<(&Path, &PackageManifest)>,
    ) -> Result<(), OperationalError> {
        let root = self.root.join("package.json");
        let output_path = replacement
            .map(|(path, _)| fs::canonicalize(path).map_err(|_| changed()))
            .transpose()?;
        let root_digest = replacement
            .filter(|_| output_path.as_ref() == Some(&root))
            .map(|(_, manifest)| digest_bytes(manifest.to_json().as_bytes()));
        lock.validate_selection(root_digest.as_deref().unwrap_or(self.root_digest()))?;
        let lock_bytes = lock.to_json()?.into_bytes();
        self.recheck()?;
        let owner = crate::filesystem::activation::ActivationLock::acquire_metadata(&self.root)?;
        self.recheck()?;
        online::validate_locked_files(&self.root, lock)?;
        let mut outputs = Vec::new();
        if let Some((path, manifest)) = replacement {
            if !self.manifests.contains_key(path) {
                return Err(OperationalError::new(
                    ErrorKind::InvalidRequest,
                    "manifest output was not captured",
                ));
            }
            let relative = output_path
                .as_ref()
                .expect("replacement output")
                .strip_prefix(&self.root)
                .ok()
                .and_then(|p| p.to_str())
                .ok_or_else(|| {
                    OperationalError::new(ErrorKind::InvalidRequest, "invalid manifest output path")
                })?;
            let before = read(path, tapid_manifest::MAX_MANIFEST_BYTES)?;
            outputs.push((
                relative.replace('\\', "/"),
                before,
                manifest.to_json().into_bytes(),
            ));
        }
        outputs.push(("tapid.lock".into(), self.lock.clone(), lock_bytes));
        let recovery = |error| OperationalError::new(ErrorKind::Recovery, error);
        let mut journal = crate::filesystem::lifecycle_journal::LifecycleJournal::prepare_metadata(
            &self.root,
            owner.owner_line(),
            outputs,
        )
        .map_err(recovery)?;
        crate::filesystem::activation::test_crash_at("metadata_prepared");
        // The journal now belongs to this operation; check source evidence
        // without interpreting our own prepared record as a stale transaction.
        let result = self.recheck_sources().and_then(|()| {
            journal.mark_committed().map_err(recovery)?;
            crate::filesystem::activation::test_crash_at("metadata_committed");
            journal.finish().map_err(recovery)
        });
        if let Err(error) = result {
            journal.settle().map_err(recovery)?;
            return Err(error);
        }
        Ok(())
    }
    pub fn receipt(&self, target: &Path, mode: InstallMode<'_>, allow_unverified: bool) -> Value {
        let identity = |path: &Path| json!({"identity_kind":"exact_source", "path":path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/"), "digest":self.files[path]});
        let mut members = self
            .workspace
            .members()
            .iter()
            .map(|m| identity(m.path()))
            .collect::<Vec<_>>();
        members.sort_by_key(|v| v["path"].as_str().unwrap_or_default().to_owned());
        let (mode_name, update) = match mode {
            InstallMode::Online => ("online", Value::Null),
            InstallMode::Refresh => ("refresh", Value::Null),
            InstallMode::Offline => ("offline", Value::Null),
            InstallMode::Frozen => ("frozen", Value::Null),
            InstallMode::Ci => ("ci", Value::Null),
            InstallMode::CiOffline => ("ci_offline", Value::Null),
            InstallMode::Update(r) => (
                "update",
                json!({"named":r.named,"selection":r.plan.mutations.iter().map(|m| &m.name).collect::<Vec<_>>(),"save_policy":r.save.map(|s| match s { super::lifecycle::SavePolicy::Exact => "exact", super::lifecycle::SavePolicy::Caret => "caret", super::lifecycle::SavePolicy::Tilde => "tilde" })}),
            ),
        };
        json!({"schema_version":1,"root_manifest":identity(&self.root.join("package.json")),"selected_manifest":identity(target),"workspace":members,
            "lock":match &self.lock { Some(bytes) => json!({"state":"present","digest":digest_bytes(bytes)}),None => json!({"state":"absent"}) },
            "registry":{"identity_kind":"semantic_configuration","routing":self.registry},
            "lifecycle_policy":if self.files[&self.root.join("tapid.lifecycle.toml")].is_some() { json!({"state":"present","identity_kind":"exact_source","digest":self.policy.digest()}) } else { json!({"state":"absent_default_deny"}) },
            "operation":{"mode":mode_name,"update":update,"allow_unverified_registry_artifacts":allow_unverified},
            "target":format!("{}/{}/{}",std::env::consts::OS,std::env::consts::ARCH,online::current_libc().unwrap_or("unspecified")),
            "consistency":"rechecked_non_atomic_observation",
            "exclusions":["credential values","mutable registry metadata","artifact availability and bytes","installed and store state","hook outputs","runtime assurance"]})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn selection_publication_rejects_redirected_project_before_locking_it() {
        let project = tapid_test_support::TempProject::new("selection-redirected-project").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0"}"#;
        project.write("app/package.json", manifest).unwrap();
        project.write("redirect/package.json", manifest).unwrap();
        let app = project.path().join("app").canonicalize().unwrap();
        let inputs = Inputs::capture(&app, &app.join("package.json")).unwrap();
        let lock = Lockfile::new_selection(inputs.root_digest()).unwrap();
        fs::rename(&app, project.path().join("original")).unwrap();
        std::os::unix::fs::symlink(project.path().join("redirect"), &app).unwrap();
        let error = inputs.publish_selection(&lock, None).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ProjectChanged);
        assert!(
            !project
                .path()
                .join("redirect/.tapid-activation.lock")
                .exists()
        );
        assert!(!project.path().join("redirect/tapid.lock").exists());
        assert_eq!(
            fs::read(project.path().join("redirect/package.json")).unwrap(),
            manifest
        );
    }

    #[test]
    fn selection_publication_writes_only_selected_metadata() {
        for edit in ["none", "root", "member"] {
            let project = tapid_test_support::TempProject::new("selection-publish").unwrap();
            let root = br#"{"name":"app","version":"1.0.0","workspaces":["z/*"]}"#;
            let member = br#"{"name":"member","version":"1.0.0"}"#;
            project.write("package.json", root).unwrap();
            project.write("z/member/package.json", member).unwrap();
            for path in [
                "node_modules/sentinel",
                "z/member/node_modules/sentinel",
                "store/sentinel",
                ".tapid-managed",
            ] {
                project.write(path, b"untouched").unwrap();
            }
            let root_path = project.path().join("package.json");
            let member_path = project.path().join("z/member/package.json");
            let target = if edit == "member" {
                &member_path
            } else {
                &root_path
            };
            let inputs = Inputs::capture(project.path(), target).unwrap();
            let next = inputs
                .manifest(target)
                .clone()
                .with_dependency_kind(
                    tapid_manifest::DependencyKind::PeerDependencies,
                    "peer",
                    "^1",
                )
                .unwrap();
            let digest = if edit == "root" {
                digest_bytes(next.to_json().as_bytes())
            } else {
                inputs.root_digest().to_owned()
            };
            let lock = Lockfile::new_selection(&digest).unwrap();
            inputs
                .publish_selection(&lock, (edit != "none").then_some((target.as_path(), &next)))
                .unwrap();
            assert_eq!(
                Lockfile::from_json(
                    &fs::read_to_string(project.path().join("tapid.lock")).unwrap()
                )
                .unwrap(),
                lock
            );
            assert_eq!(
                fs::read(&root_path).unwrap(),
                if edit == "root" {
                    next.to_json().into_bytes()
                } else {
                    root.to_vec()
                }
            );
            assert_eq!(
                fs::read(&member_path).unwrap(),
                if edit == "member" {
                    next.to_json().into_bytes()
                } else {
                    member.to_vec()
                }
            );
            for path in [
                "node_modules/sentinel",
                "z/member/node_modules/sentinel",
                "store/sentinel",
                ".tapid-managed",
            ] {
                assert_eq!(fs::read(project.path().join(path)).unwrap(), b"untouched");
            }
            assert!(!crate::filesystem::lifecycle_journal::has_pending(project.path()).unwrap());
        }
    }

    #[test]
    fn selection_publication_rejects_stale_inputs_without_overwriting_them() {
        for mutation in [
            "root",
            "member",
            "membership",
            "lock",
            "configuration",
            "copied",
        ] {
            let project = tapid_test_support::TempProject::new("selection-stale-input").unwrap();
            let root = br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"tool":"file:vendor/tool.tgz"}}"#;
            project.write("package.json", root).unwrap();
            project
                .write(
                    "packages/member/package.json",
                    br#"{"name":"member","version":"1.0.0"}"#,
                )
                .unwrap();
            project
                .write("vendor/tool.tgz", b"accepted archive bytes")
                .unwrap();
            project
                .write("node_modules/sentinel", b"running application")
                .unwrap();
            let target = project.path().join("package.json");
            let mut inputs = Inputs::capture(project.path(), &target).unwrap();
            inputs
                .capture_copied_files(
                    &PackageManifest::parse(std::str::from_utf8(root).unwrap()).unwrap(),
                )
                .unwrap();
            let lock = Lockfile::new_selection(inputs.root_digest()).unwrap();
            let changed_path = match mutation {
                "root" => "package.json",
                "member" => "packages/member/package.json",
                "membership" => "packages/new/package.json",
                "lock" => "tapid.lock",
                "configuration" => "tapid.toml",
                "copied" => "vendor/tool.tgz",
                _ => unreachable!(),
            };
            let changed = match mutation {
                "root" | "member" | "membership" => {
                    br#"{"name":"concurrent","version":"2.0.0"}"#.as_slice()
                }
                "configuration" => b"[registries.default]\nurl = 'https://example.com'\n",
                _ => b"concurrent bytes",
            };
            project.write(changed_path, changed).unwrap();
            let error = inputs.publish_selection(&lock, None).unwrap_err();
            assert_eq!(error.kind, ErrorKind::ProjectChanged, "{mutation}: {error}");
            assert_eq!(
                fs::read(project.path().join(changed_path)).unwrap(),
                changed
            );
            if mutation != "lock" {
                assert!(!project.path().join("tapid.lock").exists(), "{mutation}");
            }
            assert_eq!(
                fs::read(project.path().join("node_modules/sentinel")).unwrap(),
                b"running application"
            );
            assert!(!crate::filesystem::lifecycle_journal::has_pending(project.path()).unwrap());
        }
    }
}
