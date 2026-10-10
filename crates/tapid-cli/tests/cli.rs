use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_lockfile::Lockfile;

#[path = "cli_cases/ci.rs"]
mod ci_tests;
#[path = "cli_cases/explain.rs"]
mod explain_tests;
mod workspace_acceptance;

/// Verifies exact license output with no application environment and invalid project files.
#[test]
fn license_prints_complete_apache_text_without_accessing_a_project() {
    let project = tapid_test_support::TempProject::new("license").unwrap();
    project.write("package.json", b"invalid manifest").unwrap();
    project
        .write("tapid.toml", b"invalid configuration")
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .arg("license")
        .current_dir(project.path())
        .env_clear();
    // Keep coverage instrumentation from writing a profile into the project.
    if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile_file);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stderr.is_empty());
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let packaged_license = manifest_dir.join("LICENSE");
    let license_path = if packaged_license.is_file() {
        packaged_license
    } else {
        manifest_dir.join("../../LICENSE")
    };
    let license = fs::read_to_string(license_path).unwrap();
    assert!(license.trim_start().starts_with("Apache License"));
    let expected = format!("Copyright 2026 LimeTip AB.\n\n{license}");
    assert_eq!(output.stdout, expected.as_bytes());
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 2);
}

fn temp_dir(label: &str) -> PathBuf {
    static NEXT_TEMP_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT_TEMP_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "tapid-cli-{label}-{}-{nonce}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn test_homes()
-> &'static std::sync::Mutex<std::collections::BTreeMap<PathBuf, tapid_test_support::TempHome>> {
    static HOMES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<PathBuf, tapid_test_support::TempHome>>,
    > = std::sync::OnceLock::new();
    HOMES.get_or_init(Default::default)
}

fn isolated_command(cwd: &Path, args: &[&str], clear_environment: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    if clear_environment {
        command.env_clear();
    }
    command.args(args);
    if args.first().is_some_and(|command| {
        matches!(
            *command,
            "install" | "i" | "add" | "remove" | "update" | "prune"
        )
    }) && !args.contains(&"--store-dir")
    {
        command.arg("--store-dir").arg(cwd.join(".test-store"));
    }
    let mut homes = test_homes().lock().unwrap();
    let home = homes
        .entry(cwd.to_path_buf())
        .or_insert_with(|| tapid_test_support::TempHome::new("cli-install").unwrap());
    command
        .current_dir(cwd)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path())
        .env("LOCALAPPDATA", home.path());
    command
}

fn prior_empty_lock() -> String {
    Lockfile::new(&format!("sha256-{}", "0".repeat(64)))
        .unwrap()
        .to_json()
        .unwrap()
}

fn run(cwd: &Path, args: &[&str]) -> std::process::Output {
    isolated_command(cwd, args, false).output().unwrap()
}
fn run_with_env(cwd: &Path, args: &[&str], key: &str, value: &str) -> std::process::Output {
    isolated_command(cwd, args, false)
        .env(key, value)
        .output()
        .unwrap()
}

fn run_with_isolated_path(cwd: &Path, args: &[&str], path: &OsStr) -> std::process::Output {
    isolated_command(cwd, args, true)
        .env("PATH", path)
        .output()
        .unwrap()
}

// Keep Node optional for local test runs; native CI explicitly opts in below.
fn node_assertion_output(
    command: &mut Command,
    required: bool,
    context: &str,
) -> Option<std::process::Output> {
    match command.output() {
        Ok(output) => Some(output),
        Err(error) if required => {
            panic!("Node assertion probe required but could not spawn ({context}): {error}")
        }
        Err(_) => None,
    }
}

#[test]
#[should_panic(
    expected = "Node assertion probe required but could not spawn (missing-node-regression)"
)]
fn required_node_assertion_probe_rejects_spawn_failure() {
    let project = tapid_test_support::TempProject::new("missing-node-probe").unwrap();
    node_assertion_output(
        &mut Command::new(project.path().join("node-does-not-exist")),
        true,
        "missing-node-regression",
    );
}

#[test]
fn optional_node_assertion_probe_preserves_spawn_fallback() {
    let project = tapid_test_support::TempProject::new("optional-node-probe").unwrap();
    assert!(
        node_assertion_output(
            &mut Command::new(project.path().join("node-does-not-exist")),
            false,
            "optional-node-regression",
        )
        .is_none()
    );
}

#[test]
fn required_node_assertion_probe_preserves_spawned_output() {
    // Use this test executable so these helper regressions need no Node setup.
    let output = node_assertion_output(
        Command::new(std::env::current_exe().unwrap()).arg("--list"),
        true,
        "spawned-output-regression",
    )
    .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("required_node_assertion_probe_preserves_spawned_output: test")
    );
}

#[test]
fn install_output_summarizes_changes_and_replay() {
    let project = tapid_test_support::TempProject::new("install-output").unwrap();
    project
        .write("package.json", br#"{"name":"output","version":"1.0.0"}"#)
        .unwrap();
    let output = run(project.path(), &["install"]);
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Installed 0 package(s) in "), "{stdout}");
    assert!(
        stdout.contains("Changed: node_modules, tapid.lock"),
        "{stdout}"
    );
    assert!(!stdout.contains("package.json"), "{stdout}");
    assert!(
        stdout.contains("Lock selections: 0 added, 0 changed, 0 reused, 0 removed"),
        "{stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output);
    let output = run(project.path(), &["install", "--offline", "--frozen"]);
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Replayed lockfile: 0 package(s) in "),
        "{stdout}"
    );
    assert!(stdout.contains("Changed: node_modules"), "{stdout}");
    assert!(!stdout.contains("tapid.lock"), "{stdout}");
}

#[test]
fn install_output_counts_exact_lock_selections() {
    let project = tapid_test_support::TempProject::new("install-output-counts").unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let manifest = br#"{"name":"output","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    let args = ["install", "--registry-fixture", "registry.json"];
    for expected in [
        "Lock selections: 1 added, 0 changed, 0 reused, 0 removed",
        "Lock selections: 0 added, 0 changed, 1 reused, 0 removed",
    ] {
        let output = run(project.path(), &args);
        assert!(output.status.success(), "{:?}", output);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{:?}",
            output
        );
        assert!(output.stderr.is_empty(), "{:?}", output);
    }
    let output = run(project.path(), &["install", "--offline", "--frozen"]);
    assert!(output.status.success(), "{:?}", output);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Lock selections: 0 added, 0 changed, 1 reused, 0 removed")
    );
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("tapid.lock")).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = serde_json::json!("https://registry.npmjs.org/h3/-/h3-1.0.0.tgz");
    }
    project
        .write("tapid.lock", &serde_json::to_vec(&lock).unwrap())
        .unwrap();
    let output = run(project.path(), &args);
    assert!(output.status.success(), "{:?}", output);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Lock selections: 0 added, 0 changed, 1 reused, 0 removed"),
        "{:?}",
        output
    );
    project
        .write(
            "package.json",
            br#"{"name":"output","version":"1.0.0","dependencies":{"h3":"2.0.0"}}"#,
        )
        .unwrap();
    let output = run(project.path(), &args);
    assert!(output.status.success(), "{:?}", output);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Lock selections: 1 added, 0 changed, 0 reused, 1 removed"),
        "{:?}",
        output
    );
}

#[test]
fn install_output_reports_registry_failure_without_success_summary() {
    let project = tapid_test_support::TempProject::new("install-output-registry").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"output","version":"1.0.0","dependencies":{"@acme/private":"1.0.0"}}"#,
        )
        .unwrap();
    project.write("tapid.toml", b"[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_INSTALL_OUTPUT_MISSING'\n").unwrap();
    let mut command = isolated_command(project.path(), &["install"], true);
    command.env_remove("TAPID_INSTALL_OUTPUT_MISSING");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Install failed during registry access."),
        "{stderr}"
    );
    assert!(stderr.contains("REGISTRY_AUTH_MISSING"), "{stderr}");
    assert!(
        stderr.contains("Check the credentials configured"),
        "{stderr}"
    );
    assert!(stderr.contains("TAPID_INSTALL_OUTPUT_MISSING"), "{stderr}");
}

#[test]
fn install_output_explains_missing_lock_without_claiming_changes() {
    let project = tapid_test_support::TempProject::new("install-output-failure").unwrap();
    project
        .write("package.json", br#"{"name":"output","version":"1.0.0"}"#)
        .unwrap();
    let output = run(project.path(), &["install", "--offline", "--frozen"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("diagnostic: LOCKFILE_MISSING"), "{stderr}");
    assert!(stderr.contains("Project files unchanged."), "{stderr}");
    assert!(
        stderr.contains("Install failed during lockfile validation."),
        "{stderr}"
    );
    assert!(
        stderr.contains("Run 'tapid install' online to create tapid.lock."),
        "{stderr}"
    );
}

#[test]
fn install_accepts_a_relative_project_directory() {
    let cwd = temp_dir("relative-project-dir");
    let project = cwd.join("project");
    fs::create_dir_all(&project).unwrap();
    let manifest = r#"{"name":"relative-project","version":"1.0.0"}"#;
    fs::write(project.join("package.json"), manifest).unwrap();
    let store = cwd.join("store");

    let output = run(
        &cwd,
        &[
            "install",
            "--project-dir",
            "project",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(project.join("tapid.lock").is_file());
    assert_eq!(
        fs::read(project.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    cleanup(cwd);
}

fn cleanup(path: PathBuf) {
    test_homes().lock().unwrap().remove(&path);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn aliased_root_cycle_keeps_locked_dependencies_during_install_and_replay() {
    let project = tapid_test_support::TempProject::new("aliased-root-cycle").unwrap();
    let dir = project.path().to_path_buf();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"app":"npm:a@1.0.0","b":"2.0.0"}}"#).unwrap();
    let fixture = project
        .write(
            "registry.json",
            include_bytes!("fixtures/aliased-root-cycle.json"),
        )
        .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "i",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let lock_bytes = fs::read(dir.join("tapid.lock")).unwrap();
    for replay in [false, true] {
        if replay {
            fs::remove_dir_all(dir.join("node_modules")).unwrap();
            let output = run(
                &dir,
                &[
                    "i",
                    "--offline",
                    "--frozen",
                    "--store-dir",
                    store.to_str().unwrap(),
                ],
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for path in ["app/node_modules/b/index.js", "a/node_modules/b/index.js"] {
            assert_eq!(
                fs::read_to_string(dir.join("node_modules").join(path)).unwrap(),
                "module.exports = \"b1\";\n"
            );
        }
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/b/index.js")).unwrap(),
            "module.exports = \"b2\";\n"
        );
        if let Some(node) = node_assertion_output(
            Command::new("node")
                .args([
                    "-e",
                    "console.log(require('app'), require('a'), require('b'))",
                ])
                .current_dir(&dir),
            std::env::var_os("TAPID_REQUIRE_NODE_ASSERTIONS").is_some(),
            "aliased-root-cycle",
        ) {
            assert!(
                node.status.success(),
                "{}",
                String::from_utf8_lossy(&node.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&node.stdout).trim(), "b1 b1 b2");
            eprintln!(
                "TAPID_NODE_ASSERTION_OK aliased-root-cycle phase={} output=b1 b1 b2",
                if replay { "offline-frozen" } else { "install" }
            );
        }
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_bytes);
    }
}

#[test]
fn cross_registry_aliases_install_distinct_artifacts_and_replay_offline() {
    let project = tapid_test_support::TempProject::new("cross-registry-alias").unwrap();
    let dir = project.path().to_path_buf();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"local":"npm:@s/foo@1.0.0","jsr:@s/foo":"1.0.0"}}"#).unwrap();
    let fixture = project
        .write(
            "registry.json",
            include_bytes!("fixtures/cross-registry-alias.json"),
        )
        .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "i",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let lock_bytes = fs::read(dir.join("tapid.lock")).unwrap();
    let lock = Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
    assert_eq!(lock.packages().len(), 2);
    for replay in [false, true] {
        if replay {
            fs::remove_dir_all(dir.join("node_modules")).unwrap();
            let output = run(
                &dir,
                &[
                    "i",
                    "--offline",
                    "--frozen",
                    "--store-dir",
                    store.to_str().unwrap(),
                ],
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/local/index.js")).unwrap(),
            "module.exports = \"npm\";\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/@s/foo/index.js")).unwrap(),
            "module.exports = \"jsr\";\n"
        );
        assert_ne!(
            fs::canonicalize(dir.join("node_modules/local")).unwrap(),
            fs::canonicalize(dir.join("node_modules/@s/foo")).unwrap()
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_bytes);
    }
}

#[test]
fn npm_aliases_preserve_local_names_actual_identities_and_offline_replay() {
    let dir = temp_dir("npm-aliases");
    fs::write(dir.join("package.json"), r#"{"name":"demo","version":"1.0.0","dependencies":{"first":"npm:h3@1","second":"npm:h3@2.0.0","third":"npm:h3@1","alias-parent":"1.0.0","@local/direct":"npm:@actual/scoped@^1"}}"#).unwrap();
    let fixture = dir.join("registry.json");
    fs::write(&fixture, include_str!("fixtures/npm-aliases.json")).unwrap();
    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "i",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock_bytes = fs::read(dir.join("tapid.lock")).unwrap();
    let lock = Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
    assert_eq!(lock.packages().len(), 5);
    for args in [
        vec!["install", "--frozen"],
        vec!["install", "--offline", "--frozen"],
    ] {
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/first/index.js")).unwrap(),
            "module.exports = \"first\";\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/second/index.js")).unwrap(),
            "module.exports = \"second\";\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/h3-v2/index.js")).unwrap(),
            "module.exports = \"prerelease\";\n"
        );
        assert!(
            fs::read_to_string(dir.join("node_modules/@local/direct/package.json"))
                .unwrap()
                .contains("@actual/scoped")
        );
        assert!(
            dir.join("node_modules/.bin/scoped-tool").exists()
                || dir.join("node_modules/.bin/scoped-tool.cmd").exists()
        );
        assert!(!dir.join("node_modules/first/node_modules/h3").exists());
        assert!(dir.join("node_modules/third/index.js").is_file());
        if let Some(node) = node_assertion_output(
            Command::new("node").args(["-e", "console.log([require('first'), require('second'), require('third'), require('alias-parent'), require('@local/direct')].join('|'))"]).current_dir(&dir),
            std::env::var_os("TAPID_REQUIRE_NODE_ASSERTIONS").is_some(),
            "npm-aliases",
        ) {
            assert!(node.status.success(), "{}", String::from_utf8_lossy(&node.stderr));
            assert_eq!(String::from_utf8_lossy(&node.stdout).trim(), "first|second|first|prerelease:scoped|scoped");
            eprintln!(
                "TAPID_NODE_ASSERTION_OK npm-aliases phase={} output=first|second|first|prerelease:scoped|scoped",
                if args.contains(&"--offline") { "frozen" } else { "install" }
            );
        }

        fs::remove_dir_all(dir.join("node_modules")).unwrap();
        let mut args = args;
        args.extend(["--store-dir", store.to_str().unwrap()]);
        let replay = run(&dir, &args);
        assert!(
            replay.status.success(),
            "{}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_bytes);
    }
    cleanup(dir);
}

#[test]
fn npm_alias_cli_add_update_latest_and_outdated_keep_the_actual_target() {
    let dir = temp_dir("npm-alias-lifecycle");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    fs::write(&fixture, include_str!("fixtures/npm-aliases.json")).unwrap();
    let store = dir.join("store");
    let common = [
        "--registry-fixture",
        fixture.to_str().unwrap(),
        "--store-dir",
        store.to_str().unwrap(),
    ];
    let mut add = vec!["add", "local@npm:h3@1"];
    add.extend(common);
    let output = run(&dir, &add);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outdated = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(outdated.status.success());
    let report = String::from_utf8(outdated.stdout).unwrap();
    assert!(
        report.contains("declared=npm:h3@1 locked=1.0.0 compatible=1.0.0 available=2.0.1-rc.20"),
        "{report}"
    );
    let mut update = vec!["update", "local", "--latest"];
    update.extend(common);
    let output = run(&dir, &update);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(manifest["dependencies"]["local"], "npm:h3@*");
    let installed: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("node_modules/local/package.json")).unwrap())
            .unwrap();
    assert_eq!(installed["name"], "h3");
    assert_eq!(installed["version"], "2.0.0");
    cleanup(dir);
}

#[test]
fn npm_alias_routes_by_actual_scope_and_rejects_a_changed_route_on_replay() {
    let dir = temp_dir("npm-alias-private-route");
    fs::write(dir.join("package.json"), r#"{"name":"demo","version":"1.0.0","dependencies":{"@local/tool":"npm:@actual/scoped@^1"}}"#).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@actual']\nurl='https://packages.example'\n",
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    for package in registry["packages"].as_array_mut().unwrap() {
        if package["name"] == "@actual/scoped" {
            package["registry"] = "https://packages.example".into();
        }
    }
    fs::write(&fixture, registry.to_string()).unwrap();
    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "i",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock_bytes = fs::read(dir.join("tapid.lock")).unwrap();
    let lock = Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
    assert!(lock.roots()[0].starts_with("https://packages.example|@actual/scoped@"));
    let replay = run(
        &dir,
        &[
            "i",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@actual']\nurl='https://different.example'\n",
    )
    .unwrap();
    let rejected = run(
        &dir,
        &[
            "i",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("does not satisfy the manifest declaration")
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_bytes);
    assert!(dir.join("node_modules/@local/tool/index.js").is_file());
    cleanup(dir);
}

#[test]
fn invalid_npm_aliases_fail_before_project_or_store_mutation() {
    for declaration in [
        "npm:../outside@1",
        "npm:actual@npm:other@1",
        "npm:actual@latest",
        "npm:actual@",
    ] {
        let dir = temp_dir("invalid-npm-alias");
        let manifest = serde_json::json!({"name":"demo", "version":"1.0.0", "dependencies":{"local":declaration}}).to_string();
        fs::write(dir.join("package.json"), &manifest).unwrap();
        fs::write(dir.join("tapid.lock"), b"previous lock").unwrap();
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules/keep"), b"previous install").unwrap();
        let store = dir.join("store");
        let output = run(&dir, &["i", "--store-dir", store.to_str().unwrap()]);
        assert!(!output.status.success(), "{declaration}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(declaration));
        assert_eq!(
            fs::read_to_string(dir.join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), b"previous lock");
        assert_eq!(
            fs::read(dir.join("node_modules/keep")).unwrap(),
            b"previous install"
        );
        assert!(!store.exists());
        cleanup(dir);
    }
}

#[test]
fn legacy_replay_rejects_same_local_name_from_different_registries() {
    let dir = temp_dir("legacy-root-origin-collision");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"@actual/scoped":"1.2.0"}}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    fs::write(&fixture, include_str!("fixtures/npm-aliases.json")).unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "i",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"@actual/scoped":"1.2.0","jsr:@actual/scoped":"1.2.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let npm_key = lock["roots"][0].as_str().unwrap().to_owned();
    let jsr_key = npm_key.replace("https://registry.npmjs.org", "https://jsr.io");
    let mut jsr_package = lock["packages"][&npm_key].clone();
    jsr_package["registry"] = "https://jsr.io".into();
    lock["packages"][&jsr_key] = jsr_package;
    let mut roots = vec![npm_key, jsr_key];
    roots.sort();
    lock["roots"] = serde_json::json!(roots);
    lock["rootManifestDigest"] = format!(
        "sha256-{}",
        hex::encode(Sha256::digest(manifest.as_bytes()))
    )
    .into();
    lock["lockfileVersion"] = 6.into();
    lock.as_object_mut().unwrap().remove("rootBindings");
    let lock_bytes = lock.to_string();
    fs::write(dir.join("tapid.lock"), &lock_bytes).unwrap();
    let replay = run(
        &dir,
        &[
            "i",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!replay.status.success());
    assert!(
        String::from_utf8_lossy(&replay.stderr)
            .contains("conflicting package identities for local dependency '@actual/scoped'"),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        lock_bytes
    );
    assert!(dir.join("node_modules/@actual/scoped/index.js").is_file());
    cleanup(dir);
}

#[test]
fn missing_private_registry_credentials_fail_before_committing_manifest_changes() {
    let dir = temp_dir("missing-registry-credential");
    let original = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), original).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_MISSING'\n",
    )
    .unwrap();
    let store = dir.join("store");
    let previous_lock = prior_empty_lock();
    let previous_node_modules = b"previous installed tree marker";
    let previous_store = b"pre-existing verified store marker";
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/previous.txt"), previous_node_modules).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("previous.txt"), previous_store).unwrap();
    let output = run_with_isolated_path(
        &dir,
        &[
            "add",
            "@acme/private@1.0.0",
            "--store-dir",
            store.to_str().unwrap(),
        ],
        OsStr::new(""),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("TAPID_ISSUE164_MISSING"), "{stderr}");
    assert!(
        stderr.contains("diagnostic: REGISTRY_AUTH_MISSING"),
        "{stderr}"
    );
    assert!(!stderr.contains("secret"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        original.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        previous_lock.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("node_modules/previous.txt")).unwrap(),
        previous_node_modules
    );
    assert_eq!(
        fs::read(store.join("previous.txt")).unwrap(),
        previous_store
    );
    cleanup(dir);
}

#[test]
fn invalid_private_registry_credentials_are_redacted_and_fail_transactionally() {
    let dir = temp_dir("invalid-registry-credential");
    let original = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), original).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_INVALID'\n",
    )
    .unwrap();
    let store = dir.join("store");
    let invalid_secret = "synthetic-invalid-secret\nwith-newline";
    let output = run_with_env(
        &dir,
        &[
            "add",
            "@acme/private@1.0.0",
            "--store-dir",
            store.to_str().unwrap(),
        ],
        "TAPID_ISSUE164_INVALID",
        invalid_secret,
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid registry credential"), "{stderr}");
    assert!(!stderr.contains("synthetic-invalid-secret"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        original.as_bytes()
    );
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn scoped_registry_routing_selects_private_and_default_origins_and_replays_offline() {
    let dir = temp_dir("scoped-registry-routing");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"@acme/private":"1.0.0","left-pad":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.default]\nurl='https://mirror.example'\n[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_OFFLINE_ONLY'\n",
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://packages.example","name":"@acme/private","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}},{{"registry":"https://mirror.example","name":"left-pad","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let lock_json = fs::read_to_string(dir.join("tapid.lock")).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&lock_json).unwrap();
    let registries = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .map(|package| package["registry"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        registries,
        ["https://mirror.example", "https://packages.example"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let replay = run_with_isolated_path(
        &dir,
        &[
            "install",
            "--offline",
            "--store-dir",
            store.to_str().unwrap(),
        ],
        OsStr::new(""),
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    cleanup(dir);
}

#[test]
fn registry_credential_environment_variables_are_denied_to_root_scripts() {
    let dir = temp_dir("registry-credential-child-env");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"test":"printf child-started"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_CHILD_SECRET'\n[run.scripts.test]\nenvironment=['TAPID_ISSUE164_CHILD_SECRET']\n",
    )
    .unwrap();
    let secret = "synthetic-secret-value";
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["run", "test", "--project-dir"])
        .arg(&dir)
        .env("TAPID_ISSUE164_CHILD_SECRET", secret)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("registry credential"), "{stderr}");
    assert!(!stderr.contains(secret), "{stderr}");
    cleanup(dir);
}

#[test]
fn lifecycle_commands_are_exposed_as_cli_commands() {
    let dir = temp_dir("lifecycle-help");
    let output = run(&dir, &["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in ["add", "remove", "update", "outdated", "prune"] {
        assert!(
            stdout.contains(command),
            "missing {command} in help: {stdout}"
        );
    }
    let add_help = run(&dir, &["add", "--help"]);
    assert!(add_help.status.success());
    let add_help = String::from_utf8_lossy(&add_help.stdout);
    assert!(
        add_help.contains("Select workspace member by name"),
        "missing workspace selector guidance: {add_help}"
    );
    cleanup(dir);
}

#[test]
fn read_only_lifecycle_commands_fail_closed_without_writing() {
    let dir = temp_dir("lifecycle-read-only");
    let manifest = "{\"name\":\"demo\",\"version\":\"1.0.0\"}\n";
    fs::write(dir.join("package.json"), manifest).unwrap();
    let outdated = run(&dir, &["outdated"]);
    assert_eq!(outdated.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&outdated.stderr).contains("cannot read lockfile"));
    let prune = run(&dir, &["prune"]);
    assert_eq!(prune.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&prune.stderr).contains("requires tapid.lock"));
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert!(!dir.join("tapid.lock").exists());
    cleanup(dir);
}

#[test]
fn outdated_reports_versions_without_mutating_project_state() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let dir = temp_dir("outdated-read-only");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"is-char":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let package = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "is-char",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let key = package.key();
    lock.insert_package(package).unwrap();
    lock.set_roots([key]).unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    fs::create_dir_all(dir.join("node_modules/keep")).unwrap();
    fs::write(dir.join("node_modules/keep/sentinel"), "untouched").unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.1.0"},{"registry":"https://registry.npmjs.org","name":"is-char","version":"2.0.0"}]}"#,
    )
    .unwrap();
    let manifest_before = fs::read(dir.join("package.json")).unwrap();
    let lock_before = fs::read(dir.join("tapid.lock")).unwrap();
    let sentinel_before = fs::read(dir.join("node_modules/keep/sentinel")).unwrap();
    let entries_before = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let output = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "is-char [dependencies] declared=^1.0.0 locked=1.0.0 compatible=1.1.0 available=2.0.0"
    ));
    assert_eq!(fs::read(dir.join("package.json")).unwrap(), manifest_before);
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_before);
    assert_eq!(
        fs::read(dir.join("node_modules/keep/sentinel")).unwrap(),
        sentinel_before
    );
    let entries_after = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(entries_after, entries_before);
    cleanup(dir);
}

#[test]
fn outdated_reports_local_workspace_versions_without_registry_lookup() {
    let dir = temp_dir("outdated-workspace-local");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"local":"^1.0.0","local-star":"workspace:*"}}"#,
    )
    .unwrap();
    for (directory, name, version) in [
        ("local", "local", "1.2.0"),
        ("local-star", "local-star", "2.0.0"),
    ] {
        fs::create_dir_all(dir.join("packages").join(directory)).unwrap();
        fs::write(
            dir.join("packages").join(directory).join("package.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
    }
    let store = dir.join("store");
    let install = run(&dir, &["install", "--store-dir", store.to_str().unwrap()]);
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let lock_before = fs::read(dir.join("tapid.lock")).unwrap();
    let fixture = dir.join("registry.json");
    fs::write(&fixture, r#"{"packages":[]}"#).unwrap();

    let outdated = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );

    assert!(
        outdated.status.success(),
        "{}",
        String::from_utf8_lossy(&outdated.stderr)
    );
    let stdout = String::from_utf8_lossy(&outdated.stdout);
    assert!(stdout.contains(
        "local [dependencies] declared=^1.0.0 locked=1.2.0 compatible=1.2.0 available=1.2.0"
    ));
    assert!(stdout.contains(
        "local-star [dependencies] declared=workspace:* locked=2.0.0 compatible=2.0.0 available=2.0.0"
    ));
    assert!(!stdout.contains("registry metadata returned no versions"));
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_before);
    cleanup(dir);
}

#[test]
fn outdated_uses_configured_private_registry_identity() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let dir = temp_dir("outdated-private-registry");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"@acme/widget":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.acme.example'\n",
    )
    .unwrap();
    let mut lock = lock_for_manifest(manifest);
    let package = LockedPackage::new_with_provenance(
        "https://packages.acme.example",
        "@acme/widget",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let key = package.key();
    lock.insert_package(package).unwrap();
    lock.set_roots([key]).unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://packages.acme.example","name":"@acme/widget","version":"1.1.0"},{"registry":"https://packages.acme.example","name":"@acme/widget","version":"2.0.0"}]}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "@acme/widget [dependencies] declared=^1.0.0 locked=1.0.0 compatible=1.1.0 available=2.0.0"
    ));
    cleanup(dir);
}

#[test]
fn remove_resolves_remaining_dependencies_and_cleans_stale_materialization() {
    let dir = temp_dir("remove-re-resolve");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"removed":"1.0.0","keep":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    for name in ["removed", "keep"] {
        fs::create_dir_all(dir.join(format!("node_modules/{name}"))).unwrap();
        fs::write(
            dir.join(format!("node_modules/{name}/package.json")),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
    }
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"keep","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "remove",
            "removed",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let updated = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(!updated.contains("\"removed\""));
    assert!(updated.contains("\"keep\": \"1.0.0\""));
    assert!(!dir.join("node_modules/removed").exists());
    assert!(dir.join("node_modules/keep/package.json").is_file());
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert_eq!(lock["packages"].as_object().unwrap().len(), 1);

    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let replay = run(
        &dir,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(dir.join("node_modules/keep/package.json").is_file());
    assert!(!dir.join("node_modules/removed").exists());
    cleanup(dir);
}

#[test]
fn update_latest_preserves_overlapping_dependency_sections() {
    for (first_section, second_section, name, first_range, second_range, latest_range) in [
        (
            "devDependencies",
            "peerDependencies",
            "h3",
            "^1.0.0",
            ">=1.0.0",
            "*",
        ),
        (
            "dependencies",
            "optionalDependencies",
            "h3",
            "^1.0.0",
            ">=1.0.0",
            "*",
        ),
        (
            "devDependencies",
            "peerDependencies",
            "local",
            "npm:h3@^1.0.0",
            "npm:h3@>=1.0.0",
            "npm:h3@*",
        ),
    ] {
        for select_all in [false, true] {
            for latest in [false, true] {
                let project =
                    tapid_test_support::TempProject::new("update-overlapping-sections").unwrap();
                let dir = project.path().to_path_buf();
                let mut manifest = serde_json::json!({
                    "name": "demo", "version": "1.0.0",
                    "peerDependencies": {"unselected": "^4.0.0"},
                    "scripts": {"test": "node test.js"},
                    "customMetadata": ["preserved", 42],
                });
                manifest[first_section][name] = first_range.into();
                manifest[second_section][name] = second_range.into();
                project
                    .write("package.json", manifest.to_string().as_bytes())
                    .unwrap();
                let fixture = project
                    .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
                    .unwrap();
                let store = dir.join("store");
                let mut args = vec!["update"];
                if !select_all {
                    args.push(name);
                }
                if latest {
                    args.push("--latest");
                }
                args.extend([
                    "--store-dir",
                    store.to_str().unwrap(),
                    "--registry-fixture",
                    fixture.to_str().unwrap(),
                ]);

                let output = run(&dir, &args);
                assert!(
                    output.status.success(),
                    "{args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let updated_bytes = fs::read(dir.join("package.json")).unwrap();
                let updated: serde_json::Value = serde_json::from_slice(&updated_bytes).unwrap();
                assert_eq!(
                    updated[first_section][name],
                    if latest { latest_range } else { first_range }
                );
                assert_eq!(
                    updated[second_section][name],
                    if latest { latest_range } else { second_range }
                );
                assert_eq!(
                    updated["peerDependencies"]["unselected"],
                    if latest && select_all { "*" } else { "^4.0.0" }
                );
                assert_eq!(updated["scripts"], manifest["scripts"]);
                assert_eq!(updated["customMetadata"], manifest["customMetadata"]);
                let installed: serde_json::Value = serde_json::from_slice(
                    &fs::read(dir.join("node_modules").join(name).join("package.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(installed["version"], if latest { "2.0.0" } else { "1.0.0" });

                fs::remove_dir_all(dir.join("node_modules")).unwrap();
                let replay = run(
                    &dir,
                    &[
                        "install",
                        "--offline",
                        "--frozen",
                        "--store-dir",
                        store.to_str().unwrap(),
                    ],
                );
                assert!(
                    replay.status.success(),
                    "{}",
                    String::from_utf8_lossy(&replay.stderr)
                );
                assert_eq!(fs::read(dir.join("package.json")).unwrap(), updated_bytes);
            }
        }
    }
}

#[test]
fn update_latest_restores_overlapping_sections_after_install_failure() {
    for fail_activation in [false, true] {
        let project = tapid_test_support::TempProject::new("update-overlap-rollback").unwrap();
        let dir = project.path().to_path_buf();
        project.write("package.json", br#"{"name":"demo","version":"1.0.0","devDependencies":{"h3":"^1.0.0"},"peerDependencies":{"h3":">=1.0.0"}}"#).unwrap();
        let fixture = project
            .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
            .unwrap();
        let store = dir.join("store");
        let installed = run(
            &dir,
            &[
                "install",
                "--store-dir",
                store.to_str().unwrap(),
                "--registry-fixture",
                fixture.to_str().unwrap(),
            ],
        );
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        project
            .write("node_modules/KEEP", b"preserved active tree")
            .unwrap();
        let original_manifest = fs::read(dir.join("package.json")).unwrap();
        let original_lock = fs::read(dir.join("tapid.lock")).unwrap();
        let original_package = fs::read(dir.join("node_modules/h3/package.json")).unwrap();
        let original_marker = fs::read(dir.join(".tapid-managed")).unwrap();
        let tree_digests = || {
            fs::read_dir(store.join("trees"))
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (
                        path.file_name().unwrap().to_owned(),
                        tapid_archive::canonical_tree_digest(&path).unwrap(),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let original_trees = tree_digests();
        if !fail_activation {
            project
                .write("registry.json", br#"{"packages":[]}"#)
                .unwrap();
        }
        let args = [
            "update",
            "h3",
            "--latest",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ];
        let failed = if fail_activation {
            run_with_env(&dir, &args, "TAPID_TEST_FAIL_ACTIVATION", "1")
        } else {
            run(&dir, &args)
        };
        assert!(!failed.status.success());
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            original_manifest
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), original_lock);
        assert_eq!(
            fs::read(dir.join("node_modules/h3/package.json")).unwrap(),
            original_package
        );
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"preserved active tree"
        );
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            original_marker
        );
        assert_eq!(tree_digests(), original_trees);
    }
}

#[test]
fn update_preserves_ranges_unless_latest_is_requested() {
    let dir = temp_dir("update-range-behavior");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"is-char":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.1.0","integrity":"{integrity}","artifact":"{artifact}"}},{{"registry":"https://registry.npmjs.org","name":"is-char","version":"2.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let update = run(
        &dir,
        &[
            "update",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    let after_update = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(after_update.contains("\"is-char\": \"^1.0.0\""));
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.contains("is-char@1.1.0"))
    );

    let latest = run(
        &dir,
        &[
            "update",
            "--latest",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        latest.status.success(),
        "{}",
        String::from_utf8_lossy(&latest.stderr)
    );
    let after_latest = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(after_latest.contains("\"is-char\": \"*\""));
    let latest_lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        latest_lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.contains("is-char@2.0.0"))
    );

    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let replay = run(
        &dir,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    cleanup(dir);
}

#[test]
fn selected_member_add_links_sibling_without_registry_fallback() {
    let dir = temp_dir("workspace-member-local-add");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/web")).unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    let web_manifest = r#"{"name":"web","version":"1.0.0"}"#;
    fs::write(dir.join("packages/web/package.json"), web_manifest).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"local","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "add",
            "local@1.0.0",
            "--workspace",
            "web",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    let updated_web = fs::read_to_string(dir.join("packages/web/package.json")).unwrap();
    assert!(updated_web.contains("local"), "{updated_web}");
    assert!(dir.join("tapid.lock").is_file());
    assert!(!dir.join("packages/web/tapid.lock").exists());
    assert!(dir.join("node_modules/local/package.json").is_file());
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        lock["workspacePackages"]
            .as_object()
            .unwrap()
            .values()
            .any(|package| package["source"]["name"] == "local")
    );
    assert!(
        lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.contains("local@"))
    );
    cleanup(dir);
}

#[test]
fn lifecycle_workspace_selector_mutates_only_selected_member() {
    let dir = temp_dir("workspace-member-selection");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    let web_manifest = r#"{"name":"web","version":"1.0.0"}"#;
    let worker_manifest = r#"{"name":"worker","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/web")).unwrap();
    fs::create_dir_all(dir.join("packages/worker")).unwrap();
    fs::write(dir.join("packages/web/package.json"), web_manifest).unwrap();
    fs::write(dir.join("packages/worker/package.json"), worker_manifest).unwrap();

    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "add",
            "is-char@1.0.0",
            "--workspace",
            "web",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("packages/worker/package.json")).unwrap(),
        worker_manifest.as_bytes()
    );
    let updated_web = fs::read_to_string(dir.join("packages/web/package.json")).unwrap();
    assert!(
        updated_web.contains("is-char"),
        "selected workspace manifest was not updated; stdout={} stderr={}; manifest: {updated_web}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("tapid.lock").is_file());
    assert!(!dir.join("packages/web/tapid.lock").exists());
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(!dir.join("packages/worker/node_modules").exists());
    assert!(!dir.join("packages/web/node_modules").exists());

    let update = run(
        &dir,
        &[
            "update",
            "is-char",
            "--workspace",
            "web",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    for mode in ["--offline", "--frozen"] {
        if dir.join("node_modules").exists() {
            fs::remove_dir_all(dir.join("node_modules")).unwrap();
        }
        let replay = run(
            &dir,
            &[
                "install",
                mode,
                "--workspace",
                "web",
                "--store-dir",
                store.to_str().unwrap(),
            ],
        );
        assert!(
            replay.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert!(dir.join("node_modules/is-char/package.json").is_file());
        assert!(!dir.join("packages/web/node_modules").exists());
    }
    let prune = run(
        &dir,
        &[
            "prune",
            "--workspace",
            "web",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        prune.status.success(),
        "{}",
        String::from_utf8_lossy(&prune.stderr)
    );
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(!dir.join("packages/worker/node_modules").exists());
    assert!(!dir.join("packages/web/node_modules").exists());

    let selected_remove = run(
        &dir,
        &[
            "remove",
            "is-char",
            "--workspace",
            "web",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        selected_remove.status.success(),
        "{}",
        String::from_utf8_lossy(&selected_remove.stderr)
    );
    let web_after_remove = fs::read_to_string(dir.join("packages/web/package.json")).unwrap();
    assert!(!web_after_remove.contains("is-char"));
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("packages/worker/package.json")).unwrap(),
        worker_manifest.as_bytes()
    );
    assert!(!dir.join("node_modules/is-char").exists());
    assert!(!dir.join("packages/web/node_modules").exists());
    cleanup(dir);
}

#[test]
fn root_install_links_ordinary_workspace_dependencies_without_registry_resolution() {
    let dir = temp_dir("workspace-local-link");
    let root_manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"@example/ui":"^1.0.0","local-star":"workspace:*","local-caret":"workspace:^","local-tilde":"workspace:~","is-char":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    for (directory, name) in [
        ("ui", "@example/ui"),
        ("star", "local-star"),
        ("caret", "local-caret"),
        ("tilde", "local-tilde"),
    ] {
        fs::create_dir_all(dir.join("packages").join(directory)).unwrap();
        fs::write(
            dir.join("packages").join(directory).join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.2.0"}}"#),
        )
        .unwrap();
    }

    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let frozen = run(
        &dir,
        &[
            "install",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        frozen.status.success(),
        "frozen workspace replay failed: stdout={} stderr={}",
        String::from_utf8_lossy(&frozen.stdout),
        String::from_utf8_lossy(&frozen.stderr)
    );
    let offline = run(
        &dir,
        &[
            "install",
            "--offline",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        offline.status.success(),
        "offline workspace replay failed: stdout={} stderr={}",
        String::from_utf8_lossy(&offline.stdout),
        String::from_utf8_lossy(&offline.stderr)
    );

    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    assert_eq!(lock["lockfileVersion"], 7);
    assert!(
        lock["workspacePackages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| { key.contains("workspace:packages/ui:@example/ui@1.2.0") })
    );
    assert_eq!(
        lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.contains("@example/ui"))
            .count(),
        0,
        "workspace identity must never appear as a registry package"
    );
    let link = dir.join("node_modules/@example/ui");
    assert!(link.join("package.json").is_file());
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    for name in ["local-star", "local-caret", "local-tilde"] {
        let member_link = dir.join("node_modules").join(name);
        assert!(member_link.join("package.json").is_file());
        assert!(
            fs::symlink_metadata(member_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    let original_lock = fs::read(dir.join("tapid.lock")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"@example/ui","version":"1.3.0"}"#,
    )
    .unwrap();
    let changed_member = run(
        &dir,
        &[
            "install",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        !changed_member.status.success(),
        "frozen replay must reject changed workspace identity"
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), original_lock);
    assert!(link.join("package.json").is_file());
    cleanup(dir);
}

#[test]
fn workspace_member_private_registry_dependency_resolves_and_replays_offline() {
    let dir = temp_dir("workspace-private-route");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"@example/ui":"workspace:*"}}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"@example/ui","version":"1.0.0","dependencies":{"@acme/private":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\n",
    )
    .unwrap();
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://packages.example","name":"@acme/private","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "online install failed: {}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let member = lock["workspacePackages"]
        .as_object()
        .unwrap()
        .values()
        .find(|package| package["dependencies"].get("@acme/private").is_some())
        .expect("member dependency was not locked");
    let target = member["dependencies"]["@acme/private"].as_str().unwrap();
    assert!(target.starts_with("https://packages.example|@acme/private@1.0.0"));
    assert!(!target.starts_with("https://registry.npmjs.org|"));

    for mode in ["--frozen", "--offline"] {
        let replay = run(
            &dir,
            &["install", mode, "--store-dir", store.to_str().unwrap()],
        );
        assert!(
            replay.status.success(),
            "{mode} replay failed: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
    }
    cleanup(dir);
}

#[test]
fn workspace_member_jsr_dependency_uses_stripped_local_name_and_replays() {
    let dir = temp_dir("workspace-member-jsr-dependency");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    let member_dir = dir.join("packages/ui");
    fs::create_dir_all(&member_dir).unwrap();
    fs::write(
        member_dir.join("package.json"),
        r#"{"name":"@example/ui","version":"1.0.0","dependencies":{"jsr:@s/foo":"1.0.0"}}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        include_bytes!("fixtures/cross-registry-alias.json"),
    )
    .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "workspace member JSR dependency install failed: {}",
        String::from_utf8_lossy(&installed.stderr)
    );

    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let member = lock["workspacePackages"]
        .as_object()
        .unwrap()
        .values()
        .find(|package| package["source"]["name"] == "@example/ui")
        .expect("workspace member missing from lockfile");
    let target = member["dependencies"]["@s/foo"]
        .as_str()
        .expect("stripped JSR local name missing from member lock edges");
    assert!(target.starts_with("https://jsr.io|@s/foo@1.0.0|"));
    assert!(member["dependencies"].get("jsr:@s/foo").is_none());
    assert!(dir.join("node_modules/@s/foo/package.json").is_file());

    for mode in ["--frozen", "--offline"] {
        let replay = run(
            &dir,
            &["install", mode, "--store-dir", store.to_str().unwrap()],
        );
        assert!(
            replay.status.success(),
            "{mode} replay failed: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert!(dir.join("node_modules/@s/foo/package.json").is_file());
    }
    cleanup(dir);
}

#[test]
fn workspace_member_npm_alias_uses_private_scope_and_replays() {
    let dir = temp_dir("workspace-member-private-alias");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"@example/ui":"workspace:*"}}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"@example/ui","version":"1.0.0","dependencies":{"private-alias":"npm:@acme/private@1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\n",
    )
    .unwrap();
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://packages.example","name":"@acme/private","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "online install failed: {}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let member = lock["workspacePackages"]
        .as_object()
        .unwrap()
        .values()
        .find(|package| package["source"]["name"] == "@example/ui")
        .expect("workspace member was not locked");
    let target = member["dependencies"]["private-alias"].as_str().unwrap();
    assert!(target.starts_with("https://packages.example|@acme/private@1.0.0"));
    assert_eq!(
        member["dependencyAliases"]["private-alias"],
        "@acme/private"
    );
    assert!(
        dir.join("node_modules/private-alias/package.json")
            .is_file()
    );

    for mode in ["--frozen", "--offline"] {
        let replay = run(
            &dir,
            &["install", mode, "--store-dir", store.to_str().unwrap()],
        );
        assert!(
            replay.status.success(),
            "{mode} replay failed: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert!(
            dir.join("node_modules/private-alias/package.json")
                .is_file()
        );
    }
    cleanup(dir);
}

#[test]
fn workspace_member_bin_is_materialized_as_a_node_modules_bin_shim() {
    let dir = temp_dir("workspace-member-bin");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    let member = dir.join("packages/tool");
    fs::create_dir_all(member.join("bin")).unwrap();
    fs::write(
        member.join("package.json"),
        r#"{"name":"tool","version":"1.0.0","bin":{"workspace-tool":"bin/tool.js"}}"#,
    )
    .unwrap();
    let bin_source = member.join("bin/tool.js");
    fs::write(
        &bin_source,
        "#!/usr/bin/env node\\nconsole.log('workspace tool');\\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin_source, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(unix)]
    let mode_before = fs::metadata(&bin_source).unwrap().permissions();

    let install = run(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );

    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let shim = dir.join("node_modules/.bin/workspace-tool");
    #[cfg(unix)]
    {
        assert!(
            fs::symlink_metadata(&shim)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::canonicalize(&shim).unwrap(),
            fs::canonicalize(&bin_source).unwrap()
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&bin_source).unwrap().permissions().mode(),
            mode_before.mode(),
            "install must not change workspace source permissions"
        );
    }
    #[cfg(windows)]
    {
        assert!(shim.with_extension("cmd").is_file());
        assert!(shim.with_extension("ps1").is_file());
    }
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn workspace_bin_target_cannot_escape_member_through_symlinked_directory() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = temp_dir("workspace-bin-symlink-escape");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    let member = dir.join("packages/tool");
    fs::create_dir_all(&member).unwrap();
    fs::write(
        member.join("package.json"),
        r#"{"name":"tool","version":"1.0.0","bin":{"workspace-tool":"bin/tool.js"}}"#,
    )
    .unwrap();
    let outside = dir.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let outside_bin = outside.join("tool.js");
    fs::write(&outside_bin, "#!/usr/bin/env node\\n").unwrap();
    fs::set_permissions(&outside_bin, fs::Permissions::from_mode(0o755)).unwrap();
    symlink("../../outside", member.join("bin")).unwrap();

    let install = run(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );

    assert!(!install.status.success());
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(stderr.to_lowercase().contains("escape"), "{stderr}");
    assert!(!dir.join("node_modules").exists());
    assert!(!dir.join("tapid.lock").exists());
    assert_eq!(fs::read(&outside_bin).unwrap(), b"#!/usr/bin/env node\\n");
    cleanup(dir);
}

#[test]
fn registry_and_workspace_bin_name_collisions_fail_before_project_activation() {
    let dir = temp_dir("workspace-bin-collision");
    let root_manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"registry-tool":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    let member = dir.join("packages/tool");
    fs::create_dir_all(member.join("bin")).unwrap();
    fs::write(
        member.join("package.json"),
        r#"{"name":"workspace-tool","version":"1.0.0","bin":{"workspace-tool":"bin/tool.js"}}"#,
    )
    .unwrap();
    let workspace_bin = member.join("bin/tool.js");
    fs::write(&workspace_bin, "#!/usr/bin/env node\\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&workspace_bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let artifact = "base64:H4sIAAAAAAAC/+3U0UrDMBTG8VzvKbp4ozDbpHQV5tPUGUpdlzOSbiJj727WyRDFOx2K/9/NSU4CuUi+bJrlqmldsTnV/CmKV9/MJHVVjTX5WI0ty/N47Ftbl1ZlRl3ANg5NSMer/2mvfbN2eqGDa7s4hJfbQaTXM71zIXbi04rNTW5S56FLs71+lrCK6bm4087FsV8ch+nt6MNhovCHvOW+eHeJ6ifyfzeff5l/U3/Kf1VW5P8SrqbFNobx+p3fZV4e3WQpPkrv8l7a6/O/kI1pv7kn3wAAAAAAAAAAAAAAAL/KKzpM+8QAKAAA";
    let integrity = "sha512-hO6/Tzfzkn58mEikJ1krKMoh6fmhGtzU2wSPIUsSCNt0CCxy3/b3o3NeuhjbSM9QTSkcBS34A2tH0bRVflGcgQ==";
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"registry-tool","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let install = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(!install.status.success());
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(stderr.to_lowercase().contains("collision"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(fs::read(&workspace_bin).unwrap(), b"#!/usr/bin/env node\\n");
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    cleanup(dir);
}

#[test]
fn root_workspace_activation_failure_restores_manifest_lock_and_managed_tree() {
    let dir = temp_dir("workspace-activation-rollback");
    let manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"local":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), b"previous lock bytes\n").unwrap();
    fs::create_dir(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/KEEP"), b"user data").unwrap();
    fs::write(dir.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();

    let output = run_with_env(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
        "TAPID_TEST_FAIL_ACTIVATION",
        "1",
    );
    assert!(!output.status.success());
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"previous lock bytes\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(!dir.join("node_modules/local").exists());
    cleanup(dir);
}

#[test]
fn root_workspace_rejects_member_inside_node_modules_without_mutation() {
    let dir = temp_dir("workspace-member-in-node-modules");
    let manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["node_modules/local"],"dependencies":{"local":"workspace:*"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let member_dir = dir.join("node_modules/local");
    fs::create_dir_all(&member_dir).unwrap();
    let member_manifest = r#"{"name":"local","version":"1.0.0"}"#;
    fs::write(member_dir.join("package.json"), member_manifest).unwrap();
    fs::write(dir.join("tapid.lock"), b"previous lock bytes\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), b"previous active tree").unwrap();
    fs::write(dir.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
    let store = dir.join("store");

    let output = run(&dir, &["install", "--store-dir", store.to_str().unwrap()]);

    assert!(!output.status.success());
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"previous lock bytes\n"
    );
    assert_eq!(
        fs::read(member_dir.join("package.json")).unwrap(),
        member_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"previous active tree"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn root_workspace_lifecycle_add_update_remove_prune_stays_local() {
    let dir = temp_dir("workspace-root-lifecycle");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    let store = dir.join("store");
    let add = run(
        &dir,
        &[
            "add",
            "local@workspace:*",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        fs::symlink_metadata(dir.join("node_modules/local"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let update = run(
        &dir,
        &["update", "local", "--store-dir", store.to_str().unwrap()],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert!(
        fs::symlink_metadata(dir.join("node_modules/local"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let prune = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);
    assert!(
        prune.status.success(),
        "{}",
        String::from_utf8_lossy(&prune.stderr)
    );
    assert!(
        fs::symlink_metadata(dir.join("node_modules/local"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let remove = run(
        &dir,
        &["remove", "local", "--store-dir", store.to_str().unwrap()],
    );
    assert!(
        remove.status.success(),
        "{}",
        String::from_utf8_lossy(&remove.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("package.json")).unwrap()).unwrap();
    assert!(manifest["dependencies"].get("local").is_none());
    cleanup(dir);
}

#[test]
fn root_workspace_direct_dependency_override_must_match_the_declared_range() {
    let dir = temp_dir("workspace-direct-override");
    let root_manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"local":"^1.0.0"},"overrides":{"local":"2.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), "prior lock\\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "existing managed state").unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported direct dependency override for 'local'"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), b"prior lock\\n");
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"existing managed state"
    );
    assert!(!dir.join(".tapid-activation.lock").exists());
    cleanup(dir);
}

#[test]
fn workspace_member_registry_dependencies_use_root_overrides_in_install_and_replay() {
    let dir = temp_dir("workspace-member-root-override");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"overrides":{"is-char":"1.0.0"}}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"ui","version":"1.0.0","dependencies":{"is-char":"^2.0.0"}}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let install = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let member = lock["workspacePackages"]
        .as_object()
        .unwrap()
        .values()
        .find(|package| package["source"]["name"] == "ui")
        .unwrap();
    assert!(
        member["dependencies"]["is-char"]
            .as_str()
            .unwrap()
            .contains("is-char@1.0.0")
    );
    for flag in ["--frozen", "--offline"] {
        let replay = run(
            &dir,
            &["install", flag, "--store-dir", store.to_str().unwrap()],
        );
        assert!(
            replay.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
    }
    cleanup(dir);
}

#[test]
fn selected_member_install_uses_root_lock_and_activates_the_workspace_graph() {
    let dir = temp_dir("selected-workspace-install");
    let root_manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["apps/*","packages/*"],"dependencies":{"is-char":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("apps/news")).unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("apps/news/package.json"),
        r#"{"name":"news","version":"1.0.0","dependencies":{"@example/ui":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"@example/ui","version":"1.0.0"}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    let mut fixture_packages: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/aliased-root-cycle.json")).unwrap();
    fixture_packages["packages"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "registry": "https://registry.npmjs.org",
            "name": "is-char",
            "version": "1.0.0",
            "integrity": integrity,
            "artifact": artifact,
        }));
    fs::write(&fixture, serde_json::to_vec(&fixture_packages).unwrap()).unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "install",
            "a@1.0.0",
            "--workspace",
            "news",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root_after = fs::read(dir.join("package.json")).unwrap();
    assert_eq!(root_after, root_manifest.as_bytes());
    let member_after: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("apps/news/package.json")).unwrap()).unwrap();
    assert_eq!(member_after["dependencies"]["a"], "1.0.0");

    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        lock["workspacePackages"]
            .as_object()
            .unwrap()
            .values()
            .any(|package| package["source"]["name"] == "@example/ui")
    );
    assert!(lock["rootBindings"]["is-char"].is_string());
    assert!(lock["rootBindings"].get("a").is_none());
    assert!(dir.join("node_modules/@example/ui/package.json").is_file());
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(dir.join("node_modules/a/package.json").is_file());
    assert!(!dir.join("apps/news/tapid.lock").exists());
    assert!(!dir.join("apps/news/node_modules").exists());

    let replay = run(
        &dir,
        &[
            "install",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "frozen root replay: {}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(dir.join("node_modules/a/package.json").is_file());
    cleanup(dir);
}

#[test]
fn root_install_resolves_registry_dependencies_declared_by_workspace_members() {
    let dir = temp_dir("workspace-member-registry-dependency-positive");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"ui","version":"1.0.0","dependencies":{"is-char":"^1.0.0"}}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/worker")).unwrap();
    fs::write(
        dir.join("packages/worker/package.json"),
        r#"{"name":"worker","version":"1.0.0","dependencies":{"is-char":"^1.0.0"}}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}","dependencies":{{"subdep":"^1.0.0"}}}},{{"registry":"https://registry.npmjs.org","name":"subdep","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let workspace_packages = lock["workspacePackages"].as_object().unwrap();
    assert_eq!(workspace_packages.len(), 2);
    for name in ["ui", "worker"] {
        let workspace_package = workspace_packages
            .values()
            .find(|package| package["source"]["name"] == name)
            .unwrap();
        assert!(
            workspace_package["dependencies"]["is-char"]
                .as_str()
                .unwrap()
                .contains("is-char@1.0.0")
        );
        assert!(
            dir.join(format!("node_modules/{name}/package.json"))
                .is_file()
        );
    }
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(dir.join("node_modules/subdep/package.json").is_file());
    for flag in ["--frozen", "--offline"] {
        let replay = run(
            &dir,
            &["install", flag, "--store-dir", store.to_str().unwrap()],
        );
        assert!(
            replay.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert!(
            dir.join("node_modules/is-char/package.json").is_file(),
            "{flag} replay dropped workspace member's registry dependency"
        );
    }
    let lock_path = dir.join("tapid.lock");
    let mut tampered_lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    let workspace_package = tampered_lock["workspacePackages"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .find(|package| package["source"]["name"] == "ui")
        .unwrap();
    workspace_package["dependencies"]
        .as_object_mut()
        .unwrap()
        .remove("is-char");
    let tampered_bytes = serde_json::to_vec(&tampered_lock).unwrap();
    fs::write(&lock_path, &tampered_bytes).unwrap();
    let tampered_replay = run(
        &dir,
        &[
            "install",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!tampered_replay.status.success());
    assert_eq!(fs::read(&lock_path).unwrap(), tampered_bytes);
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    cleanup(dir);
}

#[test]
fn root_workspace_member_peer_uses_a_direct_root_provider() {
    let dir = temp_dir("workspace-member-peer-provider");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"is-char":"^1.0.0"}}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"ui","version":"1.0.0","peerDependencies":{"is-char":"^1.0.0","theme":"workspace:*"}}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/theme")).unwrap();
    fs::write(
        dir.join("packages/theme/package.json"),
        r#"{"name":"theme","version":"1.0.0"}"#,
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#),
    )
    .unwrap();
    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let workspace_lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let workspace_package = workspace_lock["workspacePackages"]
        .as_object()
        .unwrap()
        .values()
        .find(|package| package["source"]["name"] == "ui")
        .unwrap();
    assert!(workspace_package.get("dependencies").is_none());
    let replay = run(
        &dir,
        &[
            "install",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(dir.join("node_modules/theme/package.json").is_file());
    cleanup(dir);
}

#[test]
fn workspace_member_peer_dependencies_fail_before_project_mutation() {
    let dir = temp_dir("workspace-member-registry-dependency");
    let root_manifest =
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/ui")).unwrap();
    fs::write(
        dir.join("packages/ui/package.json"),
        r#"{"name":"ui","version":"1.0.0","peerDependencies":{"is-char":"^1.0.0"}}"#,
    )
    .unwrap();
    let previous_lock = prior_empty_lock();
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("workspace member peer dependency 'is-char' has no direct root provider"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        previous_lock.as_bytes()
    );
    assert!(!dir.join("node_modules").exists());
    cleanup(dir);
}

#[test]
fn root_install_links_workspace_cycles_without_registry_resolution() {
    let dir = temp_dir("workspace-cycle");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("packages/a")).unwrap();
    fs::create_dir_all(dir.join("packages/b")).unwrap();
    fs::write(
        dir.join("packages/a/package.json"),
        r#"{"name":"a","version":"1.0.0","dependencies":{"b":"^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("packages/b/package.json"),
        r#"{"name":"b","version":"1.0.0","dependencies":{"a":"^1.0.0"}}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    let packages = lock["workspacePackages"].as_object().unwrap();
    let first = packages
        .iter()
        .find(|(key, _)| key.contains("workspace:packages/a:a@1.0.0"))
        .unwrap();
    let second = packages
        .iter()
        .find(|(key, _)| key.contains("workspace:packages/b:b@1.0.0"))
        .unwrap();
    assert!(
        first.1["dependencies"]["b"]
            .as_str()
            .unwrap()
            .contains("workspace:packages/b:b@1.0.0")
    );
    assert!(
        second.1["dependencies"]["a"]
            .as_str()
            .unwrap()
            .contains("workspace:packages/a:a@1.0.0")
    );
    assert!(dir.join("node_modules/a/package.json").is_file());
    assert!(dir.join("node_modules/b/package.json").is_file());
    let replay = run(
        &dir,
        &[
            "install",
            "--frozen",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    cleanup(dir);
}

#[test]
fn workspace_protocol_add_fails_closed_before_registry_or_project_mutation() {
    use tapid_store::Store;

    let dir = temp_dir("workspace-protocol-fail-closed");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), "old lock bytes\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();

    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let prior_source = dir.join("prior-store-tree");
    fs::create_dir_all(&prior_source).unwrap();
    fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
    let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store
        .activate_verified_tree(&prior_digest, &prior_source)
        .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"local","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "add",
            "local@workspace:1.0.0",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported workspace protocol 'workspace:1.0.0'")
            && stderr.contains(
                "supported compatibility forms are workspace:*, workspace:^, and workspace:~"
            ),
        "{stderr}"
    );
    assert!(
        !stderr.contains("registry"),
        "unexpected registry fallback: {stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"old lock bytes\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(store.verified_tree_path(&prior_digest).is_ok());
    cleanup(dir);
}

#[test]
fn workspace_member_under_root_node_modules_is_rejected_before_mutation() {
    let dir = temp_dir("workspace-node-modules-member");
    let root_manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["node_modules/*"],"dependencies":{"local":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("node_modules/local")).unwrap();
    fs::write(
        dir.join("node_modules/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("node_modules/local/KEEP"), "workspace source").unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("tapid.lock"), "prior lock bytes\\n").unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workspace member may not be inside root node_modules"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"prior lock bytes\\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/local/package.json")).unwrap(),
        br#"{"name":"local","version":"1.0.0"}"#
    );
    assert_eq!(
        fs::read(dir.join("node_modules/local/KEEP")).unwrap(),
        b"workspace source"
    );
    assert!(
        !fs::symlink_metadata(dir.join("node_modules/local"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dir.join(".tapid-activation.lock").exists());
    cleanup(dir);
}

#[test]
fn install_rejects_unsupported_workspace_manifest_before_mutating_project_or_store() {
    use std::collections::BTreeSet;
    use tapid_store::Store;

    let dir = temp_dir("workspace-manifest-install-fail-closed");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"local":"workspace:1.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), "old lock bytes\\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();

    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let prior_source = dir.join("prior-store-tree");
    fs::create_dir_all(&prior_source).unwrap();
    fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
    let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store
        .activate_verified_tree(&prior_digest, &prior_source)
        .unwrap();
    let store_entries_before = fs::read_dir(&store_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<BTreeSet<_>>();

    let output = run(
        &dir,
        &["install", "--store-dir", store_dir.to_str().unwrap()],
    );

    assert!(!output.status.success());
    assert!(
        !dir.join(".tapid-activation.lock").exists(),
        "rejected manifest must not create activation state"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported workspace protocol 'workspace:1.0.0'")
            && stderr.contains(
                "supported compatibility forms are workspace:*, workspace:^, and workspace:~"
            ),
        "{stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"old lock bytes\\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\\n"
    );
    assert!(!dir.join(".tapid-activation.lock").exists());
    assert_eq!(
        fs::read_dir(&store_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<BTreeSet<_>>(),
        store_entries_before
    );
    assert!(store.verified_tree_path(&prior_digest).is_ok());
    cleanup(dir);
}

#[test]
fn install_rejects_root_registry_identity_collision_before_mutation() {
    let dir = temp_dir("root-registry-identity-collision");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"@s/foo":"1.0.0","jsr:@s/foo":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let previous_lock = b"previous lock bytes\\n";
    fs::write(dir.join("tapid.lock"), previous_lock).unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        include_bytes!("fixtures/cross-registry-alias.json"),
    )
    .unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("conflicting package identities for local dependency '@s/foo'"),
        "unexpected error: {stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), previous_lock);
    assert!(!store.exists(), "rejected install left store state behind");
    assert!(!dir.join("node_modules").exists());
    cleanup(dir);
}

#[test]
fn install_rejects_workspace_member_registry_identity_collision_before_mutation() {
    let dir = temp_dir("workspace-member-root-identity-collision");
    let root_manifest = r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"@actual/scoped":"1.2.0"}}"#;
    let member_manifest = r#"{"name":"@example/ui","version":"1.0.0","dependencies":{"@actual/scoped":"npm:@acme/private@1.0.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    let member_dir = dir.join("packages/ui");
    fs::create_dir_all(&member_dir).unwrap();
    fs::write(member_dir.join("package.json"), member_manifest).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\n",
    )
    .unwrap();
    let mut fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
    fixture["packages"].as_array_mut().unwrap().push(serde_json::json!({
        "registry": "https://packages.example",
        "name": "@acme/private",
        "version": "1.0.0",
        "integrity": "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==",
        "artifact": "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA"
    }));
    let fixture_path = dir.join("registry.json");
    fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let previous_lock = b"previous lock bytes\\n";
    fs::write(dir.join("tapid.lock"), previous_lock).unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("conflicting package identities for local dependency '@actual/scoped'"),
        "unexpected error: {stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(member_dir.join("package.json")).unwrap(),
        member_manifest.as_bytes()
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), previous_lock);
    assert!(!store.exists(), "rejected install left store state behind");
    assert!(!dir.join("node_modules").exists());
    cleanup(dir);
}

#[test]
fn npm_and_jsr_registry_identities_remain_distinct_for_related_packages() {
    let dir = temp_dir("npm-jsr-distinct-identities");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"is-char":"1.0.0","jsr:@arvid/is-char":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}},{{"registry":"https://jsr.io","name":"@arvid/is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let packages = lock["packages"].as_object().unwrap();
    assert_eq!(packages.len(), 2);
    assert!(
        packages
            .keys()
            .any(|key| key.starts_with("https://registry.npmjs.org|is-char@1.0.0|"))
    );
    assert!(
        packages
            .keys()
            .any(|key| key.starts_with("https://jsr.io|@arvid/is-char@1.0.0|"))
    );
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(
        dir.join("node_modules/@arvid/is-char/package.json")
            .is_file()
    );
    cleanup(dir);
}

#[test]
fn lifecycle_add_rolls_back_manifest_when_resolution_fails() {
    use tapid_store::Store;

    let dir = temp_dir("lifecycle-rollback");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let previous_lock = prior_empty_lock();
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();
    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let source = dir.join("prior-store-tree");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), "prior store tree").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store.activate_verified_tree(&digest, &source).unwrap();
    let trees_before = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
                r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"unrelated","version":"1.0.0","integrity":"sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==","artifact":"base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA"}]}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "add",
            "is-char",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("resolution failed"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        previous_lock.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(store.verified_tree_path(&digest).is_ok());
    let trees_after = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(trees_after, trees_before);
    cleanup(dir);
}

#[test]
fn lifecycle_add_rejects_invalid_fetched_range_without_state_changes() {
    let dir = temp_dir("lifecycle-invalid-fetched-range");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let previous_lock = prior_empty_lock();
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","artifact":"unused","dependencies":{"broken":"not a valid range"}}]}"#,
    )
    .unwrap();
    let store = dir.join("previously-nonexistent-store");

    let output = run(
        &dir,
        &[
            "add",
            "parent@1.0.0",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency broken"), "{stderr}");
    assert!(stderr.contains("not a valid range"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        previous_lock.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(!store.exists());
    assert!(!dir.join(".tapid-lifecycle-journal.json").exists());
    cleanup(dir);
}

#[test]
fn add_peer_records_only_peer_requirement() {
    let dir = temp_dir("peer-cli-transaction");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    let fixture = dir.join("registry.json");
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://jsr.io","name":"@scope/peer","version":"1.0.0","artifact":"https://jsr.io/@scope/peer/1.0.0.tgz"}]}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "add",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
            "react@^18.0.0",
            "--peer",
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let updated = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(updated.contains("\"peerDependencies\""));
    assert!(updated.contains("\"react\": \"^18.0.0\""));
    assert!(!updated.contains("\"dependencies\""));
    assert!(dir.join("tapid.lock").is_file());
    cleanup(dir);
}

#[test]
fn install_validates_peer_providers_and_persists_peer_context() {
    let dir = temp_dir("peer-context-install");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0","react":"18.2.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}","peerDependencies":{{"react":"^18.0.0"}}}},{{"registry":"https://registry.npmjs.org","name":"react","version":"18.2.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let packages = lock["packages"].as_object().unwrap();
    assert!(
        packages
            .keys()
            .any(|key| { key.contains("|plugin@1.0.0|peer=name=react;version=18.2.0|") })
    );
    assert!(
        packages
            .keys()
            .any(|key| key.contains("|react@18.2.0|peer=-|"))
    );
    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let replay = run(
        &dir,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replayed_lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        replayed_lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| { key.contains("|plugin@1.0.0|peer=name=react;version=18.2.0|") })
    );
    cleanup(dir);
}

#[test]
fn install_rolls_back_when_a_required_peer_provider_is_missing() {
    use tapid_store::Store;

    let dir = temp_dir("peer-context-rollback");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let previous_lock = prior_empty_lock();
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/sentinel"), "keep").unwrap();
    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let source = dir.join("prior-store-tree");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), "prior store tree").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store.activate_verified_tree(&digest, &source).unwrap();
    let trees_before = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","artifact":"{artifact}","peerDependencies":{{"react":"^18.0.0"}}}}]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("peer dependency"));
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        previous_lock
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/sentinel")).unwrap(),
        "keep"
    );
    assert!(store.verified_tree_path(&digest).is_ok());
    let trees_after = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(trees_after, trees_before);
    cleanup(dir);
}

#[test]
fn invalid_fetched_dependency_range_does_not_create_store_state() {
    let dir = temp_dir("invalid-fetched-range-no-store");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","artifact":"unused","dependencies":{"broken":"not a valid range"}}]}"#,
    )
    .unwrap();
    let store = dir.join("previously-nonexistent-store");

    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency broken"), "{stderr}");
    assert!(stderr.contains("not a valid range"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn invalid_online_root_range_does_not_create_store_state() {
    let dir = temp_dir("invalid-range-no-store");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"broken":"not a valid range"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let store = dir.join("previously-nonexistent-store");

    let output = run(&dir, &["install", "--store-dir", store.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("broken"), "{stderr}");
    assert!(stderr.contains("not a valid range"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn install_rolls_back_when_a_required_peer_is_incompatible() {
    let dir = temp_dir("peer-context-incompatible-rollback");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0","react":"18.2.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let previous_lock = prior_empty_lock();
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/sentinel"), "keep").unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","artifact":"{artifact}","peerDependencies":{{"react":"^19.0.0"}}}},{{"registry":"https://registry.npmjs.org","name":"react","version":"18.2.0","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("peer dependency"));
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        previous_lock
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/sentinel")).unwrap(),
        "keep"
    );
    cleanup(dir);
}

fn run_lifecycle_recovery_crash_case(crash_point: &str) {
    use tapid_store::Store;

    let dir = temp_dir("lifecycle-crash-recovery");
    let original_manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), original_manifest).unwrap();
    let original_lock = lock_for_manifest(original_manifest).to_json().unwrap();
    fs::write(dir.join("tapid.lock"), &original_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "previous activation").unwrap();

    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let prior_source = dir.join("prior-store-tree");
    fs::create_dir_all(&prior_source).unwrap();
    fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
    let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store
        .activate_verified_tree(&prior_digest, &prior_source)
        .unwrap();
    let trees_before = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let crash = run_with_env(
        &dir,
        &[
            "add",
            "plugin@1.0.0",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
        "TAPID_TEST_CRASH_POINT",
        crash_point,
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            crash.status.signal(),
            Some(6),
            "crash hook did not abort: {}",
            String::from_utf8_lossy(&crash.stderr)
        );
    }
    #[cfg(not(unix))]
    assert!(
        !crash.status.success(),
        "crash hook did not terminate process"
    );

    let recovery = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        recovery.status.success(),
        "recovery failed: {}",
        String::from_utf8_lossy(&recovery.stderr)
    );
    let trees_after = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    if crash_point == "commit_decision" {
        assert_ne!(
            fs::read(dir.join("package.json")).unwrap(),
            original_manifest.as_bytes()
        );
        assert_ne!(
            fs::read(dir.join("tapid.lock")).unwrap(),
            original_lock.as_bytes()
        );
        assert!(!dir.join("node_modules/KEEP").exists());
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        assert!(trees_after.len() > trees_before.len());
        assert!(!dir.join(".tapid-lifecycle-journal.json").exists());
        assert!(fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tapid-node-modules-old-")
        }));
    } else {
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            original_manifest.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("tapid.lock")).unwrap(),
            original_lock.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"previous activation"
        );
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        assert_eq!(trees_after, trees_before);
    }
    cleanup(dir);
}

#[test]
fn lifecycle_recovers_previous_state_after_node_modules_backup_crash() {
    run_lifecycle_recovery_crash_case("node_modules_backed_up");
}

#[test]
fn lifecycle_recovers_previous_state_after_store_publication_crash() {
    run_lifecycle_recovery_crash_case("store_published");
}

#[test]
fn lifecycle_recovers_previous_state_after_lockfile_replacement_crash() {
    run_lifecycle_recovery_crash_case("lockfile_replaced");
}

#[test]
fn lifecycle_recovers_previous_state_after_activation_completion_crash() {
    run_lifecycle_recovery_crash_case("activation_complete");
}

#[test]
fn lifecycle_finishes_committed_state_after_commit_decision_crash() {
    run_lifecycle_recovery_crash_case("commit_decision");
}

#[test]
fn install_does_not_commit_verified_store_trees_when_materialization_fails() {
    let dir = temp_dir("store-rollback-on-activation");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let previous_lock = prior_empty_lock();
    fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();
    let fixture = dir.join("registry.json");
    let store = dir.join("store");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unmarked node_modules"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        previous_lock
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/KEEP")).unwrap(),
        "user data"
    );
    assert!(!store.join("trees").exists());
    cleanup(dir);
}

#[test]
fn install_preserves_project_and_store_on_integrity_and_archive_failures() {
    use tapid_store::Store;

    for failure in ["integrity", "archive"] {
        let dir = temp_dir(&format!("rollback-{failure}"));
        let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
        fs::write(dir.join("package.json"), manifest).unwrap();
        let previous_lock = prior_empty_lock();
        fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
        fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();

        let store_dir = dir.join("store");
        let store = Store::new(&store_dir);
        let prior_source = dir.join("prior-store-tree");
        fs::create_dir_all(&prior_source).unwrap();
        fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
        let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
            .unwrap()
            .parse::<tapid_core::ArtifactDigest>()
            .unwrap();
        store
            .activate_verified_tree(&prior_digest, &prior_source)
            .unwrap();
        let trees_before = fs::read_dir(store_dir.join("trees"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();

        let archive = if failure == "integrity" {
            STANDARD.decode("H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA").unwrap()
        } else {
            b"not a gzip tar archive".to_vec()
        };
        let integrity = if failure == "integrity" {
            format!("sha512-{}", STANDARD.encode([0_u8; 64]))
        } else {
            format!("sha512-{}", STANDARD.encode(Sha512::digest(&archive)))
        };
        let fixture = dir.join("registry.json");
        fs::write(
            &fixture,
            format!(
                r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"base64:{}"}}]}}"#,
                STANDARD.encode(&archive)
            ),
        )
        .unwrap();

        let output = run(
            &dir,
            &[
                "install",
                "--store-dir",
                store_dir.to_str().unwrap(),
                "--registry-fixture",
                fixture.to_str().unwrap(),
            ],
        );

        assert!(!output.status.success(), "{failure} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if failure == "integrity" {
            assert!(output.stdout.is_empty());
            assert!(
                stderr.contains("Install failed during artifact verification."),
                "{stderr}"
            );
            assert!(stderr.contains("plugin"), "{stderr}");
            assert!(stderr.contains("integrity"), "unexpected error: {stderr}");
        } else {
            assert!(
                stderr.contains("invalid gzip header"),
                "unexpected error: {stderr}"
            );
        }
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("tapid.lock")).unwrap(),
            previous_lock.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"user data"
        );
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        assert!(store.verified_tree_path(&prior_digest).is_ok());
        let trees_after = fs::read_dir(store_dir.join("trees"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            trees_after, trees_before,
            "store changed after {failure} failure"
        );
        cleanup(dir);
    }
}

#[test]
fn upgrade_is_exposed_as_a_cli_command() {
    let dir = temp_dir("upgrade-exposed");
    let output = run(&dir, &["--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("upgrade"));
    cleanup(dir);
}

fn lock_for_manifest(raw: &str) -> Lockfile {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = format!("sha256-{}", hex::encode(hasher.finalize()));
    Lockfile::new(&digest).unwrap()
}

fn write_prune_fixture(dir: &Path, manifest: &str, mismatch: bool) -> (PathBuf, String, String) {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    use tapid_store::Store;

    fs::write(dir.join("package.json"), manifest).unwrap();
    let store_root = dir.join("store");
    let store = Store::new(&store_root);
    let make_tree = |name: &str| {
        let source = dir.join(format!("source-{name}"));
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
        let digest = tapid_archive::canonical_tree_digest(&source).unwrap();
        let parsed = digest.parse::<tapid_core::ArtifactDigest>().unwrap();
        store.activate_verified_tree(&parsed, &source).unwrap();
        digest
    };
    let required_digest = make_tree("required");
    let orphan_digest = make_tree("orphan");
    let mut lock = lock_for_manifest(if mismatch {
        r#"{"name":"app","version":"1.0.0","dependencies":{"required":"2.0.0"}}"#
    } else {
        manifest
    });
    let required = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "required",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &required_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let orphan = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "orphan",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &orphan_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let required_key = required.key();
    lock.insert_package(required).unwrap();
    lock.insert_package(orphan).unwrap();
    lock.set_roots([required_key]).unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    (store_root, required_digest, orphan_digest)
}

#[test]
fn prune_preserves_workspace_member_peer_provider_and_removes_orphan() {
    use tapid_lockfile::{LockedPackage, Lockfile, RegistryIntegrityProvenance};
    use tapid_store::Store;

    let dir = temp_dir("prune-workspace-peer");
    let root_manifest =
        r#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#;
    let member_manifest =
        r#"{"name":"web","version":"1.0.0","peerDependencies":{"theme":"workspace:*"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    let member = dir.join("packages/web");
    fs::create_dir_all(&member).unwrap();
    fs::write(member.join("package.json"), member_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/theme")).unwrap();
    fs::write(
        dir.join("packages/theme/package.json"),
        r#"{"name":"theme","version":"1.0.0"}"#,
    )
    .unwrap();
    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let install = run(
        &dir,
        &["install", "--store-dir", store_dir.to_str().unwrap()],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );

    let source = dir.join("source-orphan");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("package.json"),
        r#"{"name":"orphan","version":"1.0.0"}"#,
    )
    .unwrap();
    let orphan_digest = tapid_archive::canonical_tree_digest(&source).unwrap();
    let parsed_digest = orphan_digest.parse::<tapid_core::ArtifactDigest>().unwrap();
    store
        .activate_verified_tree(&parsed_digest, &source)
        .unwrap();
    let mut lock =
        Lockfile::from_json(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let orphan = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "orphan",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &orphan_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    lock.insert_package(orphan).unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    fs::create_dir_all(dir.join("node_modules/orphan")).unwrap();
    fs::write(
        dir.join("node_modules/orphan/package.json"),
        r#"{"name":"orphan","version":"1.0.0"}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "prune",
            "--workspace",
            "web",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/theme/package.json").is_file());
    assert!(!dir.join("node_modules/orphan").exists());
    assert!(!member.join("node_modules").exists());
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    cleanup(dir);
}

#[test]
fn prune_removes_only_unreachable_package_from_managed_node_modules() {
    let dir = temp_dir("prune-reachable");
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"required":"1.0.0"}}"#;
    let (store, _, _) = write_prune_fixture(&dir, manifest, false);
    let lock_before = fs::read_to_string(dir.join("tapid.lock")).unwrap();
    fs::create_dir_all(dir.join("node_modules/old")).unwrap();
    fs::write(dir.join("node_modules/old/file"), "old").unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();

    let output = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/required/package.json").is_file());
    assert!(!dir.join("node_modules/orphan").exists());
    assert!(!dir.join("node_modules/old").exists());
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        lock_before
    );
    cleanup(dir);
}

#[test]
fn prune_preserves_unmarked_node_modules() {
    let dir = temp_dir("prune-unmarked");
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"required":"1.0.0"}}"#;
    let (store, _, _) = write_prune_fixture(&dir, manifest, false);
    fs::create_dir_all(dir.join("node_modules/old")).unwrap();
    fs::write(dir.join("node_modules/old/file"), "old").unwrap();

    let output = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unmarked node_modules"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/old/file").is_file());
    cleanup(dir);
}

#[test]
fn prune_preserves_tree_on_lockfile_manifest_mismatch() {
    let dir = temp_dir("prune-mismatch");
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"required":"1.0.0"}}"#;
    let (store, _, _) = write_prune_fixture(&dir, manifest, true);
    fs::create_dir_all(dir.join("node_modules/old")).unwrap();
    fs::write(dir.join("node_modules/old/file"), "old").unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();

    let output = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("manifest digest mismatch"));
    assert!(dir.join("node_modules/old/file").is_file());
    cleanup(dir);
}

#[test]
fn init_creates_manifest_and_reports_stdout() {
    let dir = temp_dir("init");
    let output = run(&dir, &["init"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Created "));
    assert!(
        fs::read_to_string(dir.join("package.json"))
            .unwrap()
            .contains("\"private\": true")
    );
    cleanup(dir);
}
#[test]
fn validate_reports_success_and_malformed_input() {
    let dir = temp_dir("validate");
    fs::write(
        dir.join("good.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    )
    .unwrap();
    let good = run(&dir, &["manifest", "validate", "good.json"]);
    assert!(good.status.success());
    assert_eq!(
        String::from_utf8_lossy(&good.stdout),
        "Valid manifest: demo@1.0.0\n"
    );
    assert!(good.stderr.is_empty());
    fs::write(dir.join("bad.json"), "not json").unwrap();
    let bad = run(&dir, &["manifest", "validate", "bad.json"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid package.json"));
    assert!(bad.stdout.is_empty());
    cleanup(dir);
}
#[test]
fn lock_verify_reports_valid_and_missing_files() {
    let dir = temp_dir("lock");
    let lock =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let valid = run(&dir, &["lock", "verify"]);
    assert!(valid.status.success());
    assert!(String::from_utf8_lossy(&valid.stdout).contains("Valid lockfile"));
    fs::remove_file(dir.join("tapid.lock")).unwrap();
    let missing = run(&dir, &["lock", "verify"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot verify"));
    cleanup(dir);
}
#[test]
fn clap_rejects_unknown_commands_with_usage_error() {
    let dir = temp_dir("unknown");
    let output = run(&dir, &["wat"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    assert!(output.stdout.is_empty());
    cleanup(dir);
}

#[test]
fn run_requires_checked_in_configuration_before_execution() {
    let dir = temp_dir("run-missing-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: required run configuration is missing: tapid.toml\n"
    );
    assert!(output.stdout.is_empty());
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn run_executes_root_or_explicitly_selected_workspace_member_script() {
    let dir = temp_dir("run-workspace-selection");
    let member_dir = dir.join("packages/news");
    fs::create_dir_all(&member_dir).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"root","version":"1.0.0","workspaces":["packages/news"],"scripts":{"probe":"node script.js"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("script.js"),
        "process.stdout.write('ROOT_SCRIPT')\n",
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.defaults]\nassurance = \"restricted\"\nread = [\".\"]\nsubprocess = true\n\n[run.scripts.probe]\n",
    )
    .unwrap();
    fs::write(
        member_dir.join("package.json"),
        r#"{"name":"news","version":"1.0.0","scripts":{"probe":"node script.js"}}"#,
    )
    .unwrap();
    fs::write(
        member_dir.join("script.js"),
        "process.stdout.write('MEMBER_SCRIPT')\n",
    )
    .unwrap();
    let root_output = run(&dir, &["run", "probe"]);
    let root_stderr = String::from_utf8_lossy(&root_output.stderr);
    if !root_output.status.success()
        && root_stderr.contains("sandbox execution failed (unsupported-containment)")
    {
        eprintln!("skipping: requested Restricted backend is unavailable: {root_stderr}");
        cleanup(dir);
        return;
    }
    assert!(root_output.status.success(), "{}", root_stderr);
    let root_stdout = String::from_utf8_lossy(&root_output.stdout);
    assert!(root_stdout.contains("ROOT_SCRIPT"), "{root_stdout}");
    assert!(!root_stdout.contains("MEMBER_SCRIPT"), "{root_stdout}");

    let member_output = run(&dir, &["run", "probe", "--workspace", "news"]);
    assert!(
        member_output.status.success(),
        "{}",
        String::from_utf8_lossy(&member_output.stderr)
    );
    let member_stdout = String::from_utf8_lossy(&member_output.stdout);
    assert!(member_stdout.contains("MEMBER_SCRIPT"), "{member_stdout}");
    assert!(!member_stdout.contains("ROOT_SCRIPT"), "{member_stdout}");
    cleanup(dir);
}

#[test]
fn run_accepts_process_memory_stats_opt_in_and_aliases() {
    let dir = temp_dir("run-process-memory-stats-arg");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"build":"exit 0"}}"#,
    )
    .unwrap();

    for flag in [
        "--allow-process-memory-stats",
        "--allow-procfs",
        "--allow-memory-read",
    ] {
        let output = run(
            &dir,
            &[
                "run",
                "build",
                flag,
                "--node-runtime",
                env!("CARGO_BIN_EXE_tapid"),
            ],
        );
        assert_eq!(output.status.code(), Some(1), "flag {flag}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "error: required run configuration is missing: tapid.toml\n",
            "flag {flag} was not parsed as a Tapid option"
        );
        assert!(output.stdout.is_empty());
    }
    cleanup(dir);
}

#[cfg(target_os = "linux")]
#[test]
fn run_prints_libuv_process_memory_opt_in_hint_once() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("run-process-memory-stats-hint");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"build":"node"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.defaults]\nassurance = \"restricted\"\nread = [\".\"]\nsubprocess = true\n\n[run.scripts.build]\n",
    )
    .unwrap();
    let runtime_dir = dir.join("runtime");
    fs::create_dir(&runtime_dir).unwrap();
    let fake_node = runtime_dir.join("node");
    fs::write(
        &fake_node,
        "#!/bin/sh\nprintf '%s\\n' 'RUN_MARKER [Error: EACCES: permission denied, uv_resident_set_memory]' >&2\nexit 19\n",
    )
    .unwrap();
    fs::set_permissions(&fake_node, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run(
        &dir,
        &[
            "run",
            "build",
            "--node-runtime",
            fake_node.to_str().unwrap(),
        ],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(19), "{stderr}");
    assert_eq!(stderr.matches("RUN_MARKER").count(), 1, "{stderr}");
    assert_eq!(
        stderr
            .matches("retry with --allow-process-memory-stats")
            .count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains("aliases: --allow-memory-read, --allow-procfs"),
        "{stderr}"
    );
    cleanup(dir);
}

#[cfg(target_os = "linux")]
#[test]
fn run_memory_stats_opt_in_uses_private_procfs_or_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("run-process-memory-stats-opt-in");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"probe":"node"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.defaults]\nassurance = \"restricted\"\nread = [\".\"]\nsubprocess = true\nenvironment = [\"TAPID_TEST_HOST_PID\"]\n\n[run.scripts.probe]\n",
    )
    .unwrap();
    let runtime_dir = dir.join("runtime");
    fs::create_dir(&runtime_dir).unwrap();
    let fake_node = runtime_dir.join("node");
    fs::write(
        &fake_node,
        "#!/bin/sh\n/bin/cat /proc/self/statm\nstatus=$?\nif [ \"$status\" -ne 0 ]; then exit \"$status\"; fi\nif [ -r \"/proc/$TAPID_TEST_HOST_PID/statm\" ]; then exit 43; fi\nif printf x > /proc/self/comm; then exit 44; fi\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&fake_node, fs::Permissions::from_mode(0o755)).unwrap();

    let host_pid = std::process::id().to_string();
    let arguments = [
        "run",
        "probe",
        "--allow-process-memory-stats",
        "--node-runtime",
        fake_node.to_str().unwrap(),
    ];
    let is_root = unsafe { libc::geteuid() == 0 };
    let can_use_sudo = !is_root
        && Command::new("sudo")
            .args(["-n", "true"])
            .output()
            .is_ok_and(|output| output.status.success());
    let output = if can_use_sudo {
        Command::new("sudo")
            .args(["-n", "--", "env"])
            .arg(format!("TAPID_TEST_HOST_PID={host_pid}"))
            .arg(env!("CARGO_BIN_EXE_tapid"))
            .args(arguments)
            .current_dir(&dir)
            .output()
            .unwrap()
    } else {
        run_with_env(&dir, &arguments, "TAPID_TEST_HOST_PID", &host_pid)
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && stderr.contains("unsupported-containment") {
        assert!(
            stdout.is_empty(),
            "child ran before fail-closed rejection: {stdout}"
        );
        cleanup(dir);
        return;
    }
    assert!(output.status.success(), "stdout={stdout} stderr={stderr}");
    let stats = stdout.split_whitespace().collect::<Vec<_>>();
    assert!(!stats.is_empty(), "expected process stats, stdout={stdout}");
    assert!(
        stats.iter().all(|field| field.parse::<u64>().is_ok()),
        "stdout={stdout}"
    );
    assert!(
        stderr.contains("private PID and mount namespaces"),
        "{stderr}"
    );
    assert!(stderr.contains("procfs read-only"), "{stderr}");
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn run_rejects_a_non_regular_configuration_without_opening_it() {
    let dir = temp_dir("run-special-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    let status = Command::new("mkfifo")
        .arg(dir.join("tapid.toml"))
        .status()
        .unwrap();
    assert!(status.success());

    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot read run configuration 'tapid.toml'")
    );
    cleanup(dir);
}

#[test]
fn run_without_runtime_flag_discovers_node_then_reaches_sandbox_preflight() {
    let dir = temp_dir("run-discovered-runtime");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev]\n").unwrap();
    let runtime_dir = dir.join("host-runtime");
    fs::create_dir(&runtime_dir).unwrap();
    let runtime = runtime_dir.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &runtime).unwrap();

    let output = run_with_isolated_path(
        &dir,
        &[
            "run",
            "dev",
            "--",
            "--hostname",
            "127.0.0.1",
            "--port",
            "3001",
        ],
        runtime_dir.as_os_str(),
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // PATH discovery succeeds. The isolated home supplies LOCALAPPDATA, but
    // env_clear removes SystemRoot before validation can reach containment.
    #[cfg(windows)]
    assert_eq!(
        stderr,
        "error: invalid runner execution request: Windows AppContainer launch requires the runner's SystemRoot environment variable\n"
    );
    #[cfg(not(windows))]
    assert!(
        stderr.contains("sandbox execution failed (unsupported-containment)"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty());
    assert!(!stderr.contains("required arguments were not provided"));
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn run_preserves_non_utf8_forwarded_argument_through_cli_boundary() {
    use std::os::unix::ffi::OsStringExt;

    let dir = temp_dir("run-non-utf8-argument");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev]\n").unwrap();
    let runtime = dir.join("node");
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &runtime).unwrap();
    let bad = std::ffi::OsString::from_vec(b"bad-\xff-arg".to_vec());

    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(&dir)
        .env_clear()
        .args(["run", "dev", "--node-runtime"])
        .arg(&runtime)
        .arg("--")
        .arg(&bad)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported-containment"), "{stderr}");
    assert!(!stderr.contains("invalid UTF-8"));
    cleanup(dir);
}

#[test]
fn run_rejects_malformed_configuration_stably() {
    let dir = temp_dir("run-malformed-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev\nnetwork = true").unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: invalid run configuration (malformed)\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_oversized_configuration_before_parsing() {
    let dir = temp_dir("run-oversized-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        vec![b' '; tapid_runner::MAX_CONFIG_BYTES + 1],
    )
    .unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: invalid run configuration (capacity-exceeded)\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_oversized_manifest_before_policy_loading() {
    let dir = temp_dir("run-oversized-manifest");
    fs::write(dir.join("package.json"), vec![b' '; 1_048_577]).unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: manifest exceeds maximum size of 1048576 bytes\n"
    );
    cleanup(dir);
}

#[test]
fn run_requires_an_exact_script_profile() {
    let dir = temp_dir("run-missing-profile");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.defaults]\nnetwork = false\n").unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: run policy profile is missing for script: dev\n"
    );
    cleanup(dir);
}

#[test]
fn run_fails_closed_before_spawn_without_printing_secret_values() {
    let dir = temp_dir("run-unsupported");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"printf spawned > SHOULD_NOT_EXIST"}}"#,
    )
    .unwrap();
    let policy = "[run.scripts.dev]\nenvironment = [\"SECRET_TOKEN\"]\n";
    // Windows supports read-only profiles. Request a specifically unsupported
    // write grant so this remains a pre-spawn containment rejection, not a
    // failure caused by executing the stand-in Node binary.
    #[cfg(windows)]
    let policy = format!("{policy}write = [\"SHOULD_NOT_EXIST\"]\n");
    fs::write(dir.join("tapid.toml"), policy).unwrap();
    let secret = "tapid-super-secret-value";
    let runtime = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &runtime).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args([
            OsStr::new("run"),
            OsStr::new("dev"),
            OsStr::new("--node-runtime"),
            runtime.as_os_str(),
            OsStr::new("--"),
            OsStr::new("--hostname"),
            OsStr::new("127.0.0.1"),
            OsStr::new("--port"),
            OsStr::new("4173"),
        ])
        .current_dir(&dir)
        .env("SECRET_TOKEN", secret)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sandbox execution failed (unsupported-containment)"),
        "{stderr}"
    );
    #[cfg(windows)]
    assert!(
        stderr.contains("project write policies remain unsupported until declared writes and ACL revocation are natively verified"),
        "{stderr}"
    );
    assert!(stderr.contains("no process was started"));
    assert!(stderr.contains("no enforcement receipt was issued"));
    assert!(!stderr.contains(secret));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(output.stdout.is_empty());
    assert!(!dir.join("SHOULD_NOT_EXIST").exists());
    cleanup(dir);
}

#[test]
fn run_rejects_reserved_path_allowlisting_before_runner_execution() {
    let dir = temp_dir("run-path-reserved");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.scripts.dev]\nenvironment = [\"PATH\"]\n",
    )
    .unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: run policy cannot allowlist reserved environment variable PATH\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_a_missing_node_runtime_stably() {
    let dir = temp_dir("run-runtime-missing");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev]\n").unwrap();
    let output = run(&dir, &["run", "dev", "--node-runtime", "missing-node"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: node runtime must be an executable named node or node.exe\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_missing_script_stably_before_policy_loading() {
    let dir = temp_dir("run-missing-script");
    let member_dir = dir.join("packages/news");
    fs::create_dir_all(&member_dir).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","workspaces":["packages/news"],"scripts":{}}"#,
    )
    .unwrap();
    fs::write(
        member_dir.join("package.json"),
        r#"{"name":"news","version":"1.0.0","scripts":{}}"#,
    )
    .unwrap();
    let missing = run(
        &dir,
        &[
            "run",
            "missing",
            "--node-runtime",
            env!("CARGO_BIN_EXE_tapid"),
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr),
        "error: root package script is missing: missing\n"
    );

    let missing_member = run(
        &dir,
        &[
            "run",
            "missing",
            "--workspace",
            "news",
            "--node-runtime",
            env!("CARGO_BIN_EXE_tapid"),
        ],
    );
    assert_eq!(missing_member.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing_member.stderr),
        "error: workspace member 'news' package script is missing: missing\n"
    );
    cleanup(dir);
}

#[test]
fn install_rejects_malformed_lockfile_before_creating_output() {
    let dir = temp_dir("bad-install");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), "not json").unwrap();
    let output = run(&dir, &["install", "--offline"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid lockfile"));
    assert!(!dir.join("node_modules").exists());
    cleanup(dir);
}

#[test]
fn install_rejects_unverified_artifacts_with_offline_or_frozen() {
    for mode in ["offline", "frozen"] {
        let dir = temp_dir(&format!("unverified-{mode}"));
        fs::write(
            dir.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        let output = run(
            &dir,
            &[
                "install",
                "--allow-unverified-registry-artifacts",
                &format!("--{mode}"),
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("cannot be used with --offline or --frozen")
        );
        cleanup(dir);
    }
}

#[test]
fn install_warns_about_unverified_artifacts_before_later_failure() {
    let dir = temp_dir("unverified-warning-error");
    let missing_project = dir.join("missing-project");
    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--project-dir",
            missing_project.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not authenticated against a registry-declared digest"));
    assert!(stderr.contains("cannot access project directory"));
    cleanup(dir);
}

#[test]
fn install_allows_missing_integrity_only_with_explicit_warning() {
    let dir = temp_dir("unverified-online");
    let fixture = dir.join("registry.json");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"foo":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"foo","version":"1.0.0","artifact":"base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA"}]}"#,
    )
    .unwrap();

    let rejected = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("missing dist.integrity"));
    assert!(!dir.join("node_modules").exists());

    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("not authenticated against a registry-declared digest")
    );
    assert!(dir.join("node_modules").is_dir());
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let package = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(
        package["registryIntegrityDeclared"],
        serde_json::Value::Bool(false)
    );

    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    for mode in ["offline", "frozen"] {
        let replay = run(&dir, &["install", &format!("--{mode}")]);
        assert_eq!(replay.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&replay.stderr)
                .contains("lacks registry-declared artifact integrity")
        );
        assert!(!dir.join("node_modules").exists());
    }
    cleanup(dir);
}

#[test]
fn legacy_registry_replay_rejects_without_mutation() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    for mode in ["--offline", "--frozen"] {
        let dir = temp_dir("legacy-registry");
        let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"demo":"1.0.0"}}"#;
        fs::write(dir.join("package.json"), manifest).unwrap();
        let mut lock = lock_for_manifest(manifest);
        let package = LockedPackage::new_with_provenance(
            "https://registry.example.test",
            "demo",
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "b".repeat(64)),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        let key = package.key();
        lock.insert_package(package).unwrap();
        lock.set_roots([key]).unwrap();
        let old = lock.to_json().unwrap().replace(
            "https://registry.example.test",
            "https://REGISTRY.example.test:443",
        );
        fs::write(dir.join("tapid.lock"), &old).unwrap();
        let store = dir.join("store");
        fs::create_dir(&store).unwrap();
        fs::write(store.join("sentinel"), "store unchanged").unwrap();
        fs::create_dir(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules/sentinel"), "layout unchanged").unwrap();
        let output = run(
            &dir,
            &["install", mode, "--store-dir", store.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("noncanonical persisted registry identity"),
            "{error}"
        );
        assert!(
            error.contains("backup") && error.contains("online"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(dir.join("tapid.lock")).unwrap(), old);
        assert_eq!(
            fs::read_to_string(dir.join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(store.join("sentinel")).unwrap(),
            "store unchanged"
        );
        assert_eq!(fs::read_dir(&store).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/sentinel")).unwrap(),
            "layout unchanged"
        );
        assert_eq!(fs::read_dir(dir.join("node_modules")).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            4,
            "rejection must not acquire activation state"
        );
        cleanup(dir);
    }
}

/// Checks that both install spellings reject bare command words without changing the project.
#[test]
fn install_rejects_ambiguous_package_arguments_before_accessing_project() {
    for command in ["install", "i"] {
        for package in ["help", "install", " help ", " install "] {
            for with_manifest in [false, true] {
                let dir = temp_dir("ambiguous-install");
                let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
                if with_manifest {
                    fs::write(dir.join("package.json"), manifest).unwrap();
                }
                let store = dir.join("store");
                let output = run(
                    &dir,
                    &[command, package, "--store-dir", store.to_str().unwrap()],
                );
                assert_eq!(output.status.code(), Some(2));
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("is ambiguous here"), "{stderr}");
                assert!(stderr.contains("tapid install"), "{stderr}");
                assert!(!stderr.contains("cannot read manifest"), "{stderr}");
                assert!(output.stdout.is_empty());
                assert_eq!(
                    fs::read_dir(&dir).unwrap().count(),
                    usize::from(with_manifest)
                );
                if with_manifest {
                    assert_eq!(
                        fs::read_to_string(dir.join("package.json")).unwrap(),
                        manifest
                    );
                }
                cleanup(dir);
            }
        }
    }
}

/// Checks that explicit specs for command-like package names pass argument validation.
#[test]
fn install_accepts_explicit_specs_for_ambiguous_package_names() {
    let dir = temp_dir("explicit-install");
    for command in ["install", "i"] {
        for package in ["help@1.0.0", "install@1.0.0", "npm:help", "npm:install"] {
            let output = run(&dir, &[command, package]);
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("cannot read manifest"), "{stderr}");
        }
    }
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    cleanup(dir);
}

/// Checks frozen replay through `i`, installation help forms, and alias visibility in help.
#[test]
fn install_alias_replays_project_and_provides_help() {
    let dir = temp_dir("install-alias");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(
        dir.join("tapid.lock"),
        lock_for_manifest(manifest).to_json().unwrap(),
    )
    .unwrap();
    let output = run(&dir, &["i", "--frozen"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules").is_dir());
    for args in [
        &["i", "--help"][..],
        &["install", "--help"],
        &["help", "install"],
    ] {
        let output = run(&dir, args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Install dependencies"));
    }
    for args in [&["--help"][..], &["help"]] {
        let output = run(&dir, args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("[alias: i]"));
    }
    cleanup(dir);
}

#[test]
fn install_requires_lockfile_in_offline_and_frozen_modes() {
    for mode in ["offline", "frozen"] {
        let dir = temp_dir(mode);
        fs::write(
            dir.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        let output = run(&dir, &["install", &format!("--{mode}")]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires tapid.lock"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic: LOCKFILE_MISSING"));
        assert!(!dir.join("node_modules").exists());
        cleanup(dir);
    }
}

#[test]
fn install_supports_an_explicit_dynamic_project_directory() {
    let parent = temp_dir("parent");
    let project = parent.join("project");
    fs::create_dir(&project).unwrap();
    let raw_manifest = r#"{"name":"dynamic-app","version":"1.0.0"}"#;
    fs::write(project.join("package.json"), raw_manifest).unwrap();
    let lock = lock_for_manifest(raw_manifest);
    fs::write(project.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let output = run(
        &parent,
        &[
            "install",
            "--project-dir",
            project.to_str().unwrap(),
            "--frozen",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(project.join("node_modules").is_dir());
    cleanup(parent);
}

#[test]
fn dependency_lifecycle_policy_rejects_prepare_before_mutating_project() {
    let project = tapid_test_support::TempProject::new("dependency-hook-policy").unwrap();
    let home = tapid_test_support::TempHome::new("dependency-hook-policy").unwrap();
    let manifest = br#"{"name":"demo","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    project
        .write(
            "tapid.lifecycle.toml",
            br#"schema = 1
[[approvals]]
package = "native-demo"
version = "1.0.0"
hook = "prepare"
"#,
        )
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["install", "--store-dir"])
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("unsupported dependency lifecycle hook prepare"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    assert!(!home.path().join("store").exists());
}

#[test]
fn dependency_lifecycle_hooks_are_reported_and_never_run_by_default() {
    let project = tapid_test_support::TempProject::new("dependency-hooks-denied").unwrap();
    let home = tapid_test_support::TempHome::new("dependency-hooks-denied").unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"},"scripts":{"install":"echo root > ROOT_SCRIPT_RAN"}}"#).unwrap();
    let archive = "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=";
    let bytes = STANDARD.decode(archive).unwrap();
    let fixture = serde_json::json!({"packages":[{
        "registry":"https://registry.npmjs.org", "name":"native-demo", "version":"1.0.0",
        "integrity":format!("sha512-{}", STANDARD.encode(Sha512::digest(&bytes))),
        "artifact":format!("base64:{archive}")
    }]});
    project
        .write("registry.json", &serde_json::to_vec(&fixture).unwrap())
        .unwrap();
    for flags in [
        vec!["install", "--registry-fixture", "registry.json"],
        vec!["install", "--offline", "--frozen"],
        vec!["ci", "--registry-fixture", "registry.json"],
        vec!["ci", "--offline", "--registry-fixture", "registry.json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .args(flags)
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        for hook in ["preinstall", "install", "postinstall", "prepare"] {
            assert!(
                stderr.contains(&format!(
                    "skipped dependency lifecycle hook {hook} for native-demo@1.0.0"
                )),
                "{stderr}"
            );
        }
        assert!(!project.path().join("ROOT_SCRIPT_RAN").exists());
        assert!(!project.path().join("SHOULD_NOT_EXIST").exists());
        assert!(
            !project
                .path()
                .join("node_modules/native-demo/SHOULD_NOT_EXIST")
                .exists()
        );
    }
    project
        .write(
            "tapid.lifecycle.toml",
            format!(
                r#"schema = 1
[[approvals]]
package = "native-demo"
version = "1.0.0"
system-toolchain = true
archive-digest = "sha512-{}"
hook = "install"
script-digest = "sha256-{}"
read = ["."]
write = ["."]
network = false
environment = {{}}
timeout-seconds = 5
max-output-bytes = 1024
max-processes = 32
max-memory-bytes = 134217728
tools = [{{ name = "sh", path = {}, digest = "sha256-{}" }}]
"#,
                "A".repeat(86) + "==",
                "0".repeat(64),
                serde_json::to_string(&project.path().join("unused-sh").to_str().unwrap()).unwrap(),
                "0".repeat(64)
            )
            .as_bytes(),
        )
        .unwrap();
    let before = fs::read(project.path().join("tapid.lock")).unwrap();
    for args in [
        vec!["install", "--offline", "--frozen"],
        vec!["ci", "--registry-fixture", "registry.json"],
        vec!["ci", "--offline", "--registry-fixture", "registry.json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .args(args)
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "mismatched lifecycle approval must reject replay"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("does not match version/archive"));
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
    }
}

#[test]
fn ci_replaces_managed_modules_without_changing_project_files_or_running_scripts() {
    let project = tapid_test_support::TempProject::new("ci-empty").unwrap();
    let dir = project.path().to_path_buf();
    let manifest =
        r#"{"name":"demo","version":"1.0.0","scripts":{"preinstall":"touch SHOULD_NOT_EXIST"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let lock = lock_for_manifest(manifest).to_json().unwrap();
    project.write("tapid.lock", lock.as_bytes()).unwrap();
    let store = dir.join("store");
    let args = ["ci", "--store-dir", store.to_str().unwrap()];
    for _ in 0..2 {
        let output = run(&dir, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(dir.join("node_modules").is_dir());
        assert!(!dir.join("node_modules/stale").exists());
        assert!(!dir.join("SHOULD_NOT_EXIST").exists());
        assert_eq!(fs::read_to_string(dir.join("tapid.lock")).unwrap(), lock);
        assert_eq!(
            fs::read_to_string(dir.join("package.json")).unwrap(),
            manifest
        );
        fs::write(dir.join("node_modules/stale"), "stale").unwrap();
    }
}

#[test]
fn install_replays_valid_lockfile_without_running_scripts() {
    let dir = temp_dir("install");
    let raw_manifest =
        r#"{"name":"demo","version":"1.0.0","scripts":{"preinstall":"touch SHOULD_NOT_EXIST"}}"#;
    fs::write(dir.join("package.json"), raw_manifest).unwrap();
    let lock = lock_for_manifest(raw_manifest);
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let verified = run(&dir, &["lock", "verify"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let output = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules").is_dir());
    assert!(!dir.join("SHOULD_NOT_EXIST").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Replayed lockfile"));
    cleanup(dir);
}

#[test]
fn offline_replay_uses_exact_roots_when_names_have_transitive_versions() {
    let dir = temp_dir("exact-roots");
    let fixture = dir.join("registry.json");
    let debug_four = "base64:H4sIAAAAAAAC/+3TsQrCMBRA0cx+hWTWNLGxg38TNRQV09K0Ioj/bqwFobMU1HuWF97yhnBrtzu50mf1a6pjrIL4MJ0U1vYzGU+tbf5+P/fGFKtczLWYQBdb16Tz4j/dZHBnLzdy77ddKRfy4pt4qELaWKWVlveZwO8aus+Gb1fttRWT92/suP91Yeh/Cn32yz51QgcAAAAAAAAAAAAAAPhCD3aP37sAKAAA";
    let debug_three = "base64:H4sIAAAAAAAC/+3TvQrCMBRA4cw+hWTWNDGxg28TNRQV29IfEcR3N8aC0FkK6vmWG+5yh3Bqvzv5ImT1a6pjW5Xiw3SUO5dmNJ5aO/t+P/fG5Csr5lpMoG8738Tz4j/dZOnPQW7kPmz7Qi7kJTTtoSrjxiqttLzPBH7X0H02fLvqrp2YvH/jxv2vc0P/U0jZL1PqhA4AAAAAAAAAAAAAAPCFHl2bEsoAKAAA";
    let parent = "base64:H4sIAAAAAAAC/+3TsQrCMBRA0cx+hWTWNCltB/8mSBAV05BGKYj/bmwLQmcpqPcsD16GDI8b7P5sD64I41SnrvXiw3TWVNUws/nU+v027o1pykqstVjAtUs25u/Ff7pLby9O7mSw0fkkN/LmYndsfV4ZpZWWj5XAz5q6L6arq9QnsXj/pp73Xzcl/S9z/1f22yF1QgcAAAAAAAAAAAAAAPg+TxkNmJgAKAAA";
    let integrity = |artifact: &str| {
        let bytes = STANDARD
            .decode(artifact.strip_prefix("base64:").unwrap())
            .unwrap();
        format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))
    };
    let debug_four_integrity = integrity(debug_four);
    let debug_three_integrity = integrity(debug_three);
    let parent_integrity = integrity(parent);
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"debug":"^4.0.0","parent":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[
                {{"registry":"https://registry.npmjs.org","name":"debug","version":"4.0.0","integrity":"{debug_four_integrity}","artifact":"{debug_four}"}},
                {{"registry":"https://registry.npmjs.org","name":"debug","version":"3.0.0","integrity":"{debug_three_integrity}","artifact":"{debug_three}"}},
                {{"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","integrity":"{parent_integrity}","artifact":"{parent}","dependencies":{{"debug":"^3.0.0"}}}}
            ]}}"#
        ),
    )
    .unwrap();

    let online = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        online.status.success(),
        "{}",
        String::from_utf8_lossy(&online.stderr)
    );
    // Exercise the documented explicit recovery using a graph with exact roots
    // and transitive edges, preserving the incompatible lock independently.
    let canonical_lock = fs::read_to_string(dir.join("tapid.lock")).unwrap();
    let legacy_lock = canonical_lock.replace(
        "https://registry.npmjs.org",
        "https://REGISTRY.npmjs.org:443",
    );
    fs::write(dir.join("tapid.lock"), &legacy_lock).unwrap();
    fs::copy(dir.join("tapid.lock"), dir.join("tapid.lock.preserved")).unwrap();
    let rejected = run(&dir, &["install", "--frozen"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("noncanonical persisted registry identity")
    );
    let online_rejected = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(!online_rejected.status.success());
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        legacy_lock
    );
    fs::remove_file(dir.join("tapid.lock")).unwrap();
    let recovered = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock.preserved")).unwrap(),
        legacy_lock
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        canonical_lock
    );
    assert!(run(&dir, &["lock", "verify"]).status.success());
    fs::remove_dir_all(dir.join("node_modules")).unwrap();

    let replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(dir.join("node_modules/debug").is_dir());
    assert!(dir.join("node_modules/parent/node_modules/debug").is_dir());
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/debug/version.txt")).unwrap(),
        "debug-4.0.0\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/parent/node_modules/debug/version.txt")).unwrap(),
        "debug-3.0.0\n"
    );

    let lock_path = dir.join("tapid.lock");
    let canonical: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&lock_path).unwrap()).unwrap();
    let package_keys = canonical["packages"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let debug_three = package_keys
        .iter()
        .find(|key| key.contains("|debug@3.0.0|"))
        .unwrap()
        .clone();
    let parent = package_keys
        .iter()
        .find(|key| key.contains("|parent@1.0.0|"))
        .unwrap()
        .clone();

    let mut transitive_root = canonical.clone();
    transitive_root
        .as_object_mut()
        .unwrap()
        .remove("rootBindings");
    let mut invalid_roots = vec![debug_three, parent.clone()];
    invalid_roots.sort();
    transitive_root["roots"] = serde_json::json!(invalid_roots);
    fs::write(
        &lock_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&transitive_root).unwrap()
        ),
    )
    .unwrap();
    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let transitive_replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(!transitive_replay.status.success());
    assert!(
        String::from_utf8_lossy(&transitive_replay.stderr)
            .contains("does not satisfy a direct manifest dependency")
    );
    assert!(!dir.join("node_modules").exists());

    let mut missing_root = canonical.clone();
    missing_root.as_object_mut().unwrap().remove("rootBindings");
    missing_root["roots"] = serde_json::json!([parent]);
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string_pretty(&missing_root).unwrap()),
    )
    .unwrap();
    let missing_replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(!missing_replay.status.success());
    assert!(
        String::from_utf8_lossy(&missing_replay.stderr)
            .contains("must contain exactly one root for direct dependency")
    );
    assert!(!dir.join("node_modules").exists());

    let mut legacy = canonical;
    legacy["lockfileVersion"] = 4.into();
    legacy.as_object_mut().unwrap().remove("roots");
    legacy.as_object_mut().unwrap().remove("rootBindings");
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string_pretty(&legacy).unwrap()),
    )
    .unwrap();

    let legacy_replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(
        legacy_replay.status.success(),
        "{}",
        String::from_utf8_lossy(&legacy_replay.stderr)
    );
    assert!(dir.join("node_modules/debug").is_dir());
    assert!(dir.join("node_modules/parent/node_modules/debug").is_dir());
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/debug/version.txt")).unwrap(),
        "debug-4.0.0\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/parent/node_modules/debug/version.txt")).unwrap(),
        "debug-3.0.0\n"
    );
    cleanup(dir);
}

#[test]
fn padded_and_unpadded_fixture_integrity_produce_canonical_lockfile_values() {
    let dir = temp_dir("canonical-integrity");
    let fixture = dir.join("registry.json");
    let encoded_artifact = "H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let artifact_bytes = STANDARD.decode(encoded_artifact).unwrap();
    let padded = format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(&artifact_bytes))
    );
    let unpadded = padded.trim_end_matches('=').to_owned();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"padded":"1.0.0","unpadded":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[
                {{"registry":"https://registry.npmjs.org","name":"padded","version":"1.0.0","integrity":"{padded}","artifact":"base64:{encoded_artifact}"}},
                {{"registry":"https://registry.npmjs.org","name":"unpadded","version":"1.0.0","integrity":"{unpadded}","artifact":"base64:{encoded_artifact}"}}
            ]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    for package in lock["packages"].as_object().unwrap().values() {
        assert_eq!(package["artifactIntegrity"], padded);
    }
    cleanup(dir);
}

#[test]
fn install_refuses_to_replace_unmarked_node_modules() {
    let dir = temp_dir("ownership");
    let raw = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), raw).unwrap();
    fs::write(
        dir.join("tapid.lock"),
        lock_for_manifest(raw).to_json().unwrap(),
    )
    .unwrap();
    fs::create_dir(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules").join("KEEP"), "user data").unwrap();
    let output = run(&dir, &["install", "--offline"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unmarked node_modules"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.join("node_modules").join("KEEP")).unwrap(),
        "user data"
    );
    cleanup(dir);
}

#[test]
fn injected_activation_failure_restores_marked_node_modules() {
    let dir = temp_dir("activation-failure");
    let raw = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), raw).unwrap();
    fs::write(
        dir.join("tapid.lock"),
        lock_for_manifest(raw).to_json().unwrap(),
    )
    .unwrap();
    fs::create_dir(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules").join("KEEP"), "user data").unwrap();
    fs::write(dir.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
    let output = run_with_env(
        &dir,
        &["install", "--offline"],
        "TAPID_TEST_FAIL_ACTIVATION",
        "1",
    );
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(dir.join("node_modules").join("KEEP")).unwrap(),
        "user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1
"
    );
    assert!(!fs::read_dir(&dir).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tapid-managed-old-")
    }));
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn lifecycle_workspace_symlink_escape_preserves_external_project() {
    use tapid_test_support::TempProject;
    let root = TempProject::new("workspace-cli-escape").unwrap();
    let external = TempProject::new("workspace-cli-external").unwrap();
    root.write(
        "package.json",
        br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
    )
    .unwrap();
    let manifest = br#"{"name":"member","version":"1.0.0","dependencies":{"example":"1.0.0"}}"#;
    external.write("member/package.json", manifest).unwrap();
    external
        .write("member/tapid.lock", b"external lockfile")
        .unwrap();
    std::os::unix::fs::symlink(external.path(), root.path().join("packages")).unwrap();
    for args in [
        vec!["add", "example", "--workspace", "member"],
        vec!["remove", "example", "--workspace", "member"],
        vec!["update", "--workspace", "member"],
        vec!["prune", "--workspace", "member"],
        vec!["outdated", "--workspace", "member"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("outside project"), "{error}");
        assert_eq!(
            fs::read(external.path().join("member/package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read(external.path().join("member/tapid.lock")).unwrap(),
            b"external lockfile"
        );
        assert!(!external.path().join("member/node_modules").exists());
    }
}

#[test]
fn frozen_cold_cache_recovery_preserves_lock_bytes_and_publication_decision() {
    for crash_point in ["store_published", "activation_complete", "commit_decision"] {
        let project = tapid_test_support::TempProject::new("frozen-cold-recovery").unwrap();
        let dir = project.path().to_path_buf();
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","dependencies":{"plugin":"*"}}"#,
            )
            .unwrap();
        let fixture = project.write("registry.json", serde_json::json!({"packages": [{
            "registry": "https://registry.npmjs.org", "name": "plugin", "version": "1.0.0",
            "integrity": "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==",
            "artifact": "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA",
        }]}).to_string().as_bytes()).unwrap();
        let warm = dir.join("warm");
        let initial = run(
            &dir,
            &[
                "install",
                "--store-dir",
                warm.to_str().unwrap(),
                "--registry-fixture",
                fixture.to_str().unwrap(),
            ],
        );
        assert!(
            initial.status.success(),
            "{}",
            String::from_utf8_lossy(&initial.stderr)
        );
        let lock_path = dir.join("tapid.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
        let package = lock["packages"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        package["artifactUrl"] = "https://registry.npmjs.org/plugin/-/plugin-1.0.0.tgz".into();
        let digest: tapid_core::ArtifactDigest =
            package["treeDigest"].as_str().unwrap().parse().unwrap();
        fs::write(&lock_path, lock.to_string()).unwrap();
        let original_lock = fs::read(&lock_path).unwrap();
        project
            .write("node_modules/KEEP", b"previous layout")
            .unwrap();
        let cold = dir.join("cold");
        let args = [
            "install",
            "--frozen",
            "--store-dir",
            cold.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ];
        let offline = run(
            &dir,
            &[
                "install",
                "--offline",
                "--frozen",
                "--store-dir",
                cold.to_str().unwrap(),
                "--registry-fixture",
                fixture.to_str().unwrap(),
            ],
        );
        assert!(!offline.status.success());
        assert!(!cold.join("trees").exists());
        let crash = run_with_env(&dir, &args, "TAPID_TEST_CRASH_POINT", crash_point);
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                crash.status.signal(),
                Some(6),
                "{}",
                String::from_utf8_lossy(&crash.stderr)
            );
        }
        #[cfg(not(unix))]
        assert!(!crash.status.success());
        let recovery = run(
            &dir,
            &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
        );
        assert!(
            recovery.status.success(),
            "{}",
            String::from_utf8_lossy(&recovery.stderr)
        );
        assert_eq!(
            fs::read(&lock_path).unwrap(),
            original_lock,
            "{crash_point}"
        );
        let committed = crash_point == "commit_decision";
        assert_eq!(
            tapid_store::Store::new(&cold)
                .verified_tree_path(&digest)
                .is_ok(),
            committed,
            "{crash_point}"
        );
        assert_eq!(
            dir.join("node_modules/KEEP").exists(),
            !committed,
            "{crash_point}"
        );
        let installed = run(&dir, &args);
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        assert_eq!(fs::read(&lock_path).unwrap(), original_lock);
        test_homes().lock().unwrap().remove(&dir);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn approved_dependency_lifecycle_builds_native_output_and_replays_exactly() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("lifecycle-native").unwrap();
    let home = tapid_test_support::TempHome::new("lifecycle-native").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"}}"#,
        )
        .unwrap();
    let archive = "H4sIAAAAAAAA/+3UXUvDMBQGYK/9FSGCbcGlqZsTVATRCd6J26UgNT3OaJfUJJ3K2H+3+0BhCN7MD/B9bk57QjlJ4G2Vq8d8SGm1qOLBW7OxZrLR7XTmtbFaZbu9//4872dZt51tMLnujXym9iF3zfifmPUHTbjJR8QPWFODHlOroJHlO4yPyXltzWwlE1LIWc8rp6vgm96Ea9NcXFnOv7QFsRaxa67v4spZRd4LMmMxOLm8OLsZ9PqDm37v9Ko3SN5XX3SIO7vJoaOnWjuKozsfJeLZ6UDnuqT+q1FxNCRDLg9UiPASop2P9yi55mx7m6W1d+mtNulQKXZPZWmFYi3LFmfh0+lvX++ft8x9ury8b5nxVf4zubeSf9mVbeT/J2xpo8q6ye+RD4W24v54U5vARrk28djqIplUdfDx8u/AlB1VTTwLPktuqJ1h8nC6+duHAAAAAAAAAAAAAAAAAAAAAPjH3gAN/fj4ACgAAA==";
    // Exact fixture archive and script hashes are approvals, not package-name trust.
    let bytes = STANDARD.decode(archive).unwrap();
    let script = r#"node -e "if(process.env.TAPID_TEST_SECRET)process.exit(42);require('fs').writeFileSync('generated.txt','generated')" && /usr/bin/gcc hello.c -o native"#;
    let sh = fs::canonicalize("/bin/sh").unwrap();
    let node = fs::canonicalize("/usr/bin/node").unwrap();
    let policy = format!(
        r#"schema = 1
[[approvals]]
package = "native-demo"
version = "1.0.0"
archive-digest = "sha512-{}"
hook = "install"
script-digest = "sha256-{:x}"
system-toolchain = true
process-memory-stats = true
read = ["."]
write = ["."]
network = false
environment = {{ TMPDIR = "." }}
timeout-seconds = 20
max-output-bytes = 8192
max-processes = 64
max-memory-bytes = 536870912
tools = [{{name="sh", path="{}", digest="sha256-{:x}"}}, {{name="node", path="{}", digest="sha256-{:x}"}}]
"#,
        STANDARD.encode(Sha512::digest(&bytes)),
        Sha256::digest(script.as_bytes()),
        sh.display(),
        Sha256::digest(fs::read(&sh).unwrap()),
        node.display(),
        Sha256::digest(fs::read(&node).unwrap())
    );
    project
        .write("tapid.lifecycle.toml", policy.as_bytes())
        .unwrap();
    project.write("registry.json", &serde_json::to_vec(&serde_json::json!({"packages":[{"registry":"https://registry.npmjs.org","name":"native-demo","version":"1.0.0","integrity":format!("sha512-{}", STANDARD.encode(Sha512::digest(&bytes))),"artifact":format!("base64:{archive}")}]})).unwrap()).unwrap();
    let install = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .arg("install")
            .args(flags)
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .env("TAPID_TEST_SECRET", "must-not-inherit")
            .output()
            .unwrap()
    };
    // Approving a hook after an ordinary install must build from the pinned source.
    fs::remove_file(project.path().join("tapid.lifecycle.toml")).unwrap();
    let denied = install(&["--registry-fixture", "registry.json"]);
    assert!(
        denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    assert!(
        !project
            .path()
            .join("node_modules/native-demo/generated.txt")
            .exists()
    );
    project
        .write("tapid.lifecycle.toml", policy.as_bytes())
        .unwrap();
    let first = install(&["--registry-fixture", "registry.json"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let package = project.path().join("node_modules/native-demo");
    assert_eq!(
        fs::read(package.join("generated.txt")).unwrap(),
        b"generated"
    );
    let native = Command::new(package.join("native")).output().unwrap();
    assert!(native.status.success());
    assert_eq!(native.stdout, b"native compiled\n");
    let lock_bytes = fs::read(project.path().join("tapid.lock")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&lock_bytes).unwrap()["lockfileVersion"],
        9
    );
    let verified = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["lock", "verify"])
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    for flags in [
        vec!["--registry-fixture", "registry.json"],
        vec!["--offline", "--registry-fixture", "registry.json"],
    ] {
        let replay = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .arg("ci")
            .args(flags)
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .output()
            .unwrap();
        assert!(
            replay.status.success(),
            "{}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
        assert_eq!(
            fs::read(package.join("generated.txt")).unwrap(),
            b"generated"
        );
        assert!(
            fs::read_dir(home.path().join("store/.staging"))
                .unwrap()
                .all(|entry| { !entry.unwrap().path().join("tree").exists() }),
            "ci must release private replay snapshots"
        );
    }
    let lock = Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
    let source = lock.packages().values().next().unwrap();
    assert_ne!(source.tree_digest(), source.install_tree_digest());
    let source_tree = tapid_store::Store::new(home.path().join("store"))
        .verified_tree_path(&source.tree_digest().parse().unwrap())
        .unwrap();
    assert!(!source_tree.join("package/generated.txt").exists());
    // Rehydrate the source while replaying an authenticated derived output.
    // Output verification must happen before store publication takes its lock.
    let store = tapid_store::Store::new(home.path().join("store"));
    fs::remove_dir_all(store.artifact_path(&source.tree_digest().parse().unwrap())).unwrap();
    let hydrated = install(&["--frozen", "--registry-fixture", "registry.json"]);
    assert!(
        hydrated.status.success(),
        "{}",
        String::from_utf8_lossy(&hydrated.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_bytes
    );
    assert_eq!(
        fs::read(package.join("generated.txt")).unwrap(),
        b"generated"
    );
    store
        .verified_tree_path(&source.tree_digest().parse().unwrap())
        .unwrap();
    let mut forged: serde_json::Value = serde_json::from_slice(&lock_bytes).unwrap();
    for package in forged["packages"].as_object_mut().unwrap().values_mut() {
        package["derivedHooks"][0]["attestation"] =
            format!("hmac-sha256-{}", "0".repeat(64)).into();
    }
    let forged_bytes = serde_json::to_vec(&forged).unwrap();
    project.write("tapid.lock", &forged_bytes).unwrap();
    let rejected = install(&["--offline", "--frozen"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("missing or mismatched verified lifecycle output")
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        forged_bytes
    );
    assert_eq!(
        fs::read(package.join("generated.txt")).unwrap(),
        b"generated"
    );
    project.write("tapid.lock", &lock_bytes).unwrap();
    let replay = install(&["--offline", "--frozen"]);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_bytes
    );
    assert_eq!(
        fs::read(package.join("generated.txt")).unwrap(),
        b"generated"
    );
    project
        .write(
            "tapid.lifecycle.toml",
            format!("{policy}\n# changed policy\n").as_bytes(),
        )
        .unwrap();
    let rejected = install(&["--offline", "--frozen"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("missing or mismatched verified lifecycle output")
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_bytes
    );
    assert_eq!(
        fs::read(package.join("generated.txt")).unwrap(),
        b"generated"
    );
    // A changed recipe whose hook fails must leave the active
    // generated package and both source and output store entries unchanged.
    project
        .write(
            "tapid.lifecycle.toml",
            policy.replace("write = [\".\"]", "write = []").as_bytes(),
        )
        .unwrap();
    let failed = install(&["--registry-fixture", "registry.json"]);
    assert!(
        !failed.status.success(),
        "{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&failed.stderr)
            .contains("dependency lifecycle hook install failed")
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_bytes
    );
    assert_eq!(
        fs::read(package.join("generated.txt")).unwrap(),
        b"generated"
    );
    assert!(
        Command::new(package.join("native"))
            .status()
            .unwrap()
            .success()
    );
}
