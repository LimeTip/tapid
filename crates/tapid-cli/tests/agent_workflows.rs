//! Deterministic command sequences, not an external-model benchmark.
use serde_json::Value;
use std::{
    fs,
    process::{Command, Stdio},
};
use tapid_test_support::{TempHome, TempProject};

#[derive(Default)]
struct Measurements {
    calls: usize,
    bytes: usize,
    retries: usize,
    previous_failure: Option<Vec<String>>,
}

impl Measurements {
    /// Run one isolated CLI call and record its protocol, byte, and retry measurements.
    fn call(&mut self, project: &TempProject, home: &TempHome, args: &[&str]) -> Value {
        if self
            .previous_failure
            .as_ref()
            .is_some_and(|previous| previous.iter().map(String::as_str).eq(args.iter().copied()))
        {
            self.retries += 1;
        }
        let output = command(project, home, args).output().unwrap();
        self.calls += 1;
        self.bytes += output.stdout.len() + output.stderr.len();
        assert!(output.stderr.is_empty(), "{output:?}");
        assert!(output.stdout.ends_with(b"\n"));
        let result: Value = serde_json::from_slice(&output.stdout).expect("single JSON object");
        self.previous_failure = (result["outcome"] == "failure")
            .then(|| args.iter().map(|arg| (*arg).to_owned()).collect());
        assert_eq!(result["schema_version"], 1);
        assert_eq!(
            output.status.code(),
            Some(if result["outcome"] == "failure" {
                if result["errors"][0]["code"] == "ARGUMENT_INVALID" {
                    2
                } else {
                    1
                }
            } else {
                0
            }),
            "{result}"
        );
        assert!(
            output.stdout.len() <= 8192,
            "per-call output budget exceeded"
        );
        result
    }

    /// Check the scenario budget and print measurements only after its assertions pass.
    fn finish(&self, calls: usize) {
        assert_eq!(self.calls, calls, "tool-call budget");
        assert_eq!(self.retries, 0, "unnecessary retries");
        assert!(self.bytes <= calls * 8192);
        println!(
            "{}",
            serde_json::json!({"task_complete":true,"tool_calls":self.calls,"output_bytes":self.bytes,"unnecessary_retries":self.retries})
        );
    }
}

/// Keep child stdin closed and isolate cache, credential, and crash-test settings.
fn command(project: &TempProject, home: &TempHome, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .current_dir(project.path())
        .args(args)
        .stdin(Stdio::null())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CACHE_HOME", home.path())
        .env("LOCALAPPDATA", home.path())
        .env_remove("TAPID_NPM_TOKEN")
        .env_remove("TAPID_TEST_AGENT_TOKEN")
        .env_remove("TAPID_TEST_CRASH_POINT")
        .env_remove("TAPID_TEST_RECOVER_ONLY");
    command
}

/// Copy the npm placement fixture without resolving or contacting a registry.
fn npm_project() -> TempProject {
    let project = TempProject::new("agent-npm").unwrap();
    for (name, bytes) in [
        (
            "package.json",
            include_bytes!("fixtures/npm-import/package.json").as_slice(),
        ),
        (
            "package-lock.json",
            include_bytes!("fixtures/npm-import/package-lock.json").as_slice(),
        ),
        (
            "registry.json",
            include_bytes!("fixtures/npm-import/registry.json").as_slice(),
        ),
    ] {
        project.write(name, bytes).unwrap();
    }
    project
}

/// Require a completed operation with no retry advice.
fn success(result: &Value) {
    assert_eq!(result["outcome"], "success", "{result}");
    assert_eq!(result["retry"], Value::Null);
}

/// Execute the guide as one sequence while preserving npm selections and the original npm lock.
#[test]
fn agent_guide_examples_complete_with_bounded_calls_and_preserved_npm_lock() {
    let guide = include_str!("../../../docs/agent-guide.md");
    assert!(guide.contains(&format!("CLI source version {}", env!("CARGO_PKG_VERSION"))));
    assert!(guide.len() < 6000, "keep the shared guide compact");
    let lf_guide = guide.replace("\r\n", "\n");
    for (format, text) in [
        ("LF", lf_guide.clone()),
        ("CRLF", lf_guide.replace('\n', "\r\n")),
    ] {
        assert!(
            text.len() < 6000,
            "keep the shared guide compact with {format} line endings: {} bytes",
            text.len()
        );
    }
    let project = npm_project();
    let home = TempHome::new("agent-guide").unwrap();
    let original_npm = fs::read(project.path().join("package-lock.json")).unwrap();
    let mut metrics = Measurements::default();
    let store = project.path().join("store");
    let mut pinned = None;
    let mut examples = 0;
    for line in guide
        .lines()
        .filter(|line| line.starts_with("tapid --json "))
    {
        let args = line
            .strip_prefix("tapid ")
            .unwrap()
            .split_whitespace()
            .map(|arg| {
                if arg == "\"$PWD/store\"" {
                    store.to_str().unwrap()
                } else {
                    arg
                }
            })
            .collect::<Vec<_>>();
        let manifest_before = fs::read(project.path().join("package.json")).unwrap();
        let result = metrics.call(&project, &home, &args);
        if args.contains(&"outdated") {
            assert_eq!(result["outcome"], "partial");
        } else {
            success(&result);
        }
        examples += 1;
        if args.contains(&"import-package-lock") {
            pinned = Some(fs::read(project.path().join("tapid.lock")).unwrap());
        }
        if args.contains(&"add") || args.contains(&"remove") {
            pinned = Some(fs::read(project.path().join("tapid.lock")).unwrap());
        }
        if args.contains(&"ci") || args.contains(&"install") {
            let current = fs::read(project.path().join("tapid.lock")).unwrap();
            let expected = pinned.as_ref().unwrap();
            if args.contains(&"install") {
                // First imported replay adds verification receipts, preserving selections.
                let before = tapid_lockfile::ImportedNpmLockfile::from_json(
                    std::str::from_utf8(expected).unwrap(),
                )
                .unwrap()
                .graph()
                .unwrap();
                let after = tapid_lockfile::ImportedNpmLockfile::from_json(
                    std::str::from_utf8(&current).unwrap(),
                )
                .unwrap()
                .graph()
                .unwrap();
                assert_eq!(after.packages, before.packages);
            } else {
                assert_eq!(&current, expected);
            }
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                manifest_before
            );
            assert_eq!(
                result["data"]["package_count"],
                if args.contains(&"ci") { 5 } else { 4 }
            );
        }
        assert_eq!(
            fs::read(project.path().join("package-lock.json")).unwrap(),
            original_npm
        );
    }
    assert_eq!(examples, 10);
    metrics.finish(10);
}

/// Compare complete package records so unrelated version, source, and edge changes fail the test.
#[test]
fn agent_dependency_edits_and_repeated_install_preserve_unselected_graph() {
    let project = TempProject::new("agent-edits").unwrap();
    let home = TempHome::new("agent-edits").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let mut metrics = Measurements::default();
    let store = project.path().join("store");
    let mut invoke = |prefix: &[&str]| {
        let mut args = vec!["--json"];
        args.extend_from_slice(prefix);
        args.extend([
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ]);
        let result = metrics.call(&project, &home, &args);
        success(&result);
    };
    invoke(&["install"]);
    let before = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    invoke(&["add", "@actual/scoped@1.2.0"]);
    let added = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    for (key, package) in before.packages() {
        assert_eq!(added.packages().get(key), Some(package));
    }
    assert_eq!(added.packages().len(), before.packages().len() + 1);
    invoke(&["remove", "@actual/scoped"]);
    invoke(&["install"]);
    let after = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    assert_eq!(after.packages(), before.packages());
    metrics.finish(4);
}

/// Treat precise refusal with preserved project outputs as successful completion of unsupported tasks.
#[test]
fn agent_stops_on_precise_failures_without_retry_or_file_mutation() {
    let home = TempHome::new("agent-stop").unwrap();
    for (label, manifest, config, fixture, args, code) in [
        (
            "arguments",
            r#"{"name":"app","version":"1.0.0"}"#,
            "",
            "{}",
            vec!["--json", "ci", "--unknown"],
            "ARGUMENT_INVALID",
        ),
        (
            "credentials",
            r#"{"name":"app","version":"1.0.0","dependencies":{"@private/pkg":"1.0.0"}}"#,
            "[registries.'@private']\nurl='https://private.invalid'\ntoken-env='TAPID_TEST_AGENT_TOKEN'\n",
            "{}",
            vec!["--json", "install", "--store-dir", "store"],
            "REGISTRY_AUTH_MISSING",
        ),
        (
            "unsupported",
            r#"{"name":"app","version":"1.0.0","dependencies":{"pkg":"workspace:1.0.0"}}"#,
            "",
            "{}",
            vec![
                "--json",
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                "store",
            ],
            "INVALID_REQUEST",
        ),
        (
            "peer",
            r#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
            "",
            "{}",
            vec![
                "--json",
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                "store",
            ],
            "PEER_DEPENDENCY_UNSATISFIED",
        ),
    ] {
        let project = TempProject::new(label).unwrap();
        project.write("package.json", manifest.as_bytes()).unwrap();
        project.write("tapid.toml", config.as_bytes()).unwrap();
        if label == "peer" {
            let mut fixture: Value =
                serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
            fixture["packages"][0]["peerDependencies"] = serde_json::json!({"react":"^18"});
            project
                .write("registry.json", fixture.to_string().as_bytes())
                .unwrap();
        } else {
            project.write("registry.json", fixture.as_bytes()).unwrap();
        }
        project.write("node_modules/KEEP", b"previous").unwrap();
        let mut metrics = Measurements::default();
        let result = metrics.call(&project, &home, &args);
        assert_eq!(result["errors"][0]["code"], code, "{label}: {result}");
        assert_eq!(result["changes"]["state"], "unchanged");
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/KEEP")).unwrap(),
            b"previous"
        );
        assert!(!project.path().join("tapid.lock").exists());
        metrics.finish(1);
    }
}

/// Populate a cold shared cache once, then replay another checkout without changing pins.
#[test]
fn agent_separate_checkouts_share_verified_cache_without_graph_changes() {
    let first = npm_project();
    let second = npm_project();
    let home = TempHome::new("agent-shared-cache").unwrap();
    let store = home.path().join("store");
    let store = store.to_str().unwrap();
    let mut metrics = Measurements::default();
    let seed = first.path().join("seed-store");
    success(&metrics.call(
        &first,
        &home,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            seed.to_str().unwrap(),
        ],
    ));
    let pinned = fs::read(first.path().join("tapid.lock")).unwrap();
    second.write("tapid.lock", &pinned).unwrap();
    for (index, project) in [&first, &second].into_iter().enumerate() {
        let mut args = vec![
            "--json",
            "ci",
            "--store-dir",
            store,
            "--registry-fixture",
            "registry.json",
        ];
        if index == 1 {
            args.push("--offline");
        }
        success(&metrics.call(project, &home, &args));
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), pinned);
        assert!(
            project
                .path()
                .join("node_modules/parent/package.json")
                .is_file()
        );
    }
    metrics.finish(3);
}

/// Hold the project lock to prove contention cannot begin a dependency edit.
#[test]
fn agent_same_project_contention_stops_before_dependency_edit() {
    use fs4::FileExt;
    let project = npm_project();
    let home = TempHome::new("agent-contention").unwrap();
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let lock = fs::File::create(project.path().join(".tapid-activation.lock")).unwrap();
    FileExt::lock(&lock).unwrap();
    let mut metrics = Measurements::default();
    let result = metrics.call(
        &project,
        &home,
        &["--json", "import-package-lock", "package-lock.json"],
    );
    assert_eq!(result["errors"][0]["code"], "PROJECT_BUSY");
    assert_eq!(result["retry"], "after_contention");
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join("tapid.lock").exists());
    metrics.finish(1);
}

/// Recover a lost commit response through the documented install caller before another mutation.
#[test]
fn agent_lost_commit_response_recovers_before_repeating_mutation() {
    let project = TempProject::new("agent-lost-response").unwrap();
    let home = TempHome::new("agent-lost-response").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let args = [
        "--json",
        "install",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        "store",
    ];
    let output = command(&project, &home, &args)
        .env("TAPID_TEST_CRASH_POINT", "commit_decision")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "commit result was lost");
    let pinned = fs::read(project.path().join("tapid.lock")).unwrap();
    let mut metrics = Measurements {
        calls: 1,
        bytes: output.stdout.len() + output.stderr.len(),
        ..Measurements::default()
    };
    // Inspection must stop on the pending transaction, without recovering it.
    let result = metrics.call(&project, &home, &["--json", "outdated", "--offline"]);
    assert_eq!(result["retry"], "recover_first");
    assert_eq!(result["changes"]["state"], "recovery_required");
    // Exercise the documented install recovery without re-resolving the committed graph.
    let recovered = metrics.call(
        &project,
        &home,
        &[
            "--json",
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            "store",
        ],
    );
    success(&recovered);
    assert!(
        recovered["warnings"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("PREVIOUS_TRANSACTION_RECOVERED"))
    );
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), pinned);
    metrics.finish(3);
}
