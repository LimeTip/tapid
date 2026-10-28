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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Frozen,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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

#[test]
fn dependency_edit_evaluation_observes_one_real_install_without_leaking_counts() {
    use crate::application::dependency_edit_evaluation::measure;
    let (project, fixture) = project_with_fixture("edit-evaluation-counts", INTEGRITY);
    let original =
        PackageManifest::parse(&fs::read_to_string(project.path().join("package.json")).unwrap())
            .unwrap();
    let updated = original.clone().with_dependency("plugin", "1.0.0").unwrap();
    let (_, counts) = measure(|| {
        run(
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::ReplaceManifest(ManifestEdit {
                    expected: &original,
                    updated: &updated,
                }),
                store_root: Some(&project.path().join("store")),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
            |_| {},
        )
        .unwrap()
    });
    assert_eq!(counts.graph_operations, 1);
    assert_eq!(counts.metadata_retrievals, 1);
    assert_eq!(counts.artifact_requests, 1);
    assert_eq!(counts.materializations, 1);
    assert_eq!(counts.activations, 1);
    assert_eq!(counts.durable_decisions, 1);
    assert_eq!(measure(|| ()).1, Default::default());
}

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
fn batch_install_rolls_back_resolution_integrity_and_staging_failures() {
    use crate::application::lifecycle::batch::{self, Edit};
    use tapid_manifest::DependencyKind as Kind;
    for failure_kind in [
        ErrorKind::Resolution,
        ErrorKind::Integrity,
        ErrorKind::Materialization,
    ] {
        let (project, fixture) = project_with_fixture("batch-install-failure", INTEGRITY);
        let store = project.path().join("store");
        let initial = run(
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(initial.outcome.state, ChangeState::Committed);
        let manifest = fs::read(project.path().join("package.json")).unwrap();
        let lock = fs::read(project.path().join("tapid.lock")).unwrap();
        let before = Lockfile::from_json(std::str::from_utf8(&lock).unwrap()).unwrap();
        let digest = before.packages().values().next().unwrap().tree_digest();
        let digest = digest.parse().unwrap();
        let verified_store = tapid_store::Store::new(&store);
        let snapshot = verified_store.verified_tree_snapshot(&digest).unwrap();
        let installed = fs::read(project.path().join("node_modules/plugin/package.json")).unwrap();
        let second_spec = if failure_kind == ErrorKind::Resolution {
            "missing@1.0.0"
        } else {
            "new@npm:plugin@2.0.0"
        };
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fixture).unwrap()).unwrap();
        let mut second = metadata["packages"][0].clone();
        second["version"] = "2.0.0".into();
        if failure_kind == ErrorKind::Integrity {
            second["integrity"] = format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode([0; 64])
            )
            .into();
        }
        metadata["packages"].as_array_mut().unwrap().push(second);
        fs::write(&fixture, metadata.to_string()).unwrap();
        let _fault = (failure_kind == ErrorKind::Materialization).then(|| inject(Fault::Stage));
        let failure = batch::run(
            project.path(),
            None,
            &store,
            &fixture,
            &[
                Edit::Move {
                    name: "plugin",
                    from: Kind::Dependencies,
                    to: Kind::DevDependencies,
                },
                Edit::Add {
                    section: Kind::Dependencies,
                    spec: second_spec,
                },
            ],
        )
        .unwrap_err();
        assert_eq!(failure.error.kind, failure_kind);
        assert_eq!(failure.outcome.state, ChangeState::RolledBack);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
        assert_eq!(
            fs::read(project.path().join("node_modules/plugin/package.json")).unwrap(),
            installed
        );
        assert!(verified_store.verified_tree_snapshot(&digest).is_ok());
        drop(snapshot);
    }
}

#[test]
fn batch_postcommit_failures_retain_the_combined_state() {
    use crate::application::lifecycle::batch::{self, Edit};
    use tapid_manifest::DependencyKind as Kind;
    for fault in [Fault::AfterCommit, Fault::CleanupPending] {
        let (project, fixture) = project_with_fixture("batch-postcommit", INTEGRITY);
        let edits = [
            Edit::Add {
                section: Kind::Dependencies,
                spec: "runtime@npm:plugin@1.0.0",
            },
            Edit::Add {
                section: Kind::DevDependencies,
                spec: "dev@npm:plugin@1.0.0",
            },
        ];
        let _fault = inject(fault);
        let failure = batch::run(
            project.path(),
            None,
            &project.path().join("store"),
            &fixture,
            &edits,
        )
        .unwrap_err();
        assert!(matches!(
            failure.outcome.state,
            ChangeState::Committed | ChangeState::CommittedCleanupPending
        ));
        let manifest = PackageManifest::parse(
            &fs::read_to_string(project.path().join("package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.dependencies()["runtime"], "npm:plugin@1.0.0");
        assert_eq!(manifest.dev_dependencies()["dev"], "npm:plugin@1.0.0");
        assert!(project.path().join("tapid.lock").is_file());
        assert!(
            project
                .path()
                .join("node_modules/runtime/package.json")
                .is_file()
        );
        assert!(
            project
                .path()
                .join("node_modules/dev/package.json")
                .is_file()
        );
    }
}

#[test]
fn batch_candidate_retains_installer_project_change_detection() {
    use crate::application::lifecycle::{
        self,
        batch::{self, Edit},
    };
    use tapid_manifest::DependencyKind as Kind;
    let (project, fixture) = project_with_fixture("batch-project-changed", INTEGRITY);
    let selection = lifecycle::resolve_workspace(project.path(), None).unwrap();
    let updated = batch::plan(
        &selection,
        &[Edit::Add {
            section: Kind::DevDependencies,
            spec: "plugin@1.0.0",
        }],
    )
    .unwrap();
    let changed = br#"{"name":"app","version":"1.0.0","scripts":{"test":"other edit"}}"#;
    project.write("package.json", changed).unwrap();
    let failure = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::ReplaceManifest(ManifestEdit {
                expected: &selection.manifest,
                updated: &updated,
            }),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::ProjectChanged);
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        changed
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn batch_install_keeps_optional_and_peer_root_semantics() {
    use crate::application::lifecycle::batch::{self, Edit};
    use tapid_manifest::DependencyKind as Kind;
    let (project, fixture) = project_with_fixture("batch-optional-peer", INTEGRITY);
    let report = batch::run(
        project.path(),
        None,
        &project.path().join("store"),
        &fixture,
        &[
            Edit::Add {
                section: Kind::OptionalDependencies,
                spec: "plugin@1.0.0",
            },
            Edit::Add {
                section: Kind::PeerDependencies,
                spec: "peer-only@^1",
            },
        ],
    )
    .unwrap();
    assert_eq!(report.outcome.state, ChangeState::Committed);
    assert_eq!(report.package_count, 1);
    assert!(
        project
            .path()
            .join("node_modules/plugin/package.json")
            .is_file()
    );
    assert!(!project.path().join("node_modules/peer-only").exists());
}

#[test]
fn batch_recovers_existing_transaction_crash_points() {
    use crate::application::lifecycle::batch::{self, Edit};
    use tapid_manifest::DependencyKind as Kind;
    let edits = [
        Edit::Move {
            name: "plugin",
            from: Kind::Dependencies,
            to: Kind::DevDependencies,
        },
        Edit::Add {
            section: Kind::Dependencies,
            spec: "h3@2.0.0",
        },
    ];
    // Re-execute only this test to abort the real private batch path without
    // process-global environment changes in the parent test runner.
    if let Some(project) = std::env::var_os("TAPID_BATCH_CRASH_PROJECT") {
        let project = PathBuf::from(project);
        assert_eq!(
            fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
            fs::canonicalize(&project).unwrap(),
            "crash artifacts must stay in the isolated temporary project"
        );
        batch::run(
            &project,
            None,
            &project.join("store"),
            &project.join("registry.json"),
            &edits,
        )
        .unwrap();
        panic!("requested crash checkpoint was not reached");
    }
    for point in [
        "store_published",
        "lockfile_replaced",
        "node_modules_backed_up",
        "activation_complete",
        "commit_decision",
    ] {
        let (project, fixture) = project_with_fixture("batch-crash-recovery", INTEGRITY);
        let home = tapid_test_support::TempHome::new("batch-crash-recovery").unwrap();
        let store = project.path().join("store");
        run(
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
            |_| {},
        )
        .unwrap();
        let manifest = fs::read(project.path().join("package.json")).unwrap();
        let lock = fs::read(project.path().join("tapid.lock")).unwrap();
        let installed = fs::read(project.path().join("node_modules/plugin/package.json")).unwrap();
        let trees = || {
            fs::read_dir(store.join("trees"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let prior_trees = trees();
        let aliases: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/npm-aliases.json")).unwrap();
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fixture).unwrap()).unwrap();
        metadata["packages"]
            .as_array_mut()
            .unwrap()
            .push(aliases["packages"][1].clone());
        fs::write(&fixture, metadata.to_string()).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "application::install::outcome_tests::batch_recovers_existing_transaction_crash_points", "--nocapture"])
            // Abort may write a core dump on Linux. Keep it in the fixture,
            // where TempProject removes it along with the interrupted state.
            .current_dir(project.path())
            .env_clear()
            .env("HOME", home.path()).env("USERPROFILE", home.path())
            .env("XDG_CACHE_HOME", home.path()).env("LOCALAPPDATA", home.path())
            .env("TAPID_BATCH_CRASH_PROJECT", project.path())
            .env("TAPID_TEST_CRASH_POINT", point)
            .output().unwrap();
        assert!(
            !output.status.success(),
            "{point}: child unexpectedly completed"
        );
        assert!(
            project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .is_file(),
            "{point}: crash did not leave a journal; {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let recovery = ActivationLock::acquire(project.path()).unwrap();
        assert!(recovery.recovered, "{point}");
        drop(recovery);
        if point == "commit_decision" {
            let current = PackageManifest::parse(
                &fs::read_to_string(project.path().join("package.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(current.dev_dependencies()["plugin"], "1.0.0");
            assert_eq!(current.dependencies()["h3"], "2.0.0");
            assert!(
                project
                    .path()
                    .join("node_modules/h3/package.json")
                    .is_file()
            );
            assert_ne!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
            assert!(trees().is_superset(&prior_trees));
            assert!(trees().len() > prior_trees.len());
        } else {
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                manifest,
                "{point}"
            );
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                lock,
                "{point}"
            );
            assert_eq!(
                fs::read(project.path().join("node_modules/plugin/package.json")).unwrap(),
                installed,
                "{point}"
            );
            assert!(!project.path().join("node_modules/h3").exists());
            assert_eq!(
                trees(),
                prior_trees,
                "{point}: newly published objects must roll back"
            );
        }
        assert!(
            !project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}

#[test]
fn install_reclassifies_the_lock_after_recovery() {
    use crate::filesystem::lifecycle_journal::LifecycleJournal;
    use tapid_lockfile::ImportedNpmLockfile;

    for (restore_imported, ci, overrides) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, false, true),
        (true, false, true),
    ] {
        let (project, fixture) = project_with_fixture("recovered-lock-format", INTEGRITY);
        let manifest_path = project.path().join("package.json");
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        let digest = crate::online::root_digest(project.path()).unwrap();
        let native = Lockfile::new(&digest).unwrap().to_json().unwrap();
        let imported = ImportedNpmLockfile::import(
            r#"{"lockfileVersion":3,"packages":{"":{"name":"app","version":"1.0.0"}}}"#,
            &manifest,
            &digest,
        )
        .unwrap()
        .to_json()
        .unwrap();
        let (restored, interrupted) = if restore_imported {
            (&imported, &native)
        } else {
            (&native, &imported)
        };
        let activation = ActivationLock::acquire(project.path()).unwrap();
        let journal = LifecycleJournal::begin(
            project.path(),
            &manifest_path,
            activation.owner_line(),
            manifest.as_bytes(),
            Some(restored.as_bytes()),
        )
        .unwrap();
        project.write("tapid.lock", interrupted.as_bytes()).unwrap();
        std::mem::forget(journal);
        drop(activation);

        let result = run_with_execution(
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&project.path().join("store")),
                mode: if overrides {
                    InstallMode::Online
                } else if ci {
                    InstallMode::Ci
                } else {
                    InstallMode::Frozen
                },
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
            super::super::dependency_scripts::ExecutionOverrides {
                without_containment: overrides,
                ..Default::default()
            },
            |_| {},
        );
        let outcome = if restore_imported && (ci || overrides) {
            let failure = result
                .expect_err("CI and execution overrides must reject the recovered imported lock");
            assert_eq!(
                failure.error.kind,
                if overrides {
                    ErrorKind::InvalidRequest
                } else {
                    ErrorKind::Lockfile
                }
            );
            assert!(!project.path().join("node_modules").exists());
            *failure.outcome
        } else {
            let report = result.unwrap_or_else(|error| {
                panic!("restore_imported={restore_imported}, ci={ci}: {error:?}")
            });
            assert_eq!(report.package_count, 0);
            report.outcome
        };
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| matches!(w, Warning::PreviousTransactionRecovered))
        );
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            *restored
        );
        assert_eq!(fs::read_to_string(&manifest_path).unwrap(), manifest);
        assert!(
            !project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}

#[test]
fn failed_resolution_reports_rollback_and_preserves_its_category() {
    let (project, fixture) = project_with_fixture("typed-resolution", INTEGRITY);
    let original = fs::read(project.path().join("package.json")).unwrap();
    let failure = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("missing@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
    Stage,
    AfterCommit,
    CleanupPending,
    RollbackPending,
    PolicyChanged,
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
        (Some(Fault::Stage), "before_stage") => Err(OperationalError::new(
            ErrorKind::Materialization,
            "injected staging failure",
        )),
        (Some(Fault::PolicyChanged), "before_acquire") => {
            fs::write(project.join("tapid.lifecycle.toml"), b"schema = 2").unwrap();
            Ok(())
        }
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
fn install_reads_lifecycle_policy_after_acquiring_the_project_lock() {
    let (project, fixture) = project_with_fixture("policy-before-acquire", INTEGRITY);
    project
        .write("tapid.lifecycle.toml", b"schema = 1")
        .unwrap();
    let _fault = inject(Fault::PolicyChanged);
    let result = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    );
    let failure = result
        .expect_err("must reject the invalid current policy rather than use the earlier valid one");
    assert_eq!(failure.error.kind, ErrorKind::InvalidRequest);
    assert!(!project.path().join("node_modules").exists());
    assert!(!project.path().join("tapid.lock").exists());
}

#[test]
fn failure_after_commit_keeps_the_committed_manifest_lock_and_packages() {
    let (project, fixture) = project_with_fixture("typed-post-commit", INTEGRITY);
    let _fault = inject(Fault::AfterCommit);
    let failure = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap();
    let mut manifest = fs::read(project.path().join("package.json")).unwrap();
    manifest.push(b' ');
    project.write("package.json", &manifest).unwrap();
    let failure = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Frozen,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Frozen,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap();
    fs::remove_dir_all(store.join("trees")).unwrap();
    let failure = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Offline,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&project.path().join("store")),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: true,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Frozen,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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
                InstallRequest {
                    project_dir: project.path(),
                    target_manifest_path: Path::new("package.json"),
                    mutation: ManifestMutation::None,
                    store_root: Some(&project.path().join("store")),
                    mode: InstallMode::Online,
                    registry_fixture: None,
                    allow_unverified_registry_artifacts: false,
                },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
                store_root: Some(&project.path().join("store")),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::AddPackage("plugin@1.0.0"),
                store_root: Some(&project.path().join("store")),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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
                InstallRequest {
                    project_dir: project.path(),
                    target_manifest_path: Path::new("package.json"),
                    mutation: ManifestMutation::None,
                    store_root: Some(&store),
                    mode,
                    registry_fixture: Some(&fixture),
                    allow_unverified_registry_artifacts: false,
                },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        original_lock
    );
    fs::remove_file(&fixture).unwrap();
    let replay = run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&cold),
            mode: InstallMode::Frozen,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(fs::read(&lock_path).unwrap(), original_lock);
    run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&cold),
            mode: InstallMode::Offline,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("other@*"),
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Refresh,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&warm),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&cold),
                mode: InstallMode::Frozen,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&cold),
            mode: InstallMode::Frozen,
            registry_fixture: None,
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&cold),
            mode: InstallMode::Offline,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&cold),
            mode: InstallMode::Frozen,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("other@*"),
            store_root: Some(&warm),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
                InstallRequest {
                    project_dir: project.path(),
                    target_manifest_path: Path::new("package.json"),
                    mutation: ManifestMutation::None,
                    store_root: Some(&store),
                    mode: InstallMode::Online,
                    registry_fixture: Some(&fixture),
                    allow_unverified_registry_artifacts: true,
                },
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
                        InstallRequest {
                            project_dir: project.path(),
                            target_manifest_path: Path::new("package.json"),
                            mutation: ManifestMutation::None,
                            store_root: Some(&store),
                            mode: strict_mode,
                            registry_fixture: Some(&fixture),
                            allow_unverified_registry_artifacts: false
                        },
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
                InstallRequest {
                    project_dir: project.path(),
                    target_manifest_path: Path::new("package.json"),
                    mutation: ManifestMutation::None,
                    store_root: Some(&store),
                    mode,
                    registry_fixture: Some(&fixture),
                    allow_unverified_registry_artifacts: true,
                },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: true,
            },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: true,
            },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
        |_| {},
    )
    .unwrap();
    // Broadening a requirement does not require changing its valid selection.
    run(
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::AddPackage("plugin@*"),
            store_root: Some(&store),
            mode: InstallMode::Online,
            registry_fixture: Some(&fixture),
            allow_unverified_registry_artifacts: false,
        },
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&cold),
            mode: InstallMode::Frozen,
            registry_fixture: Some(&project.path().join("absent-fixture.json")),
            allow_unverified_registry_artifacts: false,
        },
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
            |_| {},
        )
        .unwrap();
        metadata["packages"].as_array_mut().unwrap().push(newer);
        fs::write(&fixture, metadata.to_string()).unwrap();
        run(
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::AddPackage(&format!("react@{provider}")),
                store_root: Some(&store),
                mode: InstallMode::Online,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
            |_| {},
        )
        .unwrap();
        let lock = read_lock(&project.path().join("tapid.lock")).unwrap();
        let key: tapid_lockfile::LockfilePackageKey =
            lock.root_bindings()["plugin"].parse().unwrap();
        assert_eq!(key.version.to_string(), plugin_version);
        assert_eq!(
            key.peer_context().unwrap().entries()[&"react".parse().unwrap()].to_string(),
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
        InstallRequest {
            project_dir: project.path(),
            target_manifest_path: Path::new("package.json"),
            mutation: ManifestMutation::None,
            store_root: Some(&warm),
            mode: InstallMode::Frozen,
            registry_fixture: Some(&project.path().join("absent-fixture.json")),
            allow_unverified_registry_artifacts: false,
        },
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
                InstallRequest {
                    project_dir: project.path(),
                    target_manifest_path: Path::new("package.json"),
                    mutation: ManifestMutation::None,
                    store_root: Some(&store),
                    mode,
                    registry_fixture: Some(&fixture),
                    allow_unverified_registry_artifacts: false,
                },
                |_| {},
            )
            .unwrap();
            assert_eq!(report.outcome.warnings.len(), 1);
            let Warning::DependencyLifecycleDiscoveryFailed { package } =
                &report.outcome.warnings[0]
            else {
                panic!("expected discovery failure")
            };
            assert_eq!(package.key.name.to_string(), "plugin");
            assert_eq!(package.key.version.to_string(), "1.0.0");
            let lifecycle = report.outcome.lifecycle.as_ref().unwrap();
            assert_eq!(lifecycle.coverage, "incomplete");
            assert_eq!(lifecycle.packages[0].discovery, "failed");
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
            InstallRequest {
                project_dir: project.path(),
                target_manifest_path: Path::new("package.json"),
                mutation: ManifestMutation::None,
                store_root: Some(&store),
                mode,
                registry_fixture: Some(&fixture),
                allow_unverified_registry_artifacts: false,
            },
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

#[test]
fn dependency_edit_graph_evidence_is_bounded_at_exact_record_limit() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    let digest = format!("sha256-{}", "0".repeat(64));
    let mut lock = Lockfile::new(&digest).unwrap();
    for index in 0..=MAX_GRAPH_CHANGES {
        if index == MAX_GRAPH_CHANGES {
            let exact = PackageChanges::between(None, &lock).unwrap();
            assert_eq!(exact.records.len(), 256);
            assert_eq!(exact.omitted, 0);
        }
        let package = LockedPackage::new_with_provenance(
            "https://registry.npmjs.org",
            &format!("package{index}"),
            "1.0.0",
            INTEGRITY,
            &digest,
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        lock.insert_package(package).unwrap();
    }
    lock.set_roots(lock.packages().keys().cloned().collect::<Vec<_>>())
        .unwrap();
    let extra = PackageChanges::between(None, &lock).unwrap();
    assert_eq!(extra.added, 257);
    assert_eq!(extra.records.len(), 256);
    assert_eq!(extra.omitted, 1);
    let unchanged =
        PackageChanges::between(Some(lock.to_json().unwrap().as_bytes()), &lock).unwrap();
    assert!(unchanged.records.is_empty());
    assert_eq!(unchanged.omitted, 0);
    let removed = PackageChanges::between(
        Some(lock.to_json().unwrap().as_bytes()),
        &Lockfile::new(&digest).unwrap(),
    )
    .unwrap();
    assert_eq!(removed.removed, 257);
    assert_eq!(removed.omitted, 1);
}

#[test]
fn dependency_edit_graph_preserves_origin_peer_platform_and_root_only_changes() {
    use tapid_core::{PeerContext, PlatformContext};
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    let digest = format!("sha256-{}", "0".repeat(64));
    let mut lock = Lockfile::new(&digest).unwrap();
    let peer = PeerContext::default().with("react".parse().unwrap(), "18.2.0".parse().unwrap());
    let plain = PeerContext::default();
    let any = PlatformContext::new(None, None, None).unwrap();
    let linux = PlatformContext::new(Some("linux"), Some("x86_64"), None).unwrap();
    for (origin, peers, platform) in [
        ("https://registry.npmjs.org", &plain, &any),
        ("https://other.example.test", &plain, &any),
        ("https://registry.npmjs.org", &peer, &any),
        ("https://registry.npmjs.org", &plain, &linux),
    ] {
        lock.insert_package(
            LockedPackage::new_with_context_and_provenance(
                origin,
                "same",
                "1.0.0",
                INTEGRITY,
                &digest,
                (peers, platform),
                RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap(),
        )
        .unwrap();
    }
    let changes = PackageChanges::between(None, &lock).unwrap();
    let identities = changes
        .records
        .iter()
        .map(|record| match record {
            GraphChange::Package { identity, .. } => identity.clone(),
            _ => panic!("unexpected record"),
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(identities.len(), 4);
    assert_eq!(identities, lock.packages().keys().cloned().collect());
    lock.set_roots(lock.packages().keys().cloned().collect::<Vec<_>>())
        .unwrap();
    let before = lock.to_json().unwrap();
    let bindings = lock
        .packages()
        .keys()
        .enumerate()
        .map(|(index, key)| (format!("alias{index}"), key.clone()))
        .collect();
    lock.set_root_bindings(bindings).unwrap();
    let roots = PackageChanges::between(Some(before.as_bytes()), &lock).unwrap();
    assert_eq!(roots.reused, 4);
    assert_eq!(roots.records.len(), 4);
    assert!(roots.records.iter().all(|record| matches!(
        record,
        GraphChange::Root {
            before: None,
            after: Some(_),
            ..
        }
    )));
}
