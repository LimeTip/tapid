//! Offline project preflight. No store handles, transaction locks, or recovery calls.
use crate::{
    filesystem::{
        activation::validate_managed_node_modules,
        atomic::digest_bytes,
        lifecycle_journal,
        read::{ReadError, read_bounded_file},
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
use tapid_lockfile::{ImportedNpmLockfile, Lockfile, LockfileError};
use tapid_manifest::{PackageManifest, Workspace};

const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 128;

/// A stable blocker code and the file or manifest field that needs attention.
#[derive(Serialize)]
pub(crate) struct Blocker {
    pub(crate) code: &'static str,
    pub(crate) location: String,
}

/// Local preflight evidence, including explicit verification gaps and snapshot limits.
/// A report without blockers does not establish that installation or execution will succeed.
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
    /// Retain at most `MAX_ENTRIES` blockers and mark omitted findings as truncation.
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

/// Read bounded regular-file bytes, retaining distinct absence, size, and access failures.
/// Callers use those states as snapshot identities without exposing error contents.
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    read_bounded_file(path, limit).map_err(|error| match error {
        ReadError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        ReadError::CapacityExceeded => "oversized",
        _ => "unreadable_or_non_regular",
    })
}

/// Identify file bytes by SHA-256, or inspect metadata only when `limit` is zero.
/// Metadata-only entries never read journals or enumerate installed trees.
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

/// Inspect an explicit workspace root offline, optionally selecting an exact member name.
///
/// Read failures, incompatible metadata, pending recovery, and exhausted budgets
/// become blockers rather than triggering repair. This function never opens the
/// store, acquires transaction locks, resolves credentials, or starts installation.
/// Rechecking identities catches persistent replacements, but does not exclude
/// writers or detect every change followed by restoration.
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
            "lock_schemas": [4, 5, 6, tapid_lockfile::LOCKFILE_VERSION, 8, tapid_lockfile::LIFECYCLE_LOCKFILE_VERSION, tapid_lockfile::COPIED_SOURCE_LOCKFILE_VERSION],
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
    let (recovery_path, pending) = lifecycle_journal::inspect_pending(&project);
    match pending {
        Ok(false) => report.recovery = "none_detected",
        Ok(true) => {
            report.recovery = "pending";
            report.block("RECOVERY_REQUIRED", &recovery_path);
        }
        Err(_) => {
            report.recovery = "unknown";
            report.block("RECOVERY_STATE_INVALID", &recovery_path);
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
        match Workspace::discover_bounded_with_member_source(
            &project,
            MAX_ENTRIES,
            4096,
            |path, text| {
                snapshots.insert(
                    path.to_path_buf(),
                    (MAX_FILE_BYTES, digest_bytes(text.as_bytes())),
                );
            },
        ) {
            Ok(workspace) => Some(workspace),
            Err(tapid_manifest::WorkspaceError::DiscoveryLimitExceeded) => {
                report.truncated = true;
                report.block("WORKSPACE_DISCOVERY_LIMIT", &manifest_path);
                None
            }
            Err(
                tapid_manifest::WorkspaceError::Manifest { path, .. }
                | tapid_manifest::WorkspaceError::ManifestRead { path, .. },
            ) => {
                report.block("WORKSPACE_INVALID", path);
                None
            }
            Err(_) => {
                report.block("WORKSPACE_INVALID", &manifest_path);
                None
            }
        }
    } else {
        None
    };
    #[cfg(test)]
    tests::after_workspace_discovery();
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
            inspect_manifest(member.manifest(), member.path(), workspace, &mut report);
            if let Some(dir) = member.path().parent() {
                inspect_installation(dir, &mut report, &mut snapshots);
            }
        }
    }
    let lock_path = project.join("tapid.lock");
    match read(&lock_path, MAX_FILE_BYTES) {
        Ok(bytes)
            if serde_json::from_slice::<Value>(&bytes)
                .is_ok_and(|value| value["lockfileVersion"] == 11) =>
        {
            inspect_native_selection(&bytes, &manifest_bytes, workspace.as_ref(), &mut report);
        }
        Ok(bytes)
            if serde_json::from_slice::<Value>(&bytes)
                .is_ok_and(|value| value["lockfileVersion"] == 8) =>
        {
            inspect_imported_lock(&bytes, &manifest_bytes, workspace.as_ref(), &mut report);
        }
        Ok(bytes) => match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| Lockfile::from_json(text).ok())
        {
            Some(lock) => {
                if let (Some(manifest), Ok(bytes)) = (&manifest, &manifest_bytes) {
                    let root_matches = match lock.validate_replay(&digest_bytes(bytes)) {
                        Ok(()) => true,
                        Err(LockfileError::RootManifestDigestMismatch { .. }) => {
                            report.manifest_lock_agreement = "mismatch";
                            report.block("LOCK_MANIFEST_MISMATCH", &lock_path);
                            false
                        }
                        Err(_) => {
                            report.block("LOCK_REPLAY_UNSUPPORTED", &lock_path);
                            true
                        }
                    };
                    if root_matches && let Some(config) = &config {
                        let bindings_match = crate::application::replay::validate_root_bindings(
                            &lock, manifest, config,
                        )
                        .is_ok();
                        if !bindings_match {
                            report.manifest_lock_agreement = "mismatch";
                            report.block("LOCK_ROOT_BINDINGS_INVALID", &lock_path);
                        }
                        if let Some(workspace) = &workspace {
                            match workspace_matches(&project, manifest, config, &lock, workspace) {
                                Ok(()) if bindings_match => {
                                    report.manifest_lock_agreement = "match"
                                }
                                Ok(()) => {}
                                Err(code) => {
                                    if code == "LOCK_WORKSPACE_MISMATCH" {
                                        report.manifest_lock_agreement = "mismatch";
                                    }
                                    report.block(code, &lock_path);
                                }
                            }
                        }
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

/// Record ownership evidence after fingerprinting the tree and marker before validation.
/// An absent tree still requires a valid marker when one exists; managed ownership
/// does not verify installed bytes, links, or correspondence with the lockfile.
fn inspect_installation(
    project: &Path,
    report: &mut StatusReport,
    snapshots: &mut BTreeMap<PathBuf, (usize, String)>,
) {
    let path = project.join("node_modules");
    snapshots.insert(path.clone(), (0, fingerprint(&path, 0)));
    let marker = project.join(".tapid-managed");
    snapshots.insert(marker.clone(), (1024, fingerprint(&marker, 1024)));
    let valid = validate_managed_node_modules(project).is_ok();
    #[cfg(test)]
    tests::after_ownership_validation(project);
    let state = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && valid => "absent",
        Ok(_) if valid => "managed",
        _ => {
            report.block("INSTALLATION_NOT_MANAGED", &path);
            "unmanaged_or_invalid"
        }
    };
    report
        .installation
        .push(json!({"path": path.display().to_string(), "state": state}));
}

/// Reuse install/replay planning to check workspace identities and dependency edges.
/// No registry lookup or materialization occurs; failures distinguish unsupported
/// workspace declarations from a lock that disagrees with supported declarations.
fn workspace_matches(
    project: &Path,
    manifest: &PackageManifest,
    config: &RegistryConfig,
    lock: &Lockfile,
    discovered: &Workspace,
) -> Result<(), &'static str> {
    let workspace =
        crate::online::workspace_materialization_from_discovered(project, config, None, discovered)
            .map_err(|_| "WORKSPACE_COMPATIBILITY_BLOCKER")?;
    if !crate::online::workspace_matches_lock(&workspace, lock) {
        return Err("LOCK_WORKSPACE_MISMATCH");
    }
    let dependencies =
        crate::online::resolved_workspace_registry_dependencies(manifest, &workspace, config)
            .map_err(|_| "WORKSPACE_COMPATIBILITY_BLOCKER")?;
    crate::application::replay::validate_workspace_dependency_edges(&workspace, &dependencies, lock)
        .map_err(|_| "LOCK_WORKSPACE_MISMATCH")
}

/// Inspect complete selection snapshots without consulting installed source evidence.
fn inspect_native_selection(
    bytes: &[u8],
    manifest_bytes: &Result<Vec<u8>, &'static str>,
    workspace: Option<&Workspace>,
    report: &mut StatusReport,
) {
    let lock_path = report.project.join("tapid.lock");
    let Some(selection) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| tapid_lockfile::NativeSelection::from_json(text).ok())
    else {
        report.block("LOCKFILE_INVALID", &lock_path);
        return;
    };
    report.checks_not_performed.extend([
        "native_selection_registry_routing_and_declaration_bindings",
        "native_selection_installed_profile_and_source_integrity",
    ]);
    let Ok(root) = manifest_bytes else { return };
    let members = workspace
        .into_iter()
        .flat_map(|workspace| workspace.members())
        .map(|member| {
            let relative = member
                .path()
                .parent()
                .and_then(|path| path.strip_prefix(&report.project).ok())
                .ok_or(())?;
            let key = relative
                .components()
                .map(|part| part.as_os_str().to_str().ok_or(()))
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            Ok((
                key,
                digest_bytes(&read(member.path(), MAX_FILE_BYTES).map_err(|_| ())?),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, ()>>();
    let result = members
        .map_err(|_| "LOCK_WORKSPACE_MISMATCH")
        .and_then(|members| {
            selection
                .validate_replay(&digest_bytes(root), &members)
                .map_err(|error| match error {
                    LockfileError::RootManifestDigestMismatch { .. } => "LOCK_MANIFEST_MISMATCH",
                    _ => "LOCK_WORKSPACE_MISMATCH",
                })
        });
    match result {
        Ok(()) => report.manifest_lock_agreement = "match",
        Err(code) => {
            report.manifest_lock_agreement = "mismatch";
            report.block(code, &lock_path);
        }
    }
}

/// Validate schema-8 root and workspace receipts with bounded manifest reads.
/// Routing, platform selection, and ancestor layout remain explicitly unchecked;
/// a workspace match cannot erase an already established root mismatch.
fn inspect_imported_lock(
    bytes: &[u8],
    manifest_bytes: &Result<Vec<u8>, &'static str>,
    workspace: Option<&Workspace>,
    report: &mut StatusReport,
) {
    let lock_path = report.project.join("tapid.lock");
    let Some(lock) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| ImportedNpmLockfile::from_json(text).ok())
    else {
        report.block("LOCKFILE_INVALID", &lock_path);
        return;
    };
    report
        .checks_not_performed
        .push("imported_npm_registry_routing_and_platform_selection");
    report
        .checks_not_performed
        .push("imported_npm_ancestor_installation_layout");
    let Ok(manifest) = manifest_bytes
        .as_ref()
        .map_err(|_| ())
        .and_then(|bytes| std::str::from_utf8(bytes).map_err(|_| ()))
    else {
        return;
    };
    if lock
        .validate_replay(manifest, &digest_bytes(manifest.as_bytes()))
        .is_err()
    {
        report.manifest_lock_agreement = "mismatch";
        report.block("LOCK_MANIFEST_MISMATCH", &lock_path);
    }
    let Some(workspace) = workspace else {
        return;
    };
    // Use bounded reads from the discovered members. Import/replay's collection also
    // enforces install layout rules and uses unbounded reads, so it is not a preflight reader.
    let manifests = workspace
        .members()
        .iter()
        .map(|member| {
            let relative = member
                .path()
                .parent()
                .and_then(|path| path.strip_prefix(&report.project).ok())
                .ok_or(())?;
            let key = relative
                .components()
                .map(|part| part.as_os_str().to_str().ok_or(()))
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            let bytes = read(member.path(), MAX_FILE_BYTES).map_err(|_| ())?;
            let digest = digest_bytes(&bytes);
            let text = String::from_utf8(bytes).map_err(|_| ())?;
            Ok((key, (text, digest)))
        })
        .collect::<Result<BTreeMap<_, _>, ()>>();
    match manifests {
        Ok(manifests) if lock.validate_workspace_manifests(&manifests).is_ok() => {
            if report.manifest_lock_agreement != "mismatch" {
                report.manifest_lock_agreement = "match";
            }
        }
        _ => {
            report.manifest_lock_agreement = "mismatch";
            report.block("LOCK_WORKSPACE_MISMATCH", &lock_path);
        }
    }
}

/// Check dependency specs independently, then apply override-aware checks only to the root.
/// Root peers are declarations; member peers participate in workspace validation.
/// Blocker locations identify manifest fields using escaped JSON pointer keys.
fn inspect_manifest(
    manifest: &PackageManifest,
    path: &Path,
    workspace: &Workspace,
    report: &mut StatusReport,
) {
    for (kind, dependencies) in manifest.dependency_sections() {
        if kind == tapid_manifest::DependencyKind::PeerDependencies && path == workspace.root_path()
        {
            continue;
        }
        let section = kind.field_name();
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
    // Only the root contributes overrides to install and replay. Member specs
    // have already been checked independently above.
    if path != workspace.root_path() {
        return;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use tapid_test_support::TempProject;

    #[derive(Clone, Copy)]
    enum MarkerChange {
        Invalid,
        Missing,
        Directory,
    }

    thread_local! {
        static MARKER_CHANGE: Cell<Option<MarkerChange>> = const { Cell::new(None) };
        static MANIFEST_REPLACEMENT: RefCell<Option<(PathBuf, Vec<u8>)>> = const { RefCell::new(None) };
    }

    struct FaultGuard;
    impl Drop for FaultGuard {
        fn drop(&mut self) {
            MARKER_CHANGE.set(None);
            MANIFEST_REPLACEMENT.with(|replacement| replacement.borrow_mut().take());
        }
    }

    pub(super) fn after_workspace_discovery() {
        let replacement = MANIFEST_REPLACEMENT.with(|replacement| replacement.borrow_mut().take());
        if let Some((path, bytes)) = replacement {
            fs::write(path, bytes).unwrap();
        }
    }

    #[test]
    fn status_recognizes_native_selection_and_blocks_stale_manifest() {
        let project = TempProject::new("status-native-selection").unwrap();
        let manifest = br#"{"name":"root","version":"1.0.0"}"#;
        project.write("package.json", manifest).unwrap();
        let selection = tapid_lockfile::NativeSelection::new(
            "0".into(),
            "0".into(),
            digest_bytes(manifest),
            Default::default(),
            vec![],
            Default::default(),
        )
        .unwrap();
        let lock = selection.to_json().unwrap();
        project.write("tapid.lock", lock.as_bytes()).unwrap();
        let report = inspect(project.path(), None);
        assert_eq!(report.manifest_lock_agreement, "match");
        assert!(!report.blockers.iter().any(|b| b.code == "LOCKFILE_INVALID"));
        project
            .write("package.json", br#"{"name":"root","version":"2.0.0"}"#)
            .unwrap();
        let report = inspect(project.path(), None);
        assert_eq!(report.manifest_lock_agreement, "mismatch");
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.code == "LOCK_MANIFEST_MISMATCH")
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock.as_bytes()
        );
    }

    #[test]
    fn status_blocks_member_replacement_after_discovery_even_when_lock_matches_replacement() {
        let project = TempProject::new("status-member-change").unwrap();
        let root = br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
        let original = br#"{"name":"member","version":"1.0.0"}"#;
        let replacement =
            br#"{"name":"member","version":"1.0.0","dependencies":{"bad":"https://example.invalid/pkg.tgz"}}"#;
        project.write("package.json", root).unwrap();
        let path = project
            .write("packages/member/package.json", original)
            .unwrap();
        let mut lock = Lockfile::new(&digest_bytes(root)).unwrap();
        let source =
            tapid_lockfile::LocalWorkspaceSource::new("packages/member", "member", "1.0.0")
                .unwrap();
        let package =
            tapid_lockfile::LockedWorkspacePackage::new(source, &digest_bytes(replacement))
                .unwrap();
        lock.insert_workspace_package(package).unwrap();
        lock.set_roots(
            lock.workspace_packages()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        MANIFEST_REPLACEMENT
            .with(|pending| *pending.borrow_mut() = Some((path.clone(), replacement.to_vec())));
        let _guard = FaultGuard;
        let report = inspect(project.path(), None);
        assert_eq!(report.assessment, "blocked");
        assert!(report.blockers.iter().any(|b| b.code == "SNAPSHOT_CHANGED"));
        assert_eq!(report.snapshot["changes_detected"], true);
        assert_eq!(report.manifest_lock_agreement, "unknown");
        assert_eq!(fs::read(path).unwrap(), replacement);
    }

    // Mutate only this thread's temporary fixture after the real ownership read.
    pub(super) fn after_ownership_validation(project: &Path) {
        let Some(change) = MARKER_CHANGE.replace(None) else {
            return;
        };
        let marker = project.join(".tapid-managed");
        fs::remove_file(&marker).unwrap();
        match change {
            MarkerChange::Invalid => fs::write(marker, b"invalid replacement\n").unwrap(),
            MarkerChange::Missing => {}
            MarkerChange::Directory => fs::create_dir(marker).unwrap(),
        }
    }

    #[test]
    fn status_blocks_persistent_marker_changes_after_ownership_validation() {
        for change in [
            MarkerChange::Invalid,
            MarkerChange::Missing,
            MarkerChange::Directory,
        ] {
            let project = TempProject::new("status-marker-change").unwrap();
            let manifest = br#"{"name":"app","version":"1.0.0"}"#;
            project.write("package.json", manifest).unwrap();
            let lock = Lockfile::new(&digest_bytes(manifest)).unwrap();
            project
                .write("tapid.lock", lock.to_json().unwrap().as_bytes())
                .unwrap();
            project
                .write(".tapid-managed", b"tapid-managed-v1\n")
                .unwrap();
            project.write("node_modules/keep", b"untouched").unwrap();
            MARKER_CHANGE.set(Some(change));
            let _guard = FaultGuard;
            let report = inspect(project.path(), None);
            assert_eq!(report.assessment, "blocked");
            assert!(report.blockers.iter().any(|b| b.code == "SNAPSHOT_CHANGED"));
            assert_eq!(report.snapshot["changes_detected"], true);
            assert_eq!(report.manifest_lock_agreement, "unknown");
            assert_eq!(
                fs::read(project.path().join("node_modules/keep")).unwrap(),
                b"untouched"
            );
        }
    }
}
