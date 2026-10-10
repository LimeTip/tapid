use super::*;
use crate::application::outcome::{ChangeState, ErrorKind, RetryAdvice};
use base64::Engine as _;
use tapid_test_support::TempProject;

#[test]
fn missing_replay_lock_has_a_typed_unchanged_outcome() {
    let project = TempProject::new("typed-missing-lock").unwrap();
    let original = br#"{"name":"app","version":"1.0.0"}"#;
    project.write("package.json", original).unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&project.path().join("store")),
        InstallMode::Frozen,
        None,
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::LockfileMissing);
    assert_eq!(failure.outcome.state, ChangeState::Unchanged);
    assert_eq!(failure.retry, RetryAdvice::AfterCorrection);
    assert!(failure.outcome.changed_files.is_empty());
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        original
    );
    assert!(!project.path().join("node_modules").exists());
}

const ARTIFACT: &str = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
const INTEGRITY: &str = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";

fn project_with_fixture(label: &str, integrity: &str) -> (TempProject, PathBuf) {
    let project = TempProject::new(label).unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let fixture = project
        .write(
            "registry.json",
            serde_json::json!({"packages": [{
                "registry": "https://registry.npmjs.org", "name": "plugin", "version": "1.0.0",
                "integrity": integrity, "artifact": ARTIFACT,
            }]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    (project, fixture)
}

#[test]
fn failed_resolution_reports_rollback_and_preserves_its_category() {
    let (project, fixture) = project_with_fixture("typed-resolution", INTEGRITY);
    let original = fs::read(project.path().join("package.json")).unwrap();
    let failure = run(
        project.path(),
        Some("missing@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Resolution);
    assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    assert_eq!(failure.retry, RetryAdvice::AfterCorrection);
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        original
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    assert!(failure.outcome.changed_files.is_empty());
}

#[test]
fn integrity_failure_is_typed_and_rolls_back_the_manifest() {
    use base64::Engine as _;
    let bad = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode([0; 64])
    );
    let (project, fixture) = project_with_fixture("typed-integrity", &bad);
    let original = fs::read(project.path().join("package.json")).unwrap();
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Integrity);
    assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        original
    );
    assert!(!project.path().join("node_modules").exists());
    assert!(!project.path().join("store/trees").exists());
}

#[derive(Clone, Copy)]
enum Fault {
    AfterCommit,
    CleanupPending,
    RollbackPending,
}
thread_local! { static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) }; }
struct FaultGuard;
impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAULT.set(None);
    }
}
fn inject(fault: Fault) -> FaultGuard {
    FAULT.set(Some(fault));
    FaultGuard
}

// Mutate only this thread's temporary fixture at real filesystem boundaries.
pub(super) fn checkpoint(point: &str, project: &Path, owner: &str) -> Result<(), OperationalError> {
    match (FAULT.get(), point) {
        (Some(Fault::AfterCommit), "after_commit") => Err(OperationalError::new(
            ErrorKind::Transaction,
            "injected post-commit failure",
        )),
        (Some(Fault::CleanupPending), "after_commit") => {
            // Windows rejects reads through another handle while the lock is held.
            // Use the owning guard's identity to obstruct its cleanup path.
            fs::write(
                project.join(format!(".tapid-node-modules-old-{}", owner.trim_end())),
                b"not a directory",
            )
            .unwrap();
            Ok(())
        }
        (Some(Fault::RollbackPending), "manifest_written") => {
            fs::remove_file(project.join("package.json")).unwrap();
            fs::create_dir(project.join("package.json")).unwrap();
            Err(OperationalError::new(
                ErrorKind::Transaction,
                "injected failure with obstructed rollback",
            ))
        }
        _ => Ok(()),
    }
}

#[test]
fn failure_after_commit_keeps_the_committed_manifest_lock_and_packages() {
    let (project, fixture) = project_with_fixture("typed-post-commit", INTEGRITY);
    let _fault = inject(Fault::AfterCommit);
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.outcome.state, ChangeState::Committed);
    assert_eq!(failure.retry, RetryAdvice::DoNotRepeat);
    assert!(failure.recovery_error.is_none());
    assert!(
        read_manifest(&project.path().join("package.json"))
            .unwrap()
            .dependencies()
            .contains_key("plugin")
    );
    assert!(
        project
            .path()
            .join("node_modules/plugin/package.json")
            .is_file()
    );
    let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
    assert_eq!(lock.packages().len(), 1);
    assert_eq!(failure.outcome.changed_files.len(), 3);
    assert!(
        !project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
    assert!(
        failure
            .outcome
            .project_dir
            .join("store/trees")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
}

#[test]
fn committed_cleanup_failure_preserves_the_decision_and_requires_no_repeat() {
    let (project, fixture) = project_with_fixture("typed-cleanup-pending", INTEGRITY);
    let _fault = inject(Fault::CleanupPending);
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.outcome.state, ChangeState::CommittedCleanupPending);
    assert_eq!(failure.retry, RetryAdvice::DoNotRepeat);
    assert_eq!(
        failure.recovery_error.as_ref().unwrap().kind,
        ErrorKind::Recovery
    );
    assert!(
        read_manifest(&project.path().join("package.json"))
            .unwrap()
            .dependencies()
            .contains_key("plugin")
    );
    assert!(
        project
            .path()
            .join("node_modules/plugin/package.json")
            .is_file()
    );
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(journal["state"], "Committed");
}

#[test]
fn obstructed_rollback_is_recovery_required_instead_of_rolled_back() {
    let (project, fixture) = project_with_fixture("typed-rollback-pending", INTEGRITY);
    let _fault = inject(Fault::RollbackPending);
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.outcome.state, ChangeState::RecoveryRequired);
    assert_eq!(failure.retry, RetryAdvice::RecoverFirst);
    assert_eq!(
        failure.recovery_error.as_ref().unwrap().kind,
        ErrorKind::Recovery
    );
    assert!(
        project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .is_file()
    );
}

#[test]
fn replay_preserves_the_lock_mismatch_category() {
    let (project, fixture) = project_with_fixture("typed-lock-mismatch", INTEGRITY);
    let store = project.path().join("store");
    run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let mut manifest = fs::read(project.path().join("package.json")).unwrap();
    manifest.push(b' ');
    project.write("package.json", &manifest).unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Frozen,
        None,
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::LockManifestMismatch);
    assert_eq!(failure.outcome.state, ChangeState::Unchanged);
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(
        project
            .path()
            .join("node_modules/plugin/package.json")
            .is_file()
    );
}

#[test]
fn competing_activation_returns_typed_contention_without_changes() {
    let project = TempProject::new("typed-contention").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let _lock = ActivationLock::acquire(project.path()).unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&project.path().join("store")),
        InstallMode::Frozen,
        None,
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::ProjectBusy);
    assert_eq!(failure.outcome.state, ChangeState::Unchanged);
    assert_eq!(failure.retry, RetryAdvice::AfterContention);
}

#[test]
fn missing_replay_tree_preserves_the_store_error_category() {
    let (project, fixture) = project_with_fixture("typed-store-missing", INTEGRITY);
    let store = project.path().join("store");
    run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    fs::remove_dir_all(store.join("trees")).unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Offline,
        None,
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::StoreUnavailable);
    assert_eq!(failure.outcome.state, ChangeState::Unchanged);
    assert!(
        project
            .path()
            .join("node_modules/plugin/package.json")
            .is_file()
    );
}

#[test]
fn materialization_failure_preserves_previous_project_and_store_state() {
    let (project, fixture) = project_with_fixture("typed-materialization", INTEGRITY);
    let original = fs::read(project.path().join("package.json")).unwrap();
    let prior_lock = Lockfile::new(&online::root_digest(project.path()).unwrap())
        .unwrap()
        .to_json()
        .unwrap();
    project.write("tapid.lock", prior_lock.as_bytes()).unwrap();
    project.write("node_modules/KEEP", b"user data").unwrap();
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Materialization);
    assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    assert!(failure.recovery_error.is_none());
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        prior_lock.as_bytes()
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert!(!project.path().join("store/trees").exists());
    assert!(fs::read_dir(project.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".bak")
    }));
}

#[test]
fn peer_failure_preserves_the_resolver_source() {
    let (project, fixture) = project_with_fixture("typed-peer", INTEGRITY);
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    metadata["packages"][0]["peerDependencies"] = serde_json::json!({"react": "^18"});
    fs::write(&fixture, metadata.to_string()).unwrap();
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::PeerDependency);
    assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    assert!(
        std::error::Error::source(failure.error.as_ref())
            .unwrap()
            .downcast_ref::<tapid_resolver::ResolveError>()
            .is_some()
    );
}

#[test]
fn invalid_archive_is_typed_without_activating_any_packages() {
    use base64::Engine as _;
    use sha2::Digest as _;
    let (project, fixture) = project_with_fixture("typed-archive", INTEGRITY);
    let bytes = b"not a tar archive";
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    metadata["packages"][0]["artifact"] = format!(
        "base64:{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
    .into();
    metadata["packages"][0]["integrity"] = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(bytes))
    )
    .into();
    fs::write(&fixture, metadata.to_string()).unwrap();
    let failure = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&project.path().join("store")),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Archive);
    assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn successful_install_and_replay_report_effective_project_changes_and_warnings() {
    use crate::application::outcome::Warning;
    let (project, fixture) = project_with_fixture("typed-success", INTEGRITY);
    let store = project.path().join("store");
    let report = run(
        project.path(),
        Some("plugin@1.0.0"),
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        true,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.outcome.state, ChangeState::Committed);
    assert_eq!(
        report.outcome.project_dir,
        project.path().canonicalize().unwrap()
    );
    assert_eq!(
        report.outcome.warnings,
        [Warning::UnverifiedRegistryArtifactsAllowed]
    );
    assert_eq!(report.outcome.changed_files.len(), 3);
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    // An ordinary install now preserves pinned archive metadata. Compare
    // different valid snapshots directly to cover changed lock records.
    let current = Lockfile::from_json(std::str::from_utf8(&lock).unwrap()).unwrap();
    let mut previous: serde_json::Value = serde_json::from_slice(&lock).unwrap();
    for package in previous["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] =
            serde_json::json!("https://registry.npmjs.org/plugin/-/plugin-1.0.0.tgz");
    }
    let previous = serde_json::to_vec(&previous).unwrap();
    let changes = PackageChanges::between(Some(&previous), &current).unwrap();
    assert_eq!(changes.changed, 1);
    assert_eq!(changes.added + changes.reused + changes.removed, 0);
    let replay = run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Frozen,
        None,
        false,
        |_| {},
    )
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.outcome.state, ChangeState::Committed);
    assert_eq!(
        replay.outcome.changed_files,
        [project.path().canonicalize().unwrap().join("node_modules")]
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
}

#[test]
fn failed_previous_recovery_reports_project_outputs_for_install_and_outdated() {
    for command in ["install", "outdated"] {
        let project = TempProject::new(command).unwrap();
        let original = br#"{"name":"app","version":"1.0.0"}"#;
        project.write("package.json", original).unwrap();
        project
            .write(".tapid-lifecycle-journal.json", b"{}")
            .unwrap();
        let failure = if command == "install" {
            run(
                project.path(),
                None,
                Some(&project.path().join("store")),
                InstallMode::Online,
                None,
                false,
                |_| {},
            )
            .unwrap_err()
        } else {
            crate::application::lifecycle::outdated_report(project.path(), None, None).unwrap_err()
        };
        assert_eq!(failure.error.kind, ErrorKind::Recovery);
        assert_eq!(failure.outcome.state, ChangeState::RecoveryRequired);
        assert_eq!(failure.retry, RetryAdvice::RecoverFirst);
        let root = project.path().canonicalize().unwrap();
        assert_eq!(
            failure.outcome.changed_files,
            [
                root.join("package.json"),
                root.join("tapid.lock"),
                root.join("node_modules")
            ]
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
            b"{}"
        );
    }
}

#[test]
fn invalid_fixture_package_fields_preserve_the_registry_metadata_category() {
    use std::error::Error;
    for (field, invalid) in [
        ("registry", "not-an-origin"),
        ("name", "invalid name"),
        ("version", "not-a-version"),
        ("integrity", "not-a-digest"),
    ] {
        let (project, fixture) = project_with_fixture(field, INTEGRITY);
        let original = fs::read(project.path().join("package.json")).unwrap();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
        metadata["packages"][0][field] = invalid.into();
        fs::write(&fixture, serde_json::to_vec(&metadata).unwrap()).unwrap();
        let failure = run(
            project.path(),
            Some("plugin@1.0.0"),
            Some(&project.path().join("store")),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(
            failure.error.kind,
            ErrorKind::RegistryMetadata,
            "field: {field}"
        );
        assert!(failure.error.source().is_some(), "field: {field}");
        assert_eq!(failure.outcome.state, ChangeState::RolledBack);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            original
        );
        assert!(!project.path().join("node_modules").exists());
    }
}

#[test]
fn invalid_fixture_artifact_preserves_metadata_and_transport_categories() {
    use std::error::Error;
    for (label, kind) in [
        ("encoding", ErrorKind::RegistryMetadata),
        ("missing", ErrorKind::RegistryTransport),
    ] {
        let (project, fixture) = project_with_fixture(label, INTEGRITY);
        let artifact = if label == "encoding" {
            "base64:invalid!".to_owned()
        } else {
            project
                .path()
                .join("missing.tgz")
                .to_str()
                .unwrap()
                .to_owned()
        };
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
        metadata["packages"][0]["artifact"] = artifact.into();
        fs::write(&fixture, serde_json::to_vec(&metadata).unwrap()).unwrap();
        let failure = run(
            project.path(),
            Some("plugin@1.0.0"),
            Some(&project.path().join("store")),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.error.kind, kind);
        assert!(failure.error.source().is_some());
        assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    }
}

#[test]
fn online_platform_fallback_discards_only_incompatible_locked_selections() {
    for newer in [false, true] {
        let (project, fixture) = project_with_fixture("online-platform-fallback", INTEGRITY);
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*","stable":"*"}}"#,
            )
            .unwrap();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
        let base = metadata["packages"][0].clone();
        let mut stable = base.clone();
        stable["name"] = "stable".into();
        metadata["packages"]
            .as_array_mut()
            .unwrap()
            .push(stable.clone());
        fs::write(&fixture, metadata.to_string()).unwrap();
        let store = project.path().join("store");
        run(
            project.path(),
            None,
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap();
        let lock_path = project.path().join("tapid.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
        let stable_key = lock["rootBindings"]["stable"].as_str().unwrap().to_owned();
        let stable_pin = lock["packages"][&stable_key].clone();
        let old_key = lock["rootBindings"]["plugin"].as_str().unwrap().to_owned();
        let context = "os=unsupported;cpu=;libc=";
        let new_key = old_key.replace("os=;cpu=;libc=", context);
        let mut package = lock["packages"]
            .as_object_mut()
            .unwrap()
            .remove(&old_key)
            .unwrap();
        package["platformContext"] = context.into();
        package["treeDigest"] = format!("sha256-{}", "0".repeat(64)).into();
        package["artifactIntegrity"] = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode([0; 64])
        )
        .into();
        lock["packages"][&new_key] = package;
        lock["rootBindings"]["plugin"] = new_key.clone().into();
        for root in lock["roots"].as_array_mut().unwrap() {
            if root.as_str() == Some(&old_key) {
                *root = new_key.clone().into();
            }
        }
        let incompatible_lock = lock.to_string();
        fs::write(&lock_path, &incompatible_lock).unwrap();
        if newer {
            let mut next = base;
            next["version"] = "2.0.0".into();
            metadata["packages"].as_array_mut().unwrap().push(next);
        }
        stable["version"] = "2.0.0".into();
        metadata["packages"].as_array_mut().unwrap().push(stable);
        fs::write(&fixture, metadata.to_string()).unwrap();
        let report = run(
            project.path(),
            None,
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap();
        assert!(!report.replayed);
        let resolved = read_lock(&lock_path).unwrap();
        let plugin: tapid_lockfile::LockfilePackageKey =
            resolved.root_bindings()["plugin"].parse().unwrap();
        assert_eq!(
            plugin.version.to_string(),
            if newer { "2.0.0" } else { "1.0.0" }
        );
        assert_eq!(resolved.root_bindings()["stable"], stable_key);
        let resolved_json: serde_json::Value =
            serde_json::from_str(&resolved.to_json().unwrap()).unwrap();
        assert_eq!(resolved_json["packages"][&stable_key], stable_pin);
        fs::write(&lock_path, &incompatible_lock).unwrap();
        for mode in [
            InstallMode::Frozen,
            InstallMode::Offline,
            InstallMode::Ci,
            InstallMode::CiOffline,
        ] {
            let failure = run(
                project.path(),
                None,
                Some(&store),
                mode,
                Some(&fixture),
                false,
                |_| {},
            )
            .unwrap_err();
            assert_eq!(failure.error.kind, ErrorKind::Lockfile, "{failure:?}");
            assert!(failure.error.to_string().contains("different platform"));
            assert!(failure.error.to_string().contains("tapid update"));
            assert_eq!(fs::read_to_string(&lock_path).unwrap(), incompatible_lock);
        }
        project
            .write(
                "tapid.toml",
                b"[registries.default]\nurl='https://different.example'\n",
            )
            .unwrap();
        let failure = run(
            project.path(),
            None,
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.error.kind, ErrorKind::RegistryConfiguration);
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), incompatible_lock);
    }
}

#[test]
fn repeated_online_install_preserves_locked_selection() {
    let (project, fixture) = project_with_fixture("locked-selection", INTEGRITY);
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*"}}"#,
        )
        .unwrap();
    let store = project.path().join("store");
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let original_lock = fs::read(project.path().join("tapid.lock")).unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    let mut newer = metadata["packages"][0].clone();
    newer["version"] = "1.1.0".into();
    metadata["packages"].as_array_mut().unwrap().push(newer);
    fs::write(&fixture, metadata.to_string()).unwrap();
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        original_lock
    );
    fs::remove_file(&fixture).unwrap();
    let replay = run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        original_lock
    );
}

#[test]
fn frozen_install_hydrates_exact_artifact_from_cold_store() {
    let (project, fixture) = project_with_fixture("frozen-hydration", INTEGRITY);
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*"}}"#,
        )
        .unwrap();
    let store = project.path().join("store");
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let lock_path = project.path().join("tapid.lock");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = "https://registry.npmjs.org/plugin/-/plugin-1.0.0.tgz".into();
    }
    fs::write(&lock_path, lock.to_string()).unwrap();
    let original_lock = fs::read(&lock_path).unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    metadata["packages"][0]["dependencies"] =
        serde_json::json!({"ignored":"unsupported registry requirement"});
    let mut newer = metadata["packages"][0].clone();
    newer["version"] = "1.1.0".into();
    newer["artifact"] = "missing-newer-artifact".into();
    metadata["packages"].as_array_mut().unwrap().push(newer);
    fs::write(&fixture, metadata.to_string()).unwrap();
    let cold = project.path().join("cold-store");
    run(
        project.path(),
        None,
        Some(&cold),
        InstallMode::Frozen,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    assert_eq!(fs::read(&lock_path).unwrap(), original_lock);
    run(
        project.path(),
        None,
        Some(&cold),
        InstallMode::Offline,
        None,
        false,
        |_| {},
    )
    .unwrap();
}

#[test]
fn changed_roots_keep_compatible_locked_versions() {
    let (project, fixture) = project_with_fixture("changed-roots-locked", INTEGRITY);
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*"}}"#,
        )
        .unwrap();
    let store = project.path().join("store");
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    let mut newer = metadata["packages"][0].clone();
    newer["version"] = "1.1.0".into();
    metadata["packages"].as_array_mut().unwrap().push(newer);
    let mut other = metadata["packages"][0].clone();
    other["name"] = "other".into();
    metadata["packages"].as_array_mut().unwrap().push(other);
    fs::write(&fixture, metadata.to_string()).unwrap();
    run(
        project.path(),
        Some("other@*"),
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
    assert!(
        lock.packages_typed()
            .unwrap()
            .iter()
            .any(|(key, _)| key.name.as_str() == "plugin" && key.version.to_string() == "1.0.0")
    );
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Refresh,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
    assert!(
        lock.packages_typed()
            .unwrap()
            .iter()
            .any(|(key, _)| key.name.as_str() == "plugin" && key.version.to_string() == "1.1.0")
    );
}

fn pinned_fixture_project(label: &str) -> (TempProject, PathBuf, PathBuf) {
    let (project, fixture) = project_with_fixture(label, INTEGRITY);
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*"}}"#,
        )
        .unwrap();
    let warm = project.path().join("warm-store");
    run(
        project.path(),
        None,
        Some(&warm),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let lock_path = project.path().join("tapid.lock");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = "https://registry.npmjs.org/plugin/-/plugin-1.0.0.tgz".into();
    }
    fs::write(&lock_path, lock.to_string()).unwrap();
    let cold = project.path().join("cold-store");
    (project, fixture, cold)
}

#[test]
fn frozen_hydration_failures_preserve_lock_and_installed_tree() {
    for fault in [
        "integrity",
        "tree",
        "url",
        "provenance",
        "identity",
        "routing",
        "platform",
    ] {
        let (project, fixture, cold) = pinned_fixture_project(&format!("frozen-failure-{fault}"));
        let lock_path = project.path().join("tapid.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
        let package = lock["packages"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        match fault {
            "integrity" => {
                package["artifactIntegrity"] = format!(
                    "sha512-{}",
                    base64::engine::general_purpose::STANDARD.encode([0; 64])
                )
                .into()
            }
            "tree" => package["treeDigest"] = format!("sha256-{}", "0".repeat(64)).into(),
            "url" => {
                package.as_object_mut().unwrap().remove("artifactUrl");
            }
            "provenance" => package["registryIntegrityDeclared"] = false.into(),
            "identity" => {
                let mut metadata: serde_json::Value =
                    serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
                metadata["packages"][0]["version"] = "1.1.0".into();
                fs::write(&fixture, metadata.to_string()).unwrap();
            }
            "routing" => {
                project
                    .write(
                        "tapid.toml",
                        b"[registries.default]\nurl='https://different.example'\n",
                    )
                    .unwrap();
            }
            "platform" => {
                let packages = lock["packages"].as_object_mut().unwrap();
                let old_key = packages.keys().next().unwrap().clone();
                let mut package = packages.remove(&old_key).unwrap();
                let context = "os=unsupported;cpu=;libc=";
                package["platformContext"] = context.into();
                let new_key = old_key.replace("os=;cpu=;libc=", context);
                packages.insert(new_key.clone(), package);
                lock["roots"][0] = new_key.clone().into();
                lock["rootBindings"]["plugin"] = new_key.into();
            }
            _ => unreachable!(),
        }
        fs::write(&lock_path, lock.to_string()).unwrap();
        let before = fs::read(&lock_path).unwrap();
        let installed = fs::read(project.path().join("node_modules/plugin/package.json")).unwrap();
        let failure = run(
            project.path(),
            None,
            Some(&cold),
            InstallMode::Frozen,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(
                failure.outcome.state,
                ChangeState::Unchanged | ChangeState::RolledBack
            ),
            "{fault}: {failure:?}"
        );
        assert!(failure.recovery_error.is_none(), "{fault}: {failure:?}");
        assert_eq!(fs::read(&lock_path).unwrap(), before, "{fault}");
        assert_eq!(
            fs::read(project.path().join("node_modules/plugin/package.json")).unwrap(),
            installed,
            "{fault}"
        );
        assert!(!cold.join("trees").exists(), "{fault}");
    }
}

#[test]
fn frozen_hydration_rejects_missing_private_credentials_before_network() {
    let (project, _fixture, cold) = pinned_fixture_project("frozen-private-auth");
    let lock_path = project.path().join("tapid.lock");
    let bytes = fs::read_to_string(&lock_path)
        .unwrap()
        .replace("https://registry.npmjs.org", "https://private.example");
    fs::write(&lock_path, &bytes).unwrap();
    project.write("tapid.toml", b"[registries.default]\nurl='https://private.example'\ntoken-env='TAPID_FROZEN_TEST_MISSING_TOKEN_193'\n").unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&cold),
        InstallMode::Frozen,
        None,
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::RegistryCredentialMissing);
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), bytes);
    assert!(!cold.join("trees").exists());
}

#[test]
fn offline_cold_store_never_uses_supplied_artifacts() {
    let (project, fixture, cold) = pinned_fixture_project("offline-cold");
    let failure = run(
        project.path(),
        None,
        Some(&cold),
        InstallMode::Offline,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::StoreUnavailable);
    assert!(!cold.join("trees").exists());
}

#[test]
fn frozen_hydration_rolls_back_publication_when_activation_fails() {
    let (project, fixture, cold) = pinned_fixture_project("frozen-activation-failure");
    let before = fs::read(project.path().join("tapid.lock")).unwrap();
    fs::remove_dir_all(project.path().join("node_modules")).unwrap();
    fs::remove_file(project.path().join(".tapid-managed")).unwrap();
    project
        .write("node_modules/KEEP", b"unmanaged tree")
        .unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&cold),
        InstallMode::Frozen,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Materialization);
    assert_eq!(failure.outcome.state, ChangeState::RolledBack);
    assert!(failure.recovery_error.is_none());
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
    assert_eq!(
        fs::read(project.path().join("node_modules/KEEP")).unwrap(),
        b"unmanaged tree"
    );
    assert!(!cold.join("trees").exists());
}

#[test]
fn adding_a_root_does_not_require_metadata_for_unchanged_locked_packages() {
    let (project, fixture, _) = pinned_fixture_project("locked-metadata-unavailable");
    let warm = project.path().join("warm-store");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    metadata["packages"][0]["name"] = "other".into();
    fs::write(&fixture, metadata.to_string()).unwrap();
    run(
        project.path(),
        Some("other@*"),
        Some(&warm),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
    assert_eq!(lock.packages().len(), 2);
    assert!(
        lock.packages_typed()
            .unwrap()
            .iter()
            .any(|(key, _)| key.name.as_str() == "plugin" && key.version.to_string() == "1.0.0")
    );
}

#[test]
fn online_compatibility_refetches_unverified_locks_and_preserves_verified_selections() {
    for mode in [InstallMode::Online, InstallMode::Refresh] {
        for changed in [false, true] {
            let (project, fixture) = project_with_fixture("unverified-existing-lock", INTEGRITY);
            project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*","verified":"*"}}"#).unwrap();
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
            let mut verified = metadata["packages"][0].clone();
            verified["name"] = "verified".into();
            metadata["packages"][0]
                .as_object_mut()
                .unwrap()
                .remove("integrity");
            metadata["packages"]
                .as_array_mut()
                .unwrap()
                .push(verified.clone());
            fs::write(&fixture, metadata.to_string()).unwrap();
            let store = project.path().join("store");
            run(
                project.path(),
                None,
                Some(&store),
                InstallMode::Online,
                Some(&fixture),
                true,
                |_| {},
            )
            .unwrap();
            let before = fs::read(project.path().join("tapid.lock")).unwrap();
            for strict_mode in [
                InstallMode::Online,
                InstallMode::Refresh,
                InstallMode::Offline,
                InstallMode::Frozen,
                InstallMode::Ci,
                InstallMode::CiOffline,
            ] {
                assert!(
                    run(
                        project.path(),
                        None,
                        Some(&store),
                        strict_mode,
                        Some(&fixture),
                        false,
                        |_| {}
                    )
                    .is_err()
                );
                assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
            }
            let mut newer = metadata["packages"][0].clone();
            newer["version"] = "2.0.0".into();
            newer["dependencies"] = serde_json::json!({"fresh":"1.0.0"});
            verified["version"] = "2.0.0".into();
            let mut fresh = verified.clone();
            fresh["name"] = "fresh".into();
            fresh["version"] = "1.0.0".into();
            metadata["packages"]
                .as_array_mut()
                .unwrap()
                .extend([newer, verified, fresh]);
            fs::write(&fixture, metadata.to_string()).unwrap();
            if changed {
                project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*","verified":"*","fresh":"1.0.0"}}"#).unwrap();
            }
            let report = run(
                project.path(),
                None,
                Some(&store),
                mode,
                Some(&fixture),
                true,
                |_| {},
            )
            .unwrap();
            assert!(
                report
                    .outcome
                    .warnings
                    .iter()
                    .any(|warning| matches!(warning, Warning::UnverifiedRegistryArtifactsAllowed))
            );
            let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
            let plugin: tapid_lockfile::LockfilePackageKey =
                lock.root_bindings()["plugin"].parse().unwrap();
            assert_eq!(plugin.version.to_string(), "2.0.0");
            let verified: tapid_lockfile::LockfilePackageKey =
                lock.root_bindings()["verified"].parse().unwrap();
            assert_eq!(
                verified.version.to_string(),
                if matches!(mode, InstallMode::Online) {
                    "1.0.0"
                } else {
                    "2.0.0"
                }
            );
            let plugin = lock
                .packages_typed()
                .unwrap()
                .into_iter()
                .find(|(key, _)| key.name.as_str() == "plugin")
                .unwrap()
                .1;
            assert_eq!(plugin.registry_integrity_declared(), Some(false));
            assert!(plugin.dependencies().contains_key("fresh"));
            assert!(lock.validate_replay(lock.root_manifest_digest()).is_err());
        }
    }
}

#[test]
fn compatibility_checks_fresh_integrity_before_promoting_an_existing_unverified_artifact() {
    for tampered in [false, true] {
        let (project, fixture) = project_with_fixture("unverified-promotion", INTEGRITY);
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#,
            )
            .unwrap();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
        metadata["packages"][0]
            .as_object_mut()
            .unwrap()
            .remove("integrity");
        fs::write(&fixture, metadata.to_string()).unwrap();
        let store = project.path().join("store");
        run(
            project.path(),
            None,
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            true,
            |_| {},
        )
        .unwrap();
        let before = fs::read(project.path().join("tapid.lock")).unwrap();
        project.write("node_modules/KEEP", b"previous").unwrap();
        metadata["packages"][0]["integrity"] = INTEGRITY.into();
        if tampered {
            metadata["packages"][0]["artifact"] = "base64:AA==".into();
        }
        fs::write(&fixture, metadata.to_string()).unwrap();
        let result = run(
            project.path(),
            None,
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            true,
            |_| {},
        );
        if tampered {
            assert_eq!(result.unwrap_err().error.kind, ErrorKind::Integrity);
            assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
            assert_eq!(
                fs::read(project.path().join("node_modules/KEEP")).unwrap(),
                b"previous"
            );
        } else {
            result.unwrap();
            let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
            assert_eq!(
                lock.packages_typed().unwrap()[0]
                    .1
                    .registry_integrity_declared(),
                Some(true)
            );
            lock.validate_replay(lock.root_manifest_digest()).unwrap();
        }
    }
}

#[test]
fn fetching_another_version_preserves_locked_dependency_edges() {
    let (project, fixture) = project_with_fixture("locked-packument-refresh", INTEGRITY);
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#,
        )
        .unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    let base = metadata["packages"][0].clone();
    metadata["packages"][0]["dependencies"] = serde_json::json!({"dep":"1.0.0"});
    let mut dependency = base.clone();
    dependency["name"] = "dep".into();
    metadata["packages"]
        .as_array_mut()
        .unwrap()
        .push(dependency.clone());
    fs::write(&fixture, metadata.to_string()).unwrap();
    let store = project.path().join("store");
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let before = read_lock(&project.path().join("tapid.lock")).unwrap();
    let pinned_key = before.root_bindings()["plugin"].clone();
    let pinned = before
        .packages_typed()
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.to_string() == pinned_key)
        .unwrap()
        .1;
    assert_eq!(pinned.dependencies().len(), 1);

    let mut next = base;
    next["version"] = "2.0.0".into();
    metadata["packages"].as_array_mut().unwrap().push(next);
    metadata["packages"][0]["dependencies"] = serde_json::json!({"dep":"2.0.0"});
    dependency["version"] = "2.0.0".into();
    metadata["packages"]
        .as_array_mut()
        .unwrap()
        .push(dependency);
    fs::write(&fixture, metadata.to_string()).unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"1.0.0","plugin-next":"npm:plugin@2.0.0"}}"#).unwrap();
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let after = read_lock(&project.path().join("tapid.lock")).unwrap();
    assert_eq!(after.root_bindings()["plugin"], pinned_key);
    let preserved = after
        .packages_typed()
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.to_string() == pinned_key)
        .unwrap()
        .1;
    assert_eq!(preserved, pinned);
    let next: tapid_lockfile::LockfilePackageKey =
        after.root_bindings()["plugin-next"].parse().unwrap();
    assert_eq!(next.version.to_string(), "2.0.0");
}

#[test]
fn unrelated_root_changes_preserve_direct_and_transitive_versions_of_one_package() {
    let (project, fixture) = project_with_fixture("locked-multiple-versions", INTEGRITY);
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*","parent":"1.0.0"}}"#,
        )
        .unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    let mut parent = metadata["packages"][0].clone();
    parent["name"] = "parent".into();
    parent["dependencies"] = serde_json::json!({"plugin":"2.0.0"});
    // Initially the direct root requires 1.x, while parent requires 2.x.
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"^1","parent":"1.0.0"}}"#,
        )
        .unwrap();
    let mut higher = metadata["packages"][0].clone();
    higher["version"] = "2.0.0".into();
    metadata["packages"]
        .as_array_mut()
        .unwrap()
        .extend([parent, higher]);
    fs::write(&fixture, metadata.to_string()).unwrap();
    let store = project.path().join("store");
    run(
        project.path(),
        None,
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    // Broadening a requirement does not require changing its valid selection.
    run(
        project.path(),
        Some("plugin@*"),
        Some(&store),
        InstallMode::Online,
        Some(&fixture),
        false,
        |_| {},
    )
    .unwrap();
    let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
    let key: tapid_lockfile::LockfilePackageKey = lock.root_bindings()["plugin"].parse().unwrap();
    assert_eq!(key.version.to_string(), "1.0.0");
    assert!(
        lock.packages_typed()
            .unwrap()
            .iter()
            .any(|(key, _)| key.name.as_str() == "plugin" && key.version.to_string() == "2.0.0")
    );
}

#[test]
fn frozen_rejects_invalid_root_bindings_before_downloading() {
    let (project, _fixture, cold) = pinned_fixture_project("frozen-invalid-binding");
    let lock_path = project.path().join("tapid.lock");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    let target = lock["rootBindings"]["plugin"].clone();
    lock["rootBindings"] = serde_json::json!({"unknown": target});
    fs::write(&lock_path, lock.to_string()).unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&cold),
        InstallMode::Frozen,
        Some(&project.path().join("absent-fixture.json")),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Lockfile);
    assert!(
        failure
            .error
            .to_string()
            .contains("not a direct manifest dependency")
    );
    assert!(!cold.join("trees").exists());
}

#[test]
fn changed_peer_roots_rebind_compatible_ranges_and_release_incompatible_pins() {
    for (provider, plugin_version) in [("18.3.0", "1.0.0"), ("19.0.0", "2.0.0")] {
        let (project, fixture) = project_with_fixture("changed-locked-peers", INTEGRITY);
        project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*","react":"18.2.0"}}"#).unwrap();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
        let base = metadata["packages"][0].clone();
        metadata["packages"][0]["peerDependencies"] = serde_json::json!({"react":"^18"});
        let mut plugin2 = metadata["packages"][0].clone();
        plugin2["version"] = "2.0.0".into();
        plugin2["peerDependencies"] = serde_json::json!({"react":"^19"});
        metadata["packages"].as_array_mut().unwrap().push(plugin2);
        for version in ["18.2.0", "18.3.0", "19.0.0"] {
            let mut react = base.clone();
            react["name"] = "react".into();
            react["version"] = version.into();
            metadata["packages"].as_array_mut().unwrap().push(react);
        }
        // Select v1 initially; v2 is introduced after the first lock is written.
        let newer = metadata["packages"].as_array_mut().unwrap().remove(1);
        fs::write(&fixture, metadata.to_string()).unwrap();
        let store = project.path().join("store");
        run(
            project.path(),
            None,
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap();
        metadata["packages"].as_array_mut().unwrap().push(newer);
        fs::write(&fixture, metadata.to_string()).unwrap();
        run(
            project.path(),
            Some(&format!("react@{provider}")),
            Some(&store),
            InstallMode::Online,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap();
        let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
        let key: tapid_lockfile::LockfilePackageKey =
            lock.root_bindings()["plugin"].parse().unwrap();
        assert_eq!(key.version.to_string(), plugin_version);
        assert_eq!(
            crate::context::parse_peer(&key.peer_context)
                .unwrap()
                .entries()[&"react".parse().unwrap()]
                .to_string(),
            provider
        );
    }
}

#[test]
fn frozen_refuses_corrupt_warm_content_without_redownloading() {
    let (project, _fixture, _) = pinned_fixture_project("frozen-corrupt-warm");
    let warm = project.path().join("warm-store");
    let lock_path = project.path().join("tapid.lock");
    let before = fs::read(&lock_path).unwrap();
    let lock = read_lock(&lock_path).unwrap();
    let digest = lock.packages().values().next().unwrap().tree_digest();
    fs::write(
        warm.join("trees").join(digest).join("package.json"),
        b"tampered",
    )
    .unwrap();
    let failure = run(
        project.path(),
        None,
        Some(&warm),
        InstallMode::Frozen,
        Some(&project.path().join("absent-fixture.json")),
        false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Integrity);
    assert_eq!(fs::read(&lock_path).unwrap(), before);
}

#[test]
fn dependency_discovery_failure_warns_without_package_approval_and_preserves_replay() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha512};
    let project = TempProject::new("lifecycle-discovery-failure").unwrap();
    let home = tapid_test_support::TempHome::new("lifecycle-discovery-failure").unwrap();
    let store = home.path().join("store");
    let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    // Valid package metadata larger than the bounded lifecycle discovery reader.
    let archive = "H4sIAAAAAAAA/+3QQUsCQRQHcD/KMmex3TIPHjoVFEQeNOgmy7rYlq2LqxGI372hhSDolkiH3w+GP7zHzHtMkxev+bI8a7ocvLTrundkaTQaDr8y+pHDeLLL715Xz7LRxXkvSY+9yG927TbfxPGnmPUP7UOdv5VhHJrVblnVoR/ey01bretYygbpII2VtthUzbYN432o6vhdq1VslsXzOinbIm/KRXKVTG8nj/fX84fJbH7zdDedhUM/LMruZvfaBwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADAH4VDDwAAAACAk/kE90MsPAAYEAA=";
    let bytes = STANDARD.decode(archive).unwrap();
    let integrity = format!("sha512-{}", STANDARD.encode(Sha512::digest(&bytes)));
    let fixture = project
        .write(
            "registry.json",
            serde_json::json!({"packages":[{
                "registry":"https://registry.npmjs.org", "name":"plugin", "version":"1.0.0",
                "integrity":integrity, "artifact":format!("base64:{archive}")
            }]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let policy = |package: &str| {
        format!(
            r#"schema = 1
[[approvals]]
package = "{package}"
version = "2.0.0"
archive-digest = "{integrity}"
hook = "install"
script-digest = "sha256-{}"
system-toolchain = true
read = ["."]
write = ["."]
network = false
environment = {{}}
timeout-seconds = 5
max-output-bytes = 1024
max-processes = 32
max-memory-bytes = 134217728
tools = [{{name="sh", path={}, digest="sha256-{}"}}]
"#,
            "0".repeat(64),
            serde_json::to_string(&project.path().join("unused-sh").to_str().unwrap()).unwrap(),
            "0".repeat(64)
        )
    };
    for document in ["schema = 1".to_owned(), policy("unrelated")] {
        project
            .write("tapid.lifecycle.toml", document.as_bytes())
            .unwrap();
        for mode in [InstallMode::Online, InstallMode::Frozen] {
            let report = run(
                project.path(),
                None,
                Some(&store),
                mode,
                Some(&fixture),
                false,
                |_| {},
            )
            .unwrap();
            assert_eq!(
                report.outcome.warnings,
                vec![Warning::DependencyLifecycleDiscoveryFailed {
                    package: "plugin@1.0.0".into(),
                }]
            );
            assert!(
                report.outcome.warnings[0]
                    .to_string()
                    .contains("could not discover dependency lifecycle hooks for plugin@1.0.0")
            );
            assert!(
                !project
                    .path()
                    .join("node_modules/plugin/SHOULD_NOT_EXIST")
                    .exists()
            );
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                manifest
            );
        }
    }
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    let installed = fs::read(project.path().join("node_modules/plugin/package.json")).unwrap();
    // Even an approval for a different version of this package must fail closed.
    project
        .write("tapid.lifecycle.toml", policy("plugin").as_bytes())
        .unwrap();
    for mode in [InstallMode::Online, InstallMode::Frozen] {
        let failure = run(
            project.path(),
            None,
            Some(&store),
            mode,
            Some(&fixture),
            false,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.error.kind, ErrorKind::InvalidRequest);
        assert!(
            failure
                .error
                .to_string()
                .contains("dependency package.json must be a regular file no larger than 1 MiB")
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
        assert_eq!(
            fs::read(project.path().join("node_modules/plugin/package.json")).unwrap(),
            installed
        );
    }
}
