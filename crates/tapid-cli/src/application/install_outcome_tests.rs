use super::*;
use crate::application::outcome::{ChangeState, ErrorKind, RetryAdvice};
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
        InstallMode::Frozen,
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
    project.write("tapid.lock", b"prior lock bytes").unwrap();
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
        b"prior lock bytes"
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
            crate::application::lifecycle::outdated_report(project.path(), None, None, false)
                .unwrap_err()
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
