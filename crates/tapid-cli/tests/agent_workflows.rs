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
        self.record(command(project, home, args).output().unwrap(), args)
    }

    fn record(&mut self, output: std::process::Output, args: &[&str]) -> Value {
        if self
            .previous_failure
            .as_ref()
            .is_some_and(|previous| previous.iter().map(String::as_str).eq(args.iter().copied()))
        {
            self.retries += 1;
        }
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
        .env_remove("TAPID_TEST_RECOVER_ONLY")
        .env_remove("TAPID_TEST_PAUSE_POINT")
        .env_remove("TAPID_TEST_PAUSE_READY");
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
    let lf = guide.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    for (representation, text) in [("LF", &lf), ("CRLF", &crlf)] {
        assert!(
            text.len() < 6000,
            "keep the shared guide compact: {representation} has {} bytes",
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
                project.write("observed-before.lock", expected).unwrap();
                project.write("observed-after.lock", &current).unwrap();
                let comparison = metrics.call(
                    &project,
                    &home,
                    &[
                        "--json",
                        "lock",
                        "compare",
                        "observed-before.lock",
                        "observed-after.lock",
                        "--limit",
                        "1",
                    ],
                );
                success(&comparison);
                assert_eq!(comparison["data"]["summary"]["changed_selections"], 0);
                assert_eq!(comparison["data"]["summary"]["added_instances"], 0);
                assert_eq!(
                    comparison["data"]["before_coverage"]["analysis_complete"],
                    true
                );
                assert!(
                    comparison["data"]["summary"]["evidence_changes"]
                        .as_u64()
                        .unwrap()
                        > 0
                );
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
    metrics.finish(12);
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
    fs::copy(
        project.path().join("tapid.lock"),
        project.path().join("observed-before.lock"),
    )
    .unwrap();
    invoke(&["add", "@actual/scoped@1.2.0"]);
    fs::copy(
        project.path().join("tapid.lock"),
        project.path().join("observed-after.lock"),
    )
    .unwrap();
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
    let comparison = metrics.call(
        &project,
        &home,
        &[
            "--json",
            "lock",
            "compare",
            "observed-before.lock",
            "observed-after.lock",
            "--limit",
            "1",
        ],
    );
    success(&comparison);
    assert_eq!(comparison["data"]["scope"]["kind"], "all");
    assert_eq!(
        comparison["data"]["before_coverage"]["analysis_complete"],
        true
    );
    assert_eq!(
        comparison["data"]["before_coverage"]["excluded_instances"],
        0
    );
    assert_eq!(comparison["data"]["summary"]["added_instances"], 1);
    assert_eq!(comparison["data"]["summary"]["removed_instances"], 0);
    assert_eq!(
        comparison["data"]["records"][0]["value"]["name"],
        "@actual/scoped"
    );
    assert_eq!(comparison["data"]["summary"]["evidence_changes"], 1); // declaration digest only
    metrics.finish(5);
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
    let started = std::time::Instant::now();
    let result = metrics.call(
        &project,
        &home,
        &["--json", "import-package-lock", "package-lock.json"],
    );
    assert_eq!(result["errors"][0]["code"], "PROJECT_BUSY");
    assert_eq!(result["retry"], "after_contention");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join("tapid.lock").exists());
    FileExt::unlock(&lock).unwrap();
    metrics.previous_failure = None;
    success(&metrics.call(
        &project,
        &home,
        &["--json", "import-package-lock", "package-lock.json"],
    ));
    metrics.finish(2);
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
    assert!(output.stdout.len() + output.stderr.len() <= 8192);
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

/// Isolated projects still contend on one store; the holder is ready before spawn.
#[test]
fn agent_shared_store_contention_expires_and_release_allows_retry() {
    use fs4::FileExt;
    use std::time::{Duration, Instant};
    let first = npm_project();
    let second = npm_project();
    let home = TempHome::new("agent-store-contention").unwrap();
    let store = home.path().join("store");
    let store_arg = store.to_str().unwrap();
    let args = [
        "--json",
        "install",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        store_arg,
    ];
    let mut metrics = Measurements::default();
    success(&metrics.call(&first, &home, &args));
    let manifest = fs::read(second.path().join("package.json")).unwrap();
    second.write("node_modules/KEEP", b"previous").unwrap();
    second
        .write(".tapid-managed", b"tapid-managed-v1\n")
        .unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.join(".store.lock"))
        .unwrap();
    FileExt::lock(&holder).unwrap();
    let started = Instant::now();
    let mut child = command(&second, &home, &args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(9) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("shared-store contender exceeded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let elapsed = started.elapsed();
    let result = metrics.record(child.wait_with_output().unwrap(), &args);
    assert_eq!(result["errors"][0]["code"], "STORE_BUSY", "{result}");
    assert_eq!(result["changes"]["state"], "unchanged", "{result}");
    assert_eq!(result["retry"], "after_contention");
    assert!(elapsed >= Duration::from_secs(5));
    assert!(elapsed < Duration::from_secs(7), "{elapsed:?}");
    assert_eq!(
        fs::read(second.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!second.path().join("tapid.lock").exists());
    assert_eq!(
        fs::read(second.path().join("node_modules/KEEP")).unwrap(),
        b"previous"
    );
    // Retry after observed release; store tests cover release during an active wait.
    FileExt::unlock(&holder).unwrap();
    let mut child = command(&second, &home, &args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(9);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("released store contender exceeded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // This retry follows observed release and is necessary, rather than blind repetition.
    metrics.previous_failure = None;
    success(&metrics.record(child.wait_with_output().unwrap(), &args));
    metrics.finish(3);
    println!("shared_store_expiry_ms={}", elapsed.as_millis());
}

#[derive(Debug, PartialEq, Eq)]
enum EntrySnapshot {
    Directory,
    File(Vec<u8>),
    Link(std::path::PathBuf),
}

/// Compare entries and bytes, excluding timestamps (reads may update access time).
fn snapshot(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, EntrySnapshot> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        entries: &mut std::collections::BTreeMap<std::path::PathBuf, EntrySnapshot>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            let value = if kind.is_symlink() {
                EntrySnapshot::Link(fs::read_link(&path).unwrap())
            } else if kind.is_dir() {
                visit(root, &path, entries);
                EntrySnapshot::Directory
            } else {
                EntrySnapshot::File(fs::read(&path).unwrap())
            };
            entries.insert(path.strip_prefix(root).unwrap().to_owned(), value);
        }
    }
    let mut entries = std::collections::BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn cancel_at_checkpoint(
    invocation: &mut Command,
    home: &TempHome,
    checkpoint: &str,
) -> std::process::Output {
    let ready = home.path().join("pause-ready");
    let mut child = invocation
        .env("TAPID_TEST_PAUSE_POINT", checkpoint)
        .env("TAPID_TEST_PAUSE_READY", &ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            panic!("exited before cancellation checkpoint: {output:?}");
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("cancellation checkpoint watchdog expired");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    #[cfg(unix)]
    {
        // Signal only the CLI parent, without asserting process-tree termination.
        let sent = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        if sent != 0 {
            let _ = child.kill();
            let _ = child.wait();
            panic!("cannot send cancellation signal");
        }
    }
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("cancelled parent did not exit");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(libc::SIGTERM));
    }
    output
}

#[test]
fn agent_interruptions_preserve_durable_decision_and_status_is_read_only() {
    for (checkpoint, cancellation) in [
        ("activation_complete", false),
        ("commit_decision", false),
        ("post_commit_cleanup", false),
        #[cfg(unix)]
        ("activation_complete", true),
        #[cfg(unix)]
        ("commit_decision", true),
        #[cfg(unix)]
        ("post_commit_cleanup", true),
    ] {
        let project = TempProject::new("agent-interrupted").unwrap();
        let home = TempHome::new("agent-interrupted").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
            )
            .unwrap();
        project
            .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
            .unwrap();
        let store = project.path().join("store");
        let store_arg = store.to_str().unwrap();
        let mut invocation = command(
            &project,
            &home,
            &[
                "--json",
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store_arg,
            ],
        );
        let output = if cancellation {
            cancel_at_checkpoint(&mut invocation, &home, checkpoint)
        } else {
            invocation
                .env("TAPID_TEST_CRASH_POINT", checkpoint)
                .output()
                .unwrap()
        };
        assert!(
            !output.status.success(),
            "missing interruption at {checkpoint}"
        );
        assert!(output.stdout.is_empty());
        assert!(output.stdout.len() + output.stderr.len() <= 8192);
        let pending = fs::read(project.path().join("tapid.lock")).unwrap();
        let mut metrics = Measurements {
            calls: 1,
            bytes: output.stdout.len() + output.stderr.len(),
            ..Measurements::default()
        };
        let before = snapshot(project.path());
        let status = metrics.call(&project, &home, &["--json", "status"]);
        assert_eq!(status["outcome"], "failure", "{status}");
        assert_eq!(status["data"]["recovery"], "pending");
        assert_eq!(
            snapshot(project.path()),
            before,
            "inspection changed {checkpoint}"
        );
        // The recovery-only debug seam exercises settlement without a fresh mutation.
        let recovery = command(
            &project,
            &home,
            &["--json", "install", "--offline", "--store-dir", store_arg],
        )
        .env("TAPID_TEST_RECOVER_ONLY", "1")
        .output()
        .unwrap();
        success(&metrics.record(
            recovery,
            &["--json", "install", "--offline", "--store-dir", store_arg],
        ));
        if checkpoint == "activation_complete" {
            assert!(!project.path().join("tapid.lock").exists());
            assert!(!project.path().join("node_modules").exists());
            assert!(!store.join("trees").exists());
        } else {
            assert_eq!(
                fs::read(project.path().join("tapid.lock")).unwrap(),
                pending
            );
            assert!(
                project
                    .path()
                    .join("node_modules/h3/package.json")
                    .is_file()
            );
            assert!(store.join("trees").is_dir());
        }
        assert!(!store.join(".tapid-transaction.json").exists());
        assert!(
            !project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
        metrics.finish(3);
        println!("interruption_checkpoint={checkpoint} cancellation={cancellation}");
    }
}

#[test]
fn agent_response_lost_after_cleanup_reconciles_current_state_without_attribution() {
    let project = TempProject::new("agent-loss").unwrap();
    let home = TempHome::new("agent-loss").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let store = project.path().join("store");
    let args = [
        "--json",
        "install",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        store.to_str().unwrap(),
    ];
    let output = command(&project, &home, &args).output().unwrap();
    // The harness observes completion, then withholds the result from reconciliation.
    assert!(output.status.success(), "{output:?}");
    let mut metrics = Measurements {
        calls: 1,
        bytes: output.stdout.len() + output.stderr.len(),
        ..Measurements::default()
    };
    assert!(output.stdout.len() <= 8192);
    assert!(output.stderr.is_empty());
    assert!(
        !project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
    let pinned = fs::read(project.path().join("tapid.lock")).unwrap();
    let before = snapshot(project.path());
    let status = metrics.call(&project, &home, &["--json", "status"]);
    success(&status);
    assert_eq!(status["changes"]["state"], "unchanged");
    assert_eq!(snapshot(project.path()), before);
    assert!(
        project
            .path()
            .join("node_modules/h3/package.json")
            .is_file()
    );
    success(&metrics.call(
        &project,
        &home,
        &[
            "--json",
            "add",
            "@actual/scoped@1.2.0",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    ));
    assert_ne!(fs::read(project.path().join("tapid.lock")).unwrap(), pinned);
    let before = snapshot(project.path());
    success(&metrics.call(&project, &home, &["--json", "status"]));
    assert_eq!(snapshot(project.path()), before);
    metrics.finish(4);
}

/// Refresh a selected root while newer compatible metadata leaves unrelated records pinned.
#[test]
fn agent_named_update_refreshes_selection_and_preserves_unrelated_records() {
    let project = TempProject::new("agent-update").unwrap();
    let home = TempHome::new("agent-update").unwrap();
    let manifest =
        br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"*","@actual/scoped":"^1"}}"#;
    project.write("package.json", manifest).unwrap();
    let mut fixture: Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
    let initial = serde_json::json!({"packages": fixture["packages"].as_array().unwrap()
        .iter().filter(|p| p["version"] == "1.0.0" || p["version"] == "1.2.0")
        .cloned().collect::<Vec<_>>()});
    project
        .write("registry.json", initial.to_string().as_bytes())
        .unwrap();
    let store = project.path().join("store");
    let mut metrics = Measurements::default();
    success(&metrics.call(
        &project,
        &home,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    ));
    let before = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    let mut newer = fixture["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "@actual/scoped")
        .unwrap()
        .clone();
    newer["version"] = serde_json::json!("1.3.0");
    fixture["packages"].as_array_mut().unwrap().push(newer);
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    success(&metrics.call(
        &project,
        &home,
        &[
            "--json",
            "update",
            "h3",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    ));
    let after = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    let selected = |lock: &tapid_lockfile::Lockfile| {
        lock.packages()
            .values()
            .find(|package| package.name() == "h3")
            .unwrap()
            .version()
            .to_string()
    };
    assert_eq!(selected(&before), "1.0.0");
    assert_eq!(selected(&after), "2.0.0");
    for (key, package) in before.packages() {
        if package.name() != "h3" {
            assert_eq!(after.packages().get(key), Some(package));
        }
    }
    assert_eq!(after.packages().len(), before.packages().len());
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    metrics.finish(2);
}

/// Exercise the guide's separate run channel before launch, without requiring native containment.
#[test]
fn agent_run_example_reads_complete_typed_result_file() {
    let guide = include_str!("../../../docs/agent-guide.md");
    let example = guide
        .lines()
        .find(|line| line.starts_with("tapid run "))
        .unwrap();
    assert_eq!(example, "tapid run SCRIPT --result-file PATH");
    let project = npm_project();
    let home = TempHome::new("agent-run-result").unwrap();
    let path = home.path().join("result.json");
    let args = example
        .strip_prefix("tapid ")
        .unwrap()
        .split_whitespace()
        .map(|arg| match arg {
            "SCRIPT" => "missing",
            "PATH" => path.to_str().unwrap(),
            other => other,
        })
        .collect::<Vec<_>>();
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let npm_lock = fs::read(project.path().join("package-lock.json")).unwrap();
    let output = command(&project, &home, &args).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let bytes = fs::read(&path).unwrap();
    assert!(bytes.ends_with(b"\n"));
    let result: Value = serde_json::from_slice(&bytes).expect("one complete run result");
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "run");
    assert_eq!(result["outcome"], "prelaunch_failure");
    assert_eq!(result["error"]["code"], "SCRIPT_MISSING");
    assert!(result["receipt"].is_null());
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(project.path().join("package-lock.json")).unwrap(),
        npm_lock
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    Measurements {
        calls: 1,
        bytes: output.stdout.len() + output.stderr.len() + bytes.len(),
        ..Measurements::default()
    }
    .finish(1);
}
