//! Offline project preflight. No store handles, transaction locks, or recovery calls.
use crate::{
    commands::run::{ConfigReadError, read_bounded_config_file},
    filesystem::{
        activation::validate_managed_node_modules, atomic::digest_bytes, lifecycle_journal,
    },
    registry::RegistryConfig,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tapid_lockfile::Lockfile;
use tapid_manifest::{PackageManifest, Workspace};

const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 128;

#[derive(Serialize)]
pub(crate) struct Blocker {
    pub(crate) code: &'static str,
    pub(crate) location: String,
}

#[derive(Serialize)]
pub(crate) struct StatusReport {
    pub(crate) assessment: &'static str,
    pub(crate) manifest_lock_agreement: &'static str,
    pub(crate) workspace: Value,
    pub(crate) installation: Vec<Value>,
    pub(crate) recovery: &'static str,
    pub(crate) recovery_guidance: Option<&'static str>,
    pub(crate) registries: Value,
    pub(crate) blockers: Vec<Blocker>,
    pub(crate) truncated: bool,
    pub(crate) snapshot: Value,
    pub(crate) verification_depth: &'static str,
    pub(crate) checks_not_performed: Vec<&'static str>,
    pub(crate) capabilities: Value,
    #[serde(skip)]
    pub(crate) project: PathBuf,
}

impl StatusReport {
    fn block(&mut self, code: &'static str, location: impl AsRef<Path>) {
        if self.blockers.len() == MAX_ENTRIES {
            self.truncated = true;
            return;
        }
        self.blockers.push(Blocker {
            code,
            location: location.as_ref().display().to_string(),
        });
    }
}

// Absence, rejected file types and read failures retain distinct snapshot identities.
// Never read journal contents or enumerate installed trees.
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    read_bounded_config_file(path, limit).map_err(|error| match error {
        ConfigReadError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        ConfigReadError::CapacityExceeded => "oversized",
        _ => "unreadable_or_non_regular",
    })
}

fn fingerprint(path: &Path, limit: usize) -> String {
    if limit == 0 {
        return match fs::symlink_metadata(path) {
            Ok(m) => format!(
                "metadata:{:?}:{}:{:?}",
                m.file_type(),
                m.len(),
                m.modified().ok()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing".to_owned(),
            Err(_) => "unreadable".to_owned(),
        };
    }
    match read(path, limit) {
        Ok(bytes) => digest_bytes(&bytes),
        Err(state) => state.to_owned(),
    }
}

pub(crate) fn inspect(project: &Path, selector: Option<&str>) -> StatusReport {
    let project = fs::canonicalize(project).unwrap_or_else(|_| {
        if project.is_absolute() {
            project.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(project)
        }
    });
    let mut report = StatusReport {
        assessment: "no_local_blockers",
        manifest_lock_agreement: "unknown",
        workspace: json!({"selected": selector, "manifest": null, "members": [], "linking": "not_verified"}),
        installation: Vec::new(),
        recovery: "unknown",
        recovery_guidance: None,
        registries: Value::Null,
        blockers: Vec::new(),
        truncated: false,
        snapshot: Value::Null,
        verification_depth: "local_metadata_and_ownership_only",
        checks_not_performed: vec![
            "atomic_snapshot_or_concurrent_writer_exclusion",
            "installed_tree_integrity_and_lock_correspondence",
            "workspace_links",
            "shared_store_contents_and_recovery",
            "registry_access_and_credentials",
            "dependency_resolution_and_transitive_compatibility",
            "artifact_bytes_and_copied_sources",
            "lifecycle_approvals_and_derived_outputs",
            "runner_runtime_policy_and_platform_assurance",
            "package_engine_and_target_platform_requirements",
        ],
        capabilities: json!({
            "experimental": true,
            "lock_schemas": [4, 5, 6, tapid_lockfile::LOCKFILE_VERSION, tapid_lockfile::LIFECYCLE_LOCKFILE_VERSION, tapid_lockfile::COPIED_SOURCE_LOCKFILE_VERSION],
            "spec_forms": ["semver", "npm_dist_tag", "npm_alias", "jsr_semver", "workspace:*", "workspace:^", "workspace:~", "file_tarball", "git+https"],
            "package_manager": "local_preflight_only", "runner": "not_assessed",
            "workspace_selection": "exact_member_name", "workspace_linking": "supported_not_verified",
            "registry_routing": "experimental"
        }),
        project: project.clone(),
    };
    let mut snapshots = BTreeMap::new();
    for (name, limit) in [
        ("package.json", MAX_FILE_BYTES),
        ("tapid.lock", MAX_FILE_BYTES),
        ("tapid.toml", 64 * 1024),
    ] {
        let path = project.join(name);
        snapshots.insert(path.clone(), (limit, fingerprint(&path, limit)));
    }
    inspect_installation(&project, &mut report, &mut snapshots);
    for name in [
        ".tapid-lifecycle-journal.json",
        ".tapid-lifecycle-journal.lifecycle.bak",
    ] {
        let path = project.join(name);
        snapshots.insert(path.clone(), (0, fingerprint(&path, 0)));
    }
    match lifecycle_journal::has_pending(&project) {
        Ok(false) => report.recovery = "none_detected",
        Ok(true) => {
            report.recovery = "pending";
            report.block(
                "RECOVERY_REQUIRED",
                project.join(".tapid-lifecycle-journal.json"),
            );
        }
        Err(_) => {
            report.recovery = "unknown";
            report.block(
                "RECOVERY_STATE_INVALID",
                project.join(".tapid-lifecycle-journal.json"),
            );
        }
    }
    if report.recovery != "none_detected" {
        report.recovery_guidance = Some(
            "Run tapid install to attempt normal recovery after reviewing the project. Preserve recovery files; status does not recover or delete them.",
        );
    }
    let manifest_path = project.join("package.json");
    let manifest_bytes = read(&manifest_path, MAX_FILE_BYTES);
    let manifest = manifest_bytes
        .as_ref()
        .ok()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| PackageManifest::parse(text).ok());
    if manifest.is_none() {
        report.block("MANIFEST_INVALID_OR_MISSING", &manifest_path);
    }
    let config = match RegistryConfig::load(&project) {
        Ok(config) => {
            report.registries = config.inspection();
            Some(config)
        }
        Err(_) => {
            report.block("REGISTRY_CONFIGURATION_INVALID", project.join("tapid.toml"));
            None
        }
    };
    let workspace = if manifest.is_some() {
        match Workspace::discover(&project) {
            Ok(workspace) => Some(workspace),
            Err(_) => {
                report.block("WORKSPACE_INVALID", &manifest_path);
                None
            }
        }
    } else {
        None
    };
    if let Some(workspace) = &workspace {
        match workspace.select_path(selector) {
            Ok(path) => report.workspace["manifest"] = json!(path.display().to_string()),
            Err(_) => report.block("WORKSPACE_SELECTION_INVALID", &manifest_path),
        }
        if workspace.members().len() > MAX_ENTRIES {
            report.truncated = true;
        }
        report.workspace["members"] = json!(
            workspace
                .members()
                .iter()
                .take(MAX_ENTRIES)
                .map(|m| m.name())
                .collect::<Vec<_>>()
        );
        inspect_manifest(workspace.root(), &manifest_path, workspace, &mut report);
        for member in workspace.members().iter().take(MAX_ENTRIES) {
            snapshots.insert(
                member.path().to_path_buf(),
                (MAX_FILE_BYTES, fingerprint(member.path(), MAX_FILE_BYTES)),
            );
            inspect_manifest(member.manifest(), member.path(), workspace, &mut report);
            if let Some(dir) = member.path().parent() {
                inspect_installation(dir, &mut report, &mut snapshots);
            }
        }
    }
    let lock_path = project.join("tapid.lock");
    match read(&lock_path, MAX_FILE_BYTES) {
        Ok(bytes) => match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| Lockfile::from_json(text).ok())
        {
            Some(lock) => {
                if let (Some(manifest), Ok(bytes), Some(_workspace), Some(config)) =
                    (&manifest, &manifest_bytes, &workspace, &config)
                {
                    let digest = digest_bytes(bytes);
                    if lock.root_manifest_digest() == digest
                        && lock.validate_replay(&digest).is_err()
                    {
                        report.block("LOCK_REPLAY_UNSUPPORTED", &lock_path);
                    }
                    if lock.root_manifest_digest() != digest {
                        report.manifest_lock_agreement = "mismatch";
                        report.block("LOCK_MANIFEST_MISMATCH", &lock_path);
                    } else if let Err(code) = workspace_matches(&project, manifest, config, &lock) {
                        report.manifest_lock_agreement = if code == "LOCK_WORKSPACE_MISMATCH" {
                            "mismatch"
                        } else {
                            "unknown"
                        };
                        report.block(code, &lock_path);
                    } else if crate::application::replay::validate_root_bindings(
                        &lock, manifest, config,
                    )
                    .is_err()
                    {
                        report.manifest_lock_agreement = "mismatch";
                        report.block("LOCK_ROOT_BINDINGS_INVALID", &lock_path);
                    } else {
                        report.manifest_lock_agreement = "match";
                    }
                }
            }
            None => report.block("LOCKFILE_INVALID", &lock_path),
        },
        Err("missing") => report.block("LOCKFILE_MISSING", &lock_path),
        Err(_) => report.block("LOCKFILE_UNREADABLE", &lock_path),
    }
    let mut changed = false;
    let files = snapshots.iter().map(|(path, (limit, digest))| {
        let after = fingerprint(path, *limit);
        changed |= after != *digest;
        json!({"path": path.display().to_string(), "identity": digest, "changed_on_recheck": after != *digest})
    }).collect::<Vec<_>>();
    if changed {
        report.block("SNAPSHOT_CHANGED", &project);
        report.manifest_lock_agreement = "unknown";
    }
    report.snapshot =
        json!({"consistency": "not_atomic", "changes_detected": changed, "files": files});
    if !report.blockers.is_empty() || report.truncated {
        report.assessment = "blocked";
    }
    report
}

fn inspect_installation(
    project: &Path,
    report: &mut StatusReport,
    snapshots: &mut BTreeMap<PathBuf, (usize, String)>,
) {
    let path = project.join("node_modules");
    snapshots.insert(path.clone(), (0, fingerprint(&path, 0)));
    let state = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        Ok(_) if validate_managed_node_modules(project).is_ok() => "managed",
        _ => {
            report.block("INSTALLATION_NOT_MANAGED", &path);
            "unmanaged_or_invalid"
        }
    };
    report
        .installation
        .push(json!({"path": path.display().to_string(), "state": state}));
    let marker = project.join(".tapid-managed");
    snapshots.insert(marker.clone(), (1024, fingerprint(&marker, 1024)));
}

fn workspace_matches(
    project: &Path,
    manifest: &PackageManifest,
    config: &RegistryConfig,
    lock: &Lockfile,
) -> Result<(), &'static str> {
    let workspace = crate::online::workspace_materialization(project, config)
        .map_err(|_| "WORKSPACE_COMPATIBILITY_BLOCKER")?;
    super::replay::validate_workspace_membership(&workspace, lock)
        .map_err(|_| "LOCK_WORKSPACE_MISMATCH")?;
    let dependencies =
        crate::online::resolved_workspace_registry_dependencies(manifest, &workspace, config)
            .map_err(|_| "WORKSPACE_COMPATIBILITY_BLOCKER")?;
    crate::application::install::validate_workspace_dependency_edges(
        &workspace,
        &dependencies,
        lock,
    )
    .map_err(|_| "LOCK_WORKSPACE_MISMATCH")
}

fn inspect_manifest(
    manifest: &PackageManifest,
    path: &Path,
    workspace: &Workspace,
    report: &mut StatusReport,
) {
    for (section, dependencies) in [
        ("dependencies", manifest.dependencies()),
        ("devDependencies", manifest.dev_dependencies()),
        ("optionalDependencies", manifest.optional_dependencies()),
        ("peerDependencies", manifest.peer_dependencies()),
    ] {
        for (name, raw) in dependencies.iter().take(MAX_ENTRIES) {
            let location = format!(
                "{}#/{section}/{}",
                path.display(),
                name.replace('~', "~0").replace('/', "~1")
            );
            let member_copied = path != workspace.root_path()
                && crate::online::copied_declaration(raw)
                    .ok()
                    .flatten()
                    .is_some();
            let valid = if member_copied {
                false
            } else if let Some(member) = workspace.members().iter().find(|m| m.name() == name) {
                crate::online::workspace_requirement(raw, &member.manifest().version())
                    .is_ok_and(|r| r.matches(&member.manifest().version()))
            } else if raw.starts_with("workspace:") {
                false
            } else {
                // Reuse the online manifest planner for copied specs, aliases, JSR and ranges.
                let minimal =
                    json!({"name": "inspection", "version": "1.0.0", "dependencies": {name: raw}});
                PackageManifest::parse(&minimal.to_string())
                    .ok()
                    .is_some_and(|m| crate::online::manifest_roots(&m).is_ok())
            };
            if !valid {
                report.block("UNSUPPORTED_SPEC", Path::new(&location));
            }
        }
        if dependencies.len() > MAX_ENTRIES {
            report.truncated = true;
        }
    }
    if crate::online::manifest_overrides(manifest).is_err() {
        report.block(
            "UNSUPPORTED_OVERRIDE",
            Path::new(&format!("{}#/overrides", path.display())),
        );
    } else if crate::online::manifest_roots(manifest).is_err() {
        report.block("MANIFEST_COMPATIBILITY_BLOCKER", path);
    }
}
