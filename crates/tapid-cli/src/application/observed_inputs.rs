//! Original source evidence, kept separate from proposed manifest replacements.
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
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
use tapid_manifest::{PackageManifest, Workspace};

pub(crate) struct Inputs {
    pub workspace: Workspace,
    pub lock: Option<Vec<u8>>,
    manifests: BTreeMap<PathBuf, PackageManifest>,
    files: BTreeMap<PathBuf, Option<String>>,
    registry: Value,
    pub policy: Option<tapid_runner::DependencyLifecyclePolicy>,
    pub config: crate::registry::RegistryConfig,
    root: PathBuf,
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
        "project inputs changed during observation; retry against current inputs",
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
        let mut result = Self::capture_sources(project, target)?;
        result.policy = Some(super::dependency_scripts::load_policy(&result.root)?);
        let bytes = read(&result.root.join("tapid.lifecycle.toml"), 1024 * 1024)?;
        result.files.insert(
            result.root.join("tapid.lifecycle.toml"),
            bytes.as_deref().map(digest_bytes),
        );
        result.recheck()?;
        Ok(result)
    }

    /// Observe graph inputs without requiring install-specific lifecycle policy.
    pub fn capture_sources(project: &Path, target: &Path) -> Result<Self, OperationalError> {
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
        let config = crate::registry::RegistryConfig::load(&root)
            .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
        let result = Self {
            workspace,
            lock,
            manifests,
            files,
            registry: routing(&config),
            config,
            policy: None,
            root,
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
    pub fn root_digest(&self) -> &str {
        self.files[&self.root.join("package.json")]
            .as_deref()
            .expect("root source")
    }
    pub fn recheck(&self) -> Result<(), OperationalError> {
        if crate::filesystem::lifecycle_journal::has_pending(&self.root).map_err(|_| changed())? {
            return Err(changed());
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
            || self.policy.as_ref().is_some_and(|policy| {
                super::dependency_scripts::load_policy(&self.root)
                    .map_or(true, |current| current.digest() != policy.digest())
            })
        {
            return Err(changed());
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
            "lifecycle_policy":if self.files[&self.root.join("tapid.lifecycle.toml")].is_some() { json!({"state":"present","identity_kind":"exact_source","digest":self.policy.as_ref().expect("preview policy").digest()}) } else { json!({"state":"absent_default_deny"}) },
            "operation":{"mode":mode_name,"update":update,"allow_unverified_registry_artifacts":allow_unverified},
            "target":format!("{}/{}/{}",std::env::consts::OS,std::env::consts::ARCH,online::current_libc().unwrap_or("unspecified")),
            "consistency":"rechecked_non_atomic_observation",
            "exclusions":["credential values","mutable registry metadata","artifact availability and bytes","installed and store state","hook outputs","runtime assurance"]})
    }
}
