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

#[path = "cli_cases/cache.rs"]
mod cache_tests;
#[path = "cli_cases/ci.rs"]
mod ci_tests;
#[path = "cli_cases/command_fixture.rs"]
mod command_fixture_tests;
#[path = "cli_cases/command_support.rs"]
mod command_support;
#[path = "cli_cases/explain.rs"]
mod explain_tests;
#[path = "cli_cases/lifecycle_reporting.rs"]
mod lifecycle_reporting_tests;
#[path = "cli_cases/run_permissions.rs"]
mod run_permission_tests;

#[path = "cli_cases/status.rs"]
mod status_tests;

#[path = "cli_cases/tools.rs"]
mod tool_tests;

mod workspace_acceptance;

#[test]
fn run_help_lists_portable_invocation_permission_flags() {
    let project = tapid_test_support::TempProject::new("run-permission-help").unwrap();
    let output = isolated_command(project.path(), &["run", "--help"], true)
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in [
        "--allow-read",
        "--allow-write",
        "--allow-env",
        "--allow-subprocess",
        "--allow-network",
    ] {
        assert!(help.contains(flag), "missing portable flag {flag}: {help}");
    }
}

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
    assert!(output.status.success(), "{output:?}");
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
    let mut homes = test_homes().lock().unwrap();
    let home = homes
        .entry(cwd.to_path_buf())
        .or_insert_with(|| tapid_test_support::TempHome::new("cli-install").unwrap());
    command_support::command_in_home(cwd, home.path(), args, clear_environment)
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
    assert!(output.status.success(), "{output:?}");
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
    assert!(output.stderr.is_empty(), "{output:?}");
    let output = run(project.path(), &["install", "--offline", "--frozen"]);
    assert!(output.status.success(), "{output:?}");
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
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{output:?}"
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    let output = run(project.path(), &["install", "--offline", "--frozen"]);
    assert!(output.status.success(), "{output:?}");
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
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Lock selections: 0 added, 0 changed, 1 reused, 0 removed"),
        "{output:?}"
    );
    project
        .write(
            "package.json",
            br#"{"name":"output","version":"1.0.0","dependencies":{"h3":"2.0.0"}}"#,
        )
        .unwrap();
    let output = run(project.path(), &args);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Lock selections: 1 added, 0 changed, 0 reused, 1 removed"),
        "{output:?}"
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
    assert_eq!(manifest["dependencies"]["local"], "npm:h3@^2.0.0");
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
    for declaration in ["npm:../outside@1", "npm:actual@npm:other@1", "npm:actual@"] {
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

    let project = tapid_test_support::TempProject::new("outdated-read-only").unwrap();
    let dir = project.path().to_path_buf();
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
    assert!(stdout.contains("compatible-change=minor compatible-lockfile=changed"));
    assert!(
        stdout.contains(
            "available-change=major available-lockfile=changed available-manifest=changed"
        )
    );
    assert!(stdout.contains("Transitive lockfile changes require resolution"));
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
    let offline = run(&dir, &["outdated", "--offline"]);
    assert!(offline.status.success());
    assert_eq!(offline.stdout, outdated.stdout);
    assert!(!stdout.contains("registry metadata returned no versions"));
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_before);
    cleanup(dir);
}

#[test]
fn outdated_uses_configured_private_registry_identity() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("outdated-private-registry").unwrap();
    let dir = project.path().to_path_buf();
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
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.acme.example'\ntoken-env='TAPID_TEST_OUTDATED_PRIVATE_TOKEN'\n",
    ).unwrap();
    let offline = isolated_command(&dir, &["outdated", "--offline"], false)
        .env_remove("TAPID_TEST_OUTDATED_PRIVATE_TOKEN")
        .output()
        .unwrap();
    assert!(offline.status.success());
    let stdout = String::from_utf8_lossy(&offline.stdout);
    assert!(stdout.contains("locked=1.0.0 compatible=unavailable available=unavailable"));
    assert!(stdout.contains("offline: newer registry versions were not checked"));
    assert!(stdout.contains("available-lockfile=unknown"));
    assert!(!stdout.contains("up to date"));
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
            "^2.0.0",
        ),
        (
            "dependencies",
            "optionalDependencies",
            "h3",
            "^1.0.0",
            ">=1.0.0",
            "^2.0.0",
        ),
        (
            "devDependencies",
            "peerDependencies",
            "local",
            "npm:h3@^1.0.0",
            "npm:h3@>=1.0.0",
            "npm:h3@^2.0.0",
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
                let mut registry: serde_json::Value =
                    serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
                registry["packages"].as_array_mut().unwrap().push(serde_json::json!({"registry":"https://registry.npmjs.org","name":"unselected","version":"4.2.0","artifact":"unused-peer-artifact","integrity":"sha512-SP3u8B5tYPrOW7kCfDRmZoQY3yTbvjwDgFzW0WVF1BCnjLdQxdsaAA0oOtdILLuhK+j+koC8cegztMLHxAMoxg=="}));
                let fixture = project
                    .write("registry.json", registry.to_string().as_bytes())
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
                    if latest && select_all {
                        "^4.2.0"
                    } else {
                        "^4.0.0"
                    }
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
fn named_workspace_update_saves_member_and_preserves_other_bindings() {
    let project = tapid_test_support::TempProject::new("workspace-update-save").unwrap();
    let dir = project.path();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"root-pin":"npm:h3@*"}}"#).unwrap();
    project.write("packages/web/package.json", br#"{"name":"web","version":"1.0.0","dependencies":{"selected":"npm:h3@*","member-pin":"npm:h3@*","local":"workspace:^"}}"#).unwrap();
    project
        .write(
            "packages/local/package.json",
            br#"{"name":"local","version":"1.0.0"}"#,
        )
        .unwrap();
    let full: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    let mut initial = full.clone();
    initial["packages"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["name"] != "h3" || p["version"] == "1.0.0");
    let fixture = project
        .write("registry.json", initial.to_string().as_bytes())
        .unwrap();
    let store = dir.join("store");
    let common = [
        "--registry-fixture",
        fixture.to_str().unwrap(),
        "--store-dir",
        store.to_str().unwrap(),
    ];
    let mut install = vec!["install"];
    install.extend(common);
    let output = run(dir, &install);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(&fixture, full.to_string()).unwrap();
    let root_bytes = fs::read(dir.join("package.json")).unwrap();
    let mut update = vec![
        "update",
        "selected",
        "local",
        "--workspace",
        "web",
        "--latest",
        "--save-tilde",
    ];
    update.extend(common);
    let output = run(dir, &update);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(dir.join("package.json")).unwrap(), root_bytes);
    let member: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("packages/web/package.json")).unwrap()).unwrap();
    assert_eq!(member["dependencies"]["selected"], "npm:h3@~2.0.0");
    assert_eq!(member["dependencies"]["member-pin"], "npm:h3@*");
    assert_eq!(member["dependencies"]["local"], "workspace:^");
    let lock = Lockfile::from_json(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(lock.root_bindings()["root-pin"].contains("h3@1.0.0"));
    let member = lock
        .workspace_packages()
        .values()
        .find(|member| member.dependencies().contains_key("selected"))
        .unwrap();
    assert!(member.dependencies()["member-pin"].contains("h3@1.0.0"));
    assert!(member.dependencies()["selected"].contains("h3@2.0.0"));
    let output = run(
        dir,
        &[
            "install",
            "--frozen",
            "--offline",
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

#[test]
fn update_rejects_conflicting_save_policies_before_mutation() {
    let project = tapid_test_support::TempProject::new("update-save-conflict").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1"}}"#,
        )
        .unwrap();
    let original = fs::read(project.path().join("package.json")).unwrap();
    for flags in [
        ["--save-exact", "--save-caret"],
        ["--save-exact", "--save-tilde"],
        ["--save-caret", "--save-tilde"],
    ] {
        let output = run(project.path(), &["update", "h3", flags[0], flags[1]]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            original
        );
        assert!(!project.path().join("tapid.lock").exists());
    }
}

#[test]
fn named_update_keeps_ranges_and_explains_required_transitive_changes() {
    for (peer, pinned) in [(false, false), (true, false), (true, true)] {
        let unselected = if peer { "h3" } else { "unrelated" };
        let project = tapid_test_support::TempProject::new("update-transitive").unwrap();
        let dir = project.path();
        project.write("package.json", serde_json::json!({"name":"app","version":"1.0.0","dependencies":{"alias-parent":"*", unselected: if pinned { "npm:h3@1.0.0" } else { "npm:h3@*" }}}).to_string().as_bytes()).unwrap();
        let mut registry: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
        let packages = registry["packages"].as_array_mut().unwrap();
        packages.retain(|p| p["name"] != "h3" || p["version"] == "1.0.0");
        let parent = packages
            .iter_mut()
            .find(|p| p["name"] == "alias-parent")
            .unwrap();
        parent["dependencies"] = if peer {
            serde_json::json!({})
        } else {
            serde_json::json!({"child": "npm:h3@^1"})
        };
        if peer {
            parent["peerDependencies"] = serde_json::json!({"h3": "^1"});
        }
        let fixture = project
            .write("registry.json", registry.to_string().as_bytes())
            .unwrap();
        let store = dir.join("store");
        let common = [
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ];
        let mut install = vec!["install"];
        install.extend(common);
        let output = run(dir, &install);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let original_manifest = fs::read(dir.join("package.json")).unwrap();
        let mut parent = registry["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "alias-parent")
            .unwrap()
            .clone();
        parent["version"] = "2.0.0".into();
        parent["dependencies"] = if peer {
            serde_json::json!({})
        } else {
            serde_json::json!({"child": "npm:h3@^2"})
        };
        if peer {
            parent["peerDependencies"] = serde_json::json!({"h3": "^2"});
        }
        registry["packages"].as_array_mut().unwrap().push(parent);
        let full: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
        registry["packages"].as_array_mut().unwrap().push(
            full["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "h3" && p["version"] == "2.0.0")
                .unwrap()
                .clone(),
        );
        fs::write(&fixture, registry.to_string()).unwrap();
        let original_lock = fs::read(dir.join("tapid.lock")).unwrap();
        let mut update = if peer {
            vec!["--json", "update", "alias-parent"]
        } else {
            vec!["update", "alias-parent"]
        };
        update.extend(common);
        let output = run(dir, &update);
        if pinned {
            assert!(!output.status.success());
            assert_eq!(
                fs::read(dir.join("package.json")).unwrap(),
                original_manifest
            );
            assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), original_lock);
            continue;
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            original_manifest
        );
        let lock =
            Lockfile::from_json(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
        assert!(lock.root_bindings()[unselected].contains(if peer {
            "h3@2.0.0"
        } else {
            "h3@1.0.0"
        }));
        assert!(lock.root_bindings()["alias-parent"].contains("alias-parent@2.0.0"));
        assert!(lock.packages().keys().any(|key| key.contains("h3@2.0.0")));
        let explanation = if peer {
            assert!(output.stderr.is_empty());
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(
                report["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|code| code == "UPDATE_GRAPH_CHANGE")
            );
            assert!(
                !report["data"]["graph_changes"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            report["data"]["graph_changes"].to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        assert!(
            explanation.contains("h3@2.0.0")
                && explanation.contains("alias-parent@2.0.0")
                && explanation.contains(if peer { "peer" } else { "child" }),
            "{explanation}"
        );
    }
}

#[test]
fn named_update_preserves_unselected_alias_pin_and_saves_explicit_policies() {
    for (flag, expected, latest) in [
        ("--save-exact", "2.0.0", true),
        ("--save-exact", "2.0.0", false),
        ("--save-caret", "^2.0.0", true),
        ("--save-caret", "^2.0.0", false),
        ("--save-tilde", "~2.0.0", true),
        ("--save-tilde", "~2.0.0", false),
    ] {
        let project = tapid_test_support::TempProject::new("named-update").unwrap();
        let dir = project.path();
        project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"selected":"npm:h3@*","unrelated":"npm:h3@*","alias-parent":"*"}}"#).unwrap();
        let mut full: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
        full["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"] == "alias-parent")
            .unwrap()["dependencies"] = serde_json::json!({"child": "npm:h3@*"});
        let mut next_parent = full["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "alias-parent")
            .unwrap()
            .clone();
        next_parent["version"] = "2.0.0".into();
        full["packages"].as_array_mut().unwrap().push(next_parent);
        let mut initial = full.clone();
        initial["packages"].as_array_mut().unwrap().retain(|p| {
            (p["name"] != "h3" && p["name"] != "alias-parent") || p["version"] == "1.0.0"
        });
        let fixture = project
            .write("registry.json", initial.to_string().as_bytes())
            .unwrap();
        let store = dir.join("store");
        let common = [
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ];
        let mut install = vec!["install"];
        install.extend(common);
        let output = run(dir, &install);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::write(&fixture, full.to_string()).unwrap();
        let mut update = vec!["update", "selected", "alias-parent", flag];
        if latest {
            update.push("--latest");
        }
        update.extend(common);
        let output = run(dir, &update);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("package.json")).unwrap()).unwrap();
        assert_eq!(
            manifest["dependencies"]["selected"],
            format!("npm:h3@{expected}")
        );
        assert_eq!(manifest["dependencies"]["unrelated"], "npm:h3@*");
        for (name, version) in [("selected", "2.0.0"), ("unrelated", "1.0.0")] {
            let installed: serde_json::Value = serde_json::from_slice(
                &fs::read(dir.join("node_modules").join(name).join("package.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(installed["version"], version);
        }
        let lock =
            Lockfile::from_json(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
        let parent = lock
            .packages()
            .values()
            .find(|package| package.name() == "alias-parent")
            .unwrap();
        assert!(
            parent.dependencies()["child"].contains("h3@1.0.0"),
            "unrelated transitive pin changed: {:?}",
            parent.dependencies()
        );
        let mut replay = vec!["install", "--offline", "--frozen"];
        replay.extend(["--store-dir", store.to_str().unwrap()]);
        let output = run(dir, &replay);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
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
    assert_eq!(after_update, manifest);
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
    assert!(after_latest.contains("\"is-char\": \"^2.0.0\""));
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
        r#"{"name":"tool","version":"1.0.0","bin":{"workspace-tool":"bin/tool.js","workspace-direct":"bin/direct.exe"}}"#,
    )
    .unwrap();
    let bin_source = member.join("bin/tool.js");
    fs::write(
        &bin_source,
        "#!/usr/bin/env node\nconsole.log('workspace tool');\n",
    )
    .unwrap();
    let direct_source = member.join("bin/direct.exe");
    #[cfg(windows)]
    let system_root = std::env::var_os("SystemRoot").unwrap();
    #[cfg(windows)]
    fs::copy(
        PathBuf::from(&system_root).join("System32").join("cmd.exe"),
        &direct_source,
    )
    .unwrap();
    #[cfg(unix)]
    fs::write(&direct_source, "#!/bin/sh\nexit 7\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin_source, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&direct_source, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(unix)]
    let mode_before = (
        fs::metadata(&bin_source).unwrap().permissions(),
        fs::metadata(&direct_source).unwrap().permissions(),
    );

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
        let direct_shim = dir
            .join("node_modules")
            .join(".bin")
            .join("workspace-direct");
        assert!(
            fs::symlink_metadata(&direct_shim)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::canonicalize(&direct_shim).unwrap(),
            fs::canonicalize(&direct_source).unwrap()
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&bin_source).unwrap().permissions().mode(),
            mode_before.0.mode(),
            "install must not change workspace source permissions"
        );
        assert_eq!(
            fs::metadata(&direct_source).unwrap().permissions().mode(),
            mode_before.1.mode(),
            "install must not change workspace source permissions"
        );
    }
    #[cfg(windows)]
    {
        assert!(shim.with_extension("cmd").is_file());
        assert!(shim.with_extension("ps1").is_file());
        let direct_shim = dir
            .join("node_modules")
            .join(".bin")
            .join("workspace-direct");
        assert!(direct_shim.with_extension("cmd").is_file());
        assert!(direct_shim.with_extension("ps1").is_file());
        use std::os::windows::process::CommandExt;
        let command = direct_shim.with_extension("cmd");
        let command = command.to_string_lossy();
        let command = command.strip_prefix(r"\\?\").unwrap_or(&command);
        let executed = Command::new(PathBuf::from(&system_root).join("System32").join("cmd.exe"))
            .args(["/D", "/S", "/C"])
            .raw_arg(format!("\"\"{command}\" /D /C exit 7\""))
            .env_clear()
            .env("SystemRoot", &system_root)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(executed.status.code(), Some(7), "{executed:?}");
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
    assert!(
        stderr.contains("1.0.0 (unsupported_dependency_requirement)"),
        "{stderr}"
    );
    assert!(!stderr.contains("dependency broken"), "{stderr}");
    assert!(!stderr.contains("not a valid range"), "{stderr}");
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
    assert!(
        stderr.contains("1.0.0 (unsupported_dependency_requirement)"),
        "{stderr}"
    );
    assert!(!stderr.contains("dependency broken"), "{stderr}");
    assert!(!stderr.contains("not a valid range"), "{stderr}");
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
    {
        assert!(
            !crash.status.success(),
            "crash hook did not terminate process"
        );
    }

    let pending_manifest = fs::read(dir.join("package.json")).unwrap();
    let pending_lock = fs::read(dir.join("tapid.lock")).unwrap();
    let pending_journal = fs::read(dir.join(".tapid-lifecycle-journal.json")).unwrap();
    let inspection = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert_eq!(inspection.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&inspection.stderr).contains("recovery required"));
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        pending_manifest
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), pending_lock);
    assert_eq!(
        fs::read(dir.join(".tapid-lifecycle-journal.json")).unwrap(),
        pending_journal
    );
    let recovery = run_with_env(
        &dir,
        &[
            "install",
            "--offline",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ],
        "TAPID_TEST_RECOVER_ONLY",
        "1",
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

/// Integrity, malformed-gzip, and oversized-entry failures preserve the prior project
/// and verified store, report the rejection cause, and leave no staging residue.
#[test]
fn install_preserves_project_and_store_on_integrity_and_archive_failures() {
    use tapid_store::Store;

    for failure in ["integrity", "archive", "archive-entry-limit"] {
        let project = tapid_test_support::TempProject::new(&format!("rollback-{failure}")).unwrap();
        let dir = project.path().to_path_buf();
        let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
        fs::write(dir.join("package.json"), manifest).unwrap();
        let previous_lock = prior_empty_lock();
        fs::write(dir.join("tapid.lock"), &previous_lock).unwrap();
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
        fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();

        let installed_digest =
            tapid_archive::canonical_tree_digest(&dir.join("node_modules")).unwrap();
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
        } else if failure == "archive-entry-limit" {
            // Gzip of one ustar header declaring a 512 MiB + 1 byte file,
            // with no payload. Rejection must use the header limit before
            // trying to traverse the missing payload.
            STANDARD.decode("H4sIAAAAAAAAEytITM5OTE/Vz0ksSk9loA0wMDAwMDMxAdMGBgYI2gTKMjCEiUHEDQ2MTQ0YFAxo5B4UUFpckljEYEAXuwYhAAAASQbGAAIAAA==").unwrap()
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
        } else if failure == "archive-entry-limit" {
            assert!(stderr.contains("too large (536870913 bytes)"), "{stderr}");
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
        assert_eq!(
            tapid_archive::canonical_tree_digest(&dir.join("node_modules")).unwrap(),
            installed_digest
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
        let staging = store_dir.join(".staging");
        if staging.exists() {
            assert_eq!(
                fs::read_dir(staging).unwrap().count(),
                0,
                "staging leaked after {failure}"
            );
        }
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
    // A missing write target must still reject before spawn when networking is enabled.
    #[cfg(windows)]
    let policy = {
        fs::create_dir(dir.join("writable")).unwrap();
        format!("{policy}network = true\nwrite = [\"missing/nested\"]\n")
    };
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
    {
        assert!(stderr.contains("metadata open failed"), "{stderr}");
    }
    assert!(stderr.contains("no enforcement receipt was issued"));
    assert!(!stderr.contains(secret));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(output.stdout.is_empty());
    assert!(!dir.join("SHOULD_NOT_EXIST").exists());
    cleanup(dir);
}

#[cfg(windows)]
#[test]
fn run_windows_existing_target_writes_preserve_binding_and_assurance_receipts() {
    let node = PathBuf::from(
        std::env::var_os("TAPID_TEST_NODE")
            .expect("native Windows CLI acceptance requires CI-provisioned TAPID_TEST_NODE"),
    );
    assert!(node.is_absolute() && node.is_file());
    for (assurance, target, kind) in [
        ("restricted", "writable", "DirectorySubtree"),
        ("managed-tree", "writable", "DirectorySubtree"),
        ("restricted", "exact.txt", "ExactFile"),
    ] {
        let owner = tapid_test_support::TempProject::new("cli-windows-write").unwrap();
        let root = fs::canonicalize(owner.path()).unwrap();
        fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        let existing = if kind == "ExactFile" {
            root.join(target)
        } else {
            fs::create_dir(root.join(target)).unwrap();
            root.join(target).join("existing.txt")
        };
        fs::write(&existing, b"old").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0","scripts":{"probe":"node probe.js"}}"#,
        )
        .unwrap();
        let relative = if kind == "ExactFile" {
            "exact.txt"
        } else {
            "writable/existing.txt"
        };
        fs::write(root.join("probe.js"), format!(
            "const fs=require('node:fs'),a=require('node:assert/strict');fs.writeFileSync('{relative}','overwrite');fs.appendFileSync('{relative}','append');fs.truncateSync('{relative}',3);a.throws(()=>fs.writeFileSync('outside.txt','escape'),e=>['EPERM','EACCES'].includes(e.code));console.log('CLI_WRITE_OK');"
        )).unwrap();
        let limits = if assurance == "managed-tree" {
            "timeout_seconds = 10\nmax_output_bytes = 65536\nmax_processes = 8\nmax_memory_bytes = 536870912\n"
        } else {
            ""
        };
        fs::write(root.join("tapid.toml"), format!(
            "[run.scripts.probe]\nassurance = \"{assurance}\"\nread = [\".\"]\nwrite = [\"{target}\"]\nnetwork = false\n{limits}"
        )).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(["run", "probe", "--receipt-json", "--node-runtime"])
            .arg(&node)
            .current_dir(&root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert_eq!(output.stdout, b"CLI_WRITE_OK\n");
        assert_eq!(fs::read(&existing).unwrap(), b"ove");
        assert!(!root.join("outside.txt").exists());
        let receipt: serde_json::Value =
            serde_json::from_str(stderr.lines().find(|line| line.starts_with('{')).unwrap())
                .unwrap();
        let managed = assurance == "managed-tree";
        assert_eq!(
            receipt["backend"]["name"],
            "tapid-runner/windows-appcontainer-job"
        );
        assert_eq!(
            receipt["assurance"],
            if managed { "ManagedTree" } else { "Restricted" }
        );
        assert_eq!(receipt["enforced"]["process_tree_membership"], managed);
        assert_eq!(
            receipt["completion"]["confirmed"]["complete_cleanup"],
            managed
        );
        assert_eq!(
            receipt["completion"]["cleanup_confidence"],
            if managed {
                "KernelOwnedComplete"
            } else {
                "BestEffortObserved"
            }
        );
        let writes: Vec<_> = receipt["effective_filesystem"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|grant| grant["access"] == "Write")
            .collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0]["path"],
            root.join(target).to_string_lossy().as_ref()
        );
        assert_eq!(writes[0]["kind"], kind);
        assert_eq!(writes[0]["source"], "ProjectPolicy");
        assert_eq!(writes[0]["binding"], "CanonicalPath");
    }
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
fn dependency_lifecycle_required_native_build_fails_without_approval() {
    let project = tapid_test_support::TempProject::new("required-native-hook").unwrap();
    let home = tapid_test_support::TempHome::new("required-native-hook").unwrap();
    let manifest = br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    write_dependency_script_fixture(
        &project,
        "H4sIAAAAAAAAE+3TTQrCMBAF4B5FZq3pS61d9DbRhhClk9DWghTvLv4spAhuahWcb/OG2cxsXjS7g3E2jfdU+zZwMjEAKPL8lgDGCWye5ute6yLTyQJTP/LKse1Mk2CWWz9oIDa1pZLYdL63q8rWgZbU26b1gakkraBA52//KT7j0ft067ny7JQ7xclvvO0/snH/82It/Z/DIM0WQoi/dAELlQxvAAwAAA==",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "required native build must not appear installed successfully"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("required dependency lifecycle hook install")
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    // Exact approval must pass the required-hook gate. The containment override
    // must still verify pinned tool bytes before any execution.
    project.write("shell", b"not an executable").unwrap();
    let fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("registry.json")).unwrap()).unwrap();
    let policy = toml::to_string(&serde_json::json!({"schema":1,"approvals":[{
        "package":"native-demo","version":"1.0.0","archive-digest":fixture["packages"][0]["integrity"],
        "hook":"install","script-digest":format!("sha256-{:x}", Sha256::digest(b"node-gyp rebuild")),
        "system-toolchain":true,"read":["."],"write":["."],"network":false,"environment":{},
        "timeout-seconds":5,"max-output-bytes":1024,"max-processes":32,"max-memory-bytes":134217728,
        "tools":[{"name":"sh","path":project.path().join("shell").to_str().unwrap(),"digest":format!("sha256-{}", "0".repeat(64))}]
    }]})).unwrap();
    project
        .write("tapid.lifecycle.toml", policy.as_bytes())
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "install",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("lifecycle tool digest mismatch for sh")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "install",
            "--json",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"], serde_json::Value::Null);
    let hook = &result["lifecycle"]["packages"][0]["hooks"][0];
    assert_eq!(hook["authorization"], "exact_policy");
    assert_eq!(hook["disposition"], "blocked");
    assert_eq!(hook["reason"], "tool_verification_failed");
}

fn write_dependency_script_fixture(project: &tapid_test_support::TempProject, archive: &str) {
    let bytes = STANDARD.decode(archive).unwrap();
    project
        .write(
            "registry.json",
            &serde_json::to_vec(&serde_json::json!({"packages":[{
                "registry":"https://registry.npmjs.org", "name":"native-demo", "version":"1.0.0",
                "integrity":format!("sha512-{}", STANDARD.encode(Sha512::digest(&bytes))),
                "artifact":format!("base64:{archive}")
            }]}))
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn dependency_lifecycle_failed_unsafe_hook_preserves_previous_install() {
    let project = tapid_test_support::TempProject::new("unsafe-hook-rollback").unwrap();
    let home = tapid_test_support::TempHome::new("unsafe-hook-rollback").unwrap();
    let manifest = br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    write_dependency_script_fixture(
        &project,
        "H4sIAAAAAAAAE+3PQQrCMBAF0B6l/HWNEy1Z5DahBom2ScjEUii9u1AXgrgRirrI2/zhb2Ymmu5qznYfHykuHHy1MSIi1bZrEtFrkpTPee2lVFJVNW19yDs3ziZV9JVdf2iGN4OFhjfZjXZ3skNAg9EmdsFDQwoShAbcJRczQ89wnrPpe2jYyeX6cMSy/PqPoiiK4jN3HmGcyAAIAAA=",
    );
    let install = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .args([
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
            ])
            .arg(home.path().join("store"))
            .args(flags)
            .env_remove("PATH")
            .output()
            .unwrap()
    };
    assert!(install(&[]).status.success());
    project
        .write("node_modules/previous-marker", b"preserved")
        .unwrap();
    project
        .write("package-lock.json", b"npm lock sentinel\n")
        .unwrap();
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    let output = install(&[
        "--allow-unapproved-dependency-scripts",
        "--unsafe-no-dependency-sandbox",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Exited(23)"));
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/previous-marker")).unwrap(),
        b"preserved"
    );
    let output = install(&[
        "--allow-unapproved-dependency-scripts",
        "--unsafe-no-dependency-sandbox",
        "--json",
    ]);
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["lifecycle"]["hooks_executed"], 0);
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["disposition"],
        "blocked"
    );
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["reason"],
        "execution_or_output_failed"
    );
    assert_eq!(
        fs::read(project.path().join("package-lock.json")).unwrap(),
        b"npm lock sentinel\n"
    );
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
    assert_eq!(
        fs::read(project.path().join("node_modules/previous-marker")).unwrap(),
        b"preserved"
    );
    for package in Lockfile::from_json(std::str::from_utf8(&lock).unwrap())
        .unwrap()
        .packages()
        .values()
    {
        tapid_store::Store::new(home.path().join("store"))
            .verified_tree_path(&package.tree_digest().parse().unwrap())
            .unwrap();
    }
    assert!(!home.path().join("store/.tapid-lifecycle-key").exists());
}

#[test]
fn dependency_override_warnings_preserve_json_and_human_output_contracts() {
    let project = tapid_test_support::TempProject::new("override-warning-output").unwrap();
    let home = tapid_test_support::TempHome::new("override-warning-output").unwrap();
    project
        .write("package.json", br#"{"name":"demo","version":"1.0.0"}"#)
        .unwrap();
    let install = |json: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .current_dir(project.path())
            .args([
                "install",
                "--allow-unapproved-dependency-scripts",
                "--unsafe-no-dependency-sandbox",
                "--store-dir",
            ])
            .arg(home.path().join("store"));
        if json {
            command.arg("--json");
        }
        command.output().unwrap()
    };
    for json in [true, false] {
        let output = install(json);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            assert!(
                output.stderr.is_empty(),
                "JSON commands must keep stderr empty"
            );
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(
                result
                    .to_string()
                    .contains("DEPENDENCY_SCRIPTS_WITHOUT_CONTAINMENT")
            );
        } else {
            assert_eq!(
                String::from_utf8_lossy(&output.stderr)
                    .matches("warning:")
                    .count(),
                2
            );
        }
    }
    fs::remove_file(project.path().join("package.json")).unwrap();
    let failed = install(false);
    assert!(!failed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&failed.stderr)
            .matches("warning:")
            .count(),
        2
    );
    let failed = install(true);
    assert!(!failed.status.success());
    assert!(failed.stderr.is_empty());
    serde_json::from_slice::<serde_json::Value>(&failed.stdout).unwrap();
}

#[test]
fn dependency_review_regression_workspace_selection_retains_json_override_warnings() {
    let project = tapid_test_support::TempProject::new("workspace-override-warning").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"root","version":"1.0.0","private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
    project
        .write(
            "packages/local/package.json",
            br#"{"name":"local","version":"1.0.0"}"#,
        )
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "install",
            "--json",
            "--workspace",
            "missing",
            "--allow-unapproved-dependency-scripts",
            "--unsafe-no-dependency-sandbox",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for warning in [
        "UNAPPROVED_DEPENDENCY_SCRIPTS_ALLOWED",
        "DEPENDENCY_SCRIPTS_WITHOUT_CONTAINMENT",
    ] {
        assert!(
            json.to_string().contains(warning),
            "missing {warning}: {json}"
        );
    }
    assert!(!project.path().join("tapid.lock").exists());
}

#[test]
fn dependency_review_regression_workspace_override_does_not_read_live_members() {
    let project = tapid_test_support::TempProject::new("workspace-hook-override").unwrap();
    let home = tapid_test_support::TempHome::new("workspace-hook-override").unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","private":true,"workspaces":["packages/*"],"dependencies":{"native-demo":"1.0.0"}}"#).unwrap();
    project
        .write(
            "packages/local/package.json",
            br#"{"name":"local","version":"1.0.0"}"#,
        )
        .unwrap();
    project
        .write("packages/local/private.txt", b"live-workspace-secret")
        .unwrap();
    write_dependency_script_fixture(
        &project,
        "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "install",
            "--allow-unapproved-dependency-scripts",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(home.path().join("store"))
        .env_remove("PATH")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .is_file()
    );
    assert_eq!(
        fs::read(project.path().join("packages/local/private.txt")).unwrap(),
        b"live-workspace-secret"
    );
    assert!(!home.path().join("store/.tapid-lifecycle-key").exists());
    // An exact approval still cannot request live workspace dependency access.
    let package: serde_json::Value = serde_json::from_slice(
        &fs::read(project.path().join("node_modules/native-demo/package.json")).unwrap(),
    )
    .unwrap();
    let fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("registry.json")).unwrap()).unwrap();
    let shell = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe")
    } else {
        PathBuf::from("/bin/sh")
    };
    let shell = fs::canonicalize(shell).unwrap();
    let policy = toml::to_string(&serde_json::json!({"schema":1,"approvals":[{
        "package":"native-demo","version":"1.0.0","archive-digest":fixture["packages"][0]["integrity"],
        "hook":"postinstall","script-digest":format!("sha256-{:x}", Sha256::digest(package["scripts"]["postinstall"].as_str().unwrap().as_bytes())),
        "system-toolchain":true,"dependencies":true,"read":["."],"write":["."],"network":false,"environment":{},
        "timeout-seconds":5,"max-output-bytes":1024,"max-processes":32,"max-memory-bytes":134217728,
        "tools":[{"name":if cfg!(windows) {"cmd"} else {"sh"},"path":shell,"digest":format!("sha256-{:x}", Sha256::digest(fs::read(&shell).unwrap()))}]
    }]})).unwrap();
    project
        .write("tapid.lifecycle.toml", policy.as_bytes())
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args([
            "install",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(home.path().join("store"))
        .env_remove("PATH")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot read live workspace packages")
    );
    assert!(
        project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .is_file()
    );
}

#[test]
fn dependency_review_regression_implicit_node_gyp_uses_private_bundled_modules() {
    let Some(node) = node_assertion_output(
        Command::new("node").args(["-p", "process.execPath"]),
        std::env::var_os("TAPID_REQUIRE_NODE_ASSERTIONS").is_some(),
        "implicit-node-gyp",
    ) else {
        return;
    };
    assert!(node.status.success());
    let node = PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
    let project = tapid_test_support::TempProject::new("implicit-node-gyp").unwrap();
    let home = tapid_test_support::TempHome::new("implicit-node-gyp").unwrap();
    let runtime = tapid_test_support::TempProject::new("node-gyp-runtime").unwrap();
    runtime.write("bin/placeholder", b"").unwrap();
    let node_name = if cfg!(windows) { "node.exe" } else { "node" };
    fs::copy(node, runtime.path().join("bin").join(node_name)).unwrap();
    runtime
        .write(
            "lib/node_modules/npm/package.json",
            br#"{"name":"npm","version":"1.0.0"}"#,
        )
        .unwrap();
    runtime
        .write(
            "lib/node_modules/npm/node_modules/node-gyp/package.json",
            br#"{"name":"node-gyp","version":"1.0.0"}"#,
        )
        .unwrap();
    runtime.write("lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js", br#"const fs = require('node:fs'); if (process.argv[2] !== 'rebuild') throw Error('wrong arguments'); if (!__filename.includes('.tools')) throw Error('host tool executed'); fs.writeFileSync('BUILT', require('fixture-helper'));"#).unwrap();
    runtime
        .write(
            "lib/node_modules/npm/node_modules/fixture-helper/index.js",
            b"module.exports = 'private-toolchain';",
        )
        .unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"}}"#,
        )
        .unwrap();
    write_dependency_script_fixture(
        &project,
        "H4sIAAAAAAAAE+3TTQrCMBAF4B5FZq3pS61d9DbRhhClk9DWghTvLv4spAhuahWcb/OG2cxsXjS7g3E2jfdU+zZwMjEAKPL8lgDGCWye5ute6yLTyQJTP/LKse1Mk2CWWz9oIDa1pZLYdL63q8rWgZbU26b1gakkraBA52//KT7j0ft067ny7JQ7xclvvO0/snH/82It/Z/DIM0WQoi/dAELlQxvAAwAAA==",
    );
    let install = || {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .args([
                "install",
                "--allow-unapproved-dependency-scripts",
                "--unsafe-no-dependency-sandbox",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
            ])
            .arg(home.path().join("store"))
            .env("PATH", runtime.path().join("bin"))
            .output()
            .unwrap()
    };
    let output = install();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/native-demo/BUILT")).unwrap(),
        b"private-toolchain"
    );
    assert!(!home.path().join("store/.tapid-lifecycle-key").exists());
    fs::remove_dir_all(runtime.path().join("lib/node_modules/npm")).unwrap();
    let output = install();
    assert!(
        !output.status.success(),
        "missing bundled toolchain must fail before executing an arbitrary PATH tool"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("node-gyp"));
    assert_eq!(
        fs::read(project.path().join("node_modules/native-demo/BUILT")).unwrap(),
        b"private-toolchain"
    );
}

#[test]
fn dependency_lifecycle_overrides_are_explicit_and_do_not_create_attestations() {
    let project = tapid_test_support::TempProject::new("dependency-hooks-override").unwrap();
    let home = tapid_test_support::TempHome::new("dependency-hooks-override").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"}}"#,
        )
        .unwrap();
    let archive = "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=";
    write_dependency_script_fixture(&project, archive);
    let install = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .current_dir(project.path())
            .args([
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
            ])
            .arg(home.path().join("store"))
            .args(flags)
            .env_remove("PATH")
            .output()
            .unwrap()
    };
    let output = install(&["--unsafe-no-dependency-sandbox"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .exists()
    );
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
        let output = install(&["--allow-unapproved-dependency-scripts"]);
        assert!(
            !output.status.success(),
            "approval override must retain containment"
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_before
        );
        assert!(
            !project
                .path()
                .join("node_modules/native-demo/SHOULD_NOT_EXIST")
                .exists()
        );
    }
    #[cfg(windows)]
    {
        let output = install(&["--allow-unapproved-dependency-scripts"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("without containment"));
        assert!(
            project
                .path()
                .join("node_modules/native-demo/SHOULD_NOT_EXIST")
                .is_file()
        );
        assert!(!home.path().join("store/.tapid-lifecycle-key").exists());
    }
    let output = install(&[
        "--allow-unapproved-dependency-scripts",
        "--unsafe-no-dependency-sandbox",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("3 hook(s) executed, 0 authenticated output(s) reused")
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("without containment"));
    assert!(
        project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .is_file()
    );
    assert!(!home.path().join("store/.tapid-lifecycle-key").exists());
    let lock = Lockfile::from_json(&fs::read_to_string(project.path().join("tapid.lock")).unwrap())
        .unwrap();
    assert!(
        lock.packages()
            .values()
            .all(|p| p.derived_hooks().is_empty())
    );
    let output = install(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .exists()
    );
    let output = install(&[
        "--allow-unapproved-dependency-scripts",
        "--unsafe-no-dependency-sandbox",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["lifecycle"]["hooks_executed"], 3);
    assert_eq!(result["lifecycle"]["generated_outputs_reused"], 0);
    let hooks = result["lifecycle"]["packages"][0]["hooks"]
        .as_array()
        .unwrap();
    assert_eq!(
        hooks
            .iter()
            .filter(
                |h| h["authorization"] == "invocation_override" && h["disposition"] == "executed"
            )
            .count(),
        3
    );
    let text = result.to_string();
    assert!(text.contains("DEPENDENCY_SCRIPTS_WITHOUT_CONTAINMENT"));
    assert!(text.contains("UNAPPROVED_DEPENDENCY_SCRIPTS_ALLOWED"));
    for flags in [
        vec!["--allow-unapproved-dependency-scripts", "--offline"],
        vec!["--unsafe-no-dependency-sandbox", "--frozen"],
        vec![
            "--unsafe-no-dependency-sandbox",
            "--allow-unverified-registry-artifacts",
        ],
    ] {
        let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
        assert!(!install(&flags).status.success());
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_before
        );
    }
}

#[test]
fn dependency_lifecycle_sandbox_only_override_skips_hooks_with_a_dependency_cycle() {
    let fixture = command_support::CliFixture::new("sandbox-only-cycle");
    let project = &fixture.project;
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"a":"1.0.0","b":"2.0.0","native-demo":"1.0.0"}}"#).unwrap();
    write_dependency_script_fixture(
        project,
        "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=",
    );
    let mut registry: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/aliased-root-cycle.json")).unwrap();
    let hook_registry: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("registry.json")).unwrap()).unwrap();
    registry["packages"].as_array_mut().unwrap().extend(
        hook_registry["packages"]
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    project
        .write("registry.json", &serde_json::to_vec(&registry).unwrap())
        .unwrap();
    let output = fixture
        .command(&[
            "install",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    for hook in ["preinstall", "install", "postinstall", "prepare"] {
        assert!(
            stderr.contains(&format!(
                "skipped dependency lifecycle hook {hook} for native-demo@1.0.0"
            )),
            "{stderr}"
        );
    }
    assert!(
        !project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .exists()
    );
    let lock = Lockfile::from_json(&fs::read_to_string(project.path().join("tapid.lock")).unwrap())
        .unwrap();
    assert!(
        lock.packages()
            .values()
            .all(|package| package.derived_hooks().is_empty())
    );
    assert!(project.path().join("node_modules/a/package.json").is_file());
    assert!(
        project
            .path()
            .join("node_modules/a/node_modules/b/package.json")
            .is_file()
    );
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let output = fixture
        .command(&[
            "install",
            "--allow-unapproved-dependency-scripts",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "authorized cyclic lifecycle graphs must still be rejected"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("approved dependency lifecycle graphs must be acyclic")
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
    assert!(
        !project
            .path()
            .join("node_modules/native-demo/SHOULD_NOT_EXIST")
            .exists()
    );
}

#[test]
fn dependency_lifecycle_sandbox_only_override_warns_on_discovery_failure() {
    let fixture = command_support::CliFixture::new("sandbox-only-discovery");
    let project = &fixture.project;
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"broken":"1.0.0"}}"#,
        )
        .unwrap();
    // The verified manifest is valid but exceeds the 1 MiB lifecycle discovery bound.
    // Skipping hooks must still allow installation; authorization retains that bound.
    project
        .write(
            "registry.json",
            include_bytes!("fixtures/dependency-discovery-oversized.json"),
        )
        .unwrap();
    let output = fixture
        .command(&[
            "install",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("could not discover dependency lifecycle hooks for broken@1.0.0"),
        "{stderr}"
    );
    assert!(
        !project
            .path()
            .join("node_modules/broken/SHOULD_NOT_EXIST")
            .exists()
    );
    assert!(
        project
            .path()
            .join("node_modules/broken/package.json")
            .is_file()
    );
    let output = fixture
        .command(&["install", "--offline", "--frozen", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["lifecycle"]["coverage"], "incomplete");
    assert_eq!(result["lifecycle"]["reason"], "discovery_failed");
    assert_eq!(result["lifecycle"]["packages"][0]["discovery"], "failed");
    assert_eq!(result["warning_details"][0]["package"]["name"], "broken");
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let output = fixture
        .command(&[
            "install",
            "--allow-unapproved-dependency-scripts",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "discovery limits must fail closed when execution is authorized"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("dependency package.json must be a regular file no larger than 1 MiB")
    );
    assert!(
        !project
            .path()
            .join("node_modules/broken/SHOULD_NOT_EXIST")
            .exists()
    );
    let fatal = fixture
        .command(&[
            "install",
            "--allow-unapproved-dependency-scripts",
            "--unsafe-no-dependency-sandbox",
            "--registry-fixture",
            "registry.json",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!fatal.status.success());
    let result: serde_json::Value = serde_json::from_slice(&fatal.stdout).unwrap();
    assert_eq!(result["lifecycle"]["packages"][0]["discovery"], "failed");
    assert_eq!(result["lifecycle"]["reason"], "discovery_failed");
    assert_eq!(result["lifecycle"]["hooks_executed"], 0);
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
}

#[test]
fn dependency_lifecycle_hooks_are_reported_and_never_run_by_default() {
    let project = tapid_test_support::TempProject::new("dependency-hooks-denied").unwrap();
    let home = tapid_test_support::TempHome::new("dependency-hooks-denied").unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0","second-demo":"1.0.0"},"scripts":{"install":"echo root > ROOT_SCRIPT_RAN"}}"#).unwrap();
    let archive = "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=";
    write_dependency_script_fixture(&project, archive);
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("registry.json")).unwrap()).unwrap();
    let second = "H4sIAC6v32oAA+3QwUoDMRCA4T5KyFnXLFsqePCkoCD20AreSsgOutomIdmKUPruZit4EA8Wi3r4v8uEyQwzTLTu2T7ISXyP1VMOfnRgppiMx7tYfI6mOW0+3rt8XU+aeqTMoRf5yjr3NpXxvzHrH9pob1eiz5TO4oJvj1tZBX2k9Iuk3AU//NSVqcyQyy51sc8lt9ExSefL7ZbLoUTcY1DyKm7dS6vO1exqendzsbidzheX99ez+dC9Z3kMud+3JUm0Sb5Vvt3+9ekBAAAAAAAAAAAAAAAA4EfeACIMKbwAKAAA";
    registry["packages"].as_array_mut().unwrap().push(serde_json::json!({
        "registry":"https://registry.npmjs.org", "name":"second-demo", "version":"1.0.0",
        "integrity": format!("sha512-{}", STANDARD.encode(Sha512::digest(STANDARD.decode(second).unwrap()))),
        "artifact": format!("base64:{second}")
    }));
    project
        .write("registry.json", &serde_json::to_vec(&registry).unwrap())
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
        assert!(
            !project
                .path()
                .join("node_modules/second-demo/SHOULD_NOT_EXIST")
                .exists()
        );
        assert!(!project.path().join("ROOT_SCRIPT_RAN").exists());
        assert!(!project.path().join("SHOULD_NOT_EXIST").exists());
        assert!(
            !project
                .path()
                .join("node_modules/native-demo/SHOULD_NOT_EXIST")
                .exists()
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["install", "--offline", "--frozen", "--json", "--store-dir"])
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["warnings"],
        serde_json::json!(["DEPENDENCY_LIFECYCLE_HOOK_SKIPPED"])
    );
    assert_eq!(result["lifecycle"]["coverage"], "complete");
    assert_eq!(result["lifecycle"]["packages"][0]["discovery"], "succeeded");
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["authorization"],
        "not_approved"
    );
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["disposition"],
        "skipped"
    );
    let details = result["warning_details"]
        .as_array()
        .expect("structured warning occurrences");
    assert_eq!(details.len(), 8);
    for package in ["native-demo", "second-demo"] {
        for hook in ["preinstall", "install", "postinstall", "prepare"] {
            assert!(
                details
                    .iter()
                    .any(|detail| detail["package"]["name"] == package && detail["hook"] == hook)
            );
        }
    }
    for detail in details {
        assert!(
            ["native-demo", "second-demo"].contains(&detail["package"]["name"].as_str().unwrap())
        );
        assert_eq!(detail["package"]["version"], "1.0.0");
        assert!(detail["package"]["key"].as_str().unwrap().contains("-demo"));
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
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["install", "--offline", "--frozen", "--json", "--store-dir"])
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"], serde_json::Value::Null);
    assert_eq!(result["lifecycle"]["coverage"], "incomplete");
    assert_eq!(result["lifecycle"]["hooks_skipped"], 1);
    assert_eq!(result["warning_details"][0]["hook"], "preinstall");
    let hooks = result["lifecycle"]["packages"][0]["hooks"]
        .as_array()
        .unwrap();
    assert!(hooks.iter().any(|h| h["hook"] == "install"
        && h["authorization"] == "rejected"
        && h["reason"] == "approval_rejected"));
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
        {
            assert!(!crash.status.success());
        }
        let journal_path = dir.join(".tapid-lifecycle-journal.json");
        let pending_journal = fs::read(&journal_path).unwrap();
        let inspection = run(
            &dir,
            &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
        );
        assert!(!inspection.status.success());
        assert!(String::from_utf8_lossy(&inspection.stderr).contains("recovery required"));
        assert_eq!(fs::read(&journal_path).unwrap(), pending_journal);
        let recovery = run_with_env(
            &dir,
            &[
                "install",
                "--offline",
                "--store-dir",
                cold.to_str().unwrap(),
            ],
            "TAPID_TEST_RECOVER_ONLY",
            "1",
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
    let first = install(&["--registry-fixture", "registry.json", "--json"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(result["lifecycle"]["hooks_executed"], 1);
    assert_eq!(result["lifecycle"]["generated_outputs_reused"], 0);
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["authorization"],
        "exact_policy"
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
            .args(["ci", "--json"])
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
        let result: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
        assert_eq!(result["lifecycle"]["hooks_executed"], 0);
        assert_eq!(result["lifecycle"]["generated_outputs_reused"], 1);
        assert_eq!(
            result["lifecycle"]["packages"][0]["hooks"][0]["disposition"],
            "reused"
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
    let failed = install(&["--offline", "--frozen", "--json"]);
    assert!(!failed.status.success());
    let result: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(result["lifecycle"]["hooks_executed"], 0);
    assert_eq!(
        result["lifecycle"]["packages"][0]["hooks"][0]["reason"],
        "authenticated_output_missing_or_mismatched"
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

#[test]
fn dist_tags_pin_prereleases_and_replay_after_tags_move_or_disappear() {
    let project = tapid_test_support::TempProject::new("dist-tags").unwrap();
    let dir = project.path();
    fs::write(dir.join("package.json"), r#"{"name":"demo","version":"1.0.0","dependencies":{"local":"npm:h3@next","@local/direct":"npm:@actual/scoped@latest"}}"#).unwrap();
    let mut fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    for package in fixture["packages"].as_array_mut().unwrap() {
        if package["name"] == "h3" && package["version"] == "2.0.1-rc.20" {
            package["dist_tags"] = serde_json::json!(["next"]);
        }
        if package["name"] == "@actual/scoped" {
            package["dist_tags"] = serde_json::json!(["latest"]);
        }
    }
    let fixture_path = dir.join("registry.json");
    fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let store = dir.join("store");
    let install = run(
        dir,
        &[
            "install",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let mut pinned: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
    for package in pinned["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = serde_json::json!(format!(
            "https://registry.npmjs.org/{}/-/fixture.tgz",
            package["name"].as_str().unwrap()
        ));
    }
    fs::write(dir.join("tapid.lock"), serde_json::to_vec(&pinned).unwrap()).unwrap();
    let locked = fs::read(dir.join("tapid.lock")).unwrap();
    for package in fixture["packages"].as_array_mut().unwrap() {
        package.as_object_mut().unwrap().remove("dist_tags");
        if package["name"] == "h3" && package["version"] == "2.0.0" {
            package["dist_tags"] = serde_json::json!(["next"]);
        }
    }
    fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    for flags in [
        vec!["install"],
        vec!["install", "--frozen"],
        vec!["install", "--offline"],
        vec!["ci", "--offline"],
    ] {
        fs::remove_dir_all(dir.join("node_modules")).unwrap();
        let mut args = flags;
        args.extend([
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ]);
        let output = run(dir, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
        assert!(
            fs::read_to_string(dir.join("node_modules/local/index.js"))
                .unwrap()
                .contains("prerelease")
        );
        assert!(
            dir.join("node_modules/.bin/scoped-tool").exists()
                || dir.join("node_modules/.bin/scoped-tool.cmd").exists()
        );
    }
    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let cold_store = dir.join("cold-store");
    let hydrate = run(
        dir,
        &[
            "install",
            "--frozen",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            cold_store.to_str().unwrap(),
        ],
    );
    assert!(
        hydrate.status.success(),
        "{}",
        String::from_utf8_lossy(&hydrate.stderr)
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
    assert!(
        fs::read_to_string(dir.join("node_modules/local/index.js"))
            .unwrap()
            .contains("prerelease")
    );
    let outdated = run(
        dir,
        &[
            "outdated",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        outdated.status.success(),
        "{}",
        String::from_utf8_lossy(&outdated.stderr)
    );
    assert!(
        String::from_utf8_lossy(&outdated.stdout)
            .contains("declared=npm:h3@next locked=2.0.1-rc.20 compatible=2.0.0")
    );
    let before = fs::read(dir.join("package.json")).unwrap();
    let output = run(
        dir,
        &[
            "update",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("latest"));
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
    assert_eq!(fs::read(dir.join("package.json")).unwrap(), before);
    for package in fixture["packages"].as_array_mut().unwrap() {
        if package["name"] == "@actual/scoped" {
            package["dist_tags"] = serde_json::json!(["latest"]);
        }
    }
    fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let update = run(
        dir,
        &[
            "update",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    let updated_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("package.json")).unwrap()).unwrap();
    let original_manifest: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        updated_manifest["dependencies"],
        original_manifest["dependencies"]
    );
    assert!(
        fs::read_to_string(dir.join("node_modules/local/package.json"))
            .unwrap()
            .contains("2.0.0")
    );
}

#[test]
fn copied_file_tarballs_keep_source_identity_and_reject_changed_bytes_on_replay() {
    use base64::Engine;
    let project = tapid_test_support::TempProject::new("file-tarballs").unwrap();
    let dir = project.path();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    let package = fixture["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "h3" && package["version"] == "1.0.0")
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            package["artifact"]
                .as_str()
                .unwrap()
                .strip_prefix("base64:")
                .unwrap(),
        )
        .unwrap();
    project.write("vendor/pkg.tgz", &bytes).unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"copied":"file:./vendor/pkg.tgz","registry":"npm:h3@1"}}"#).unwrap();
    let fixture_path = project
        .write(
            "registry.json",
            serde_json::to_vec(&fixture).unwrap().as_slice(),
        )
        .unwrap();
    let store = dir.join("store");
    let output = run(
        dir,
        &[
            "install",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let locked = fs::read(dir.join("tapid.lock")).unwrap();
    let lock = Lockfile::from_json(std::str::from_utf8(&locked).unwrap()).unwrap();
    assert_eq!(lock.packages().len(), 2);
    assert!(lock.root_bindings()["copied"].starts_with("file:vendor/pkg.tgz#sha256-"));
    let why = run(dir, &["--json", "why", "h3"]);
    assert!(
        why.status.success(),
        "{}",
        String::from_utf8_lossy(&why.stderr)
    );
    let explanation: serde_json::Value = serde_json::from_slice(&why.stdout).unwrap();
    let copied = explanation["data"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .find(|path| path["steps"][0]["key"] == lock.root_bindings()["copied"])
        .expect("why must retain the copied source path alongside the registry path");
    assert_eq!(copied["steps"][0]["sourceKind"], "copied");
    let why = run(dir, &["why", "h3"]);
    assert!(
        why.status.success(),
        "{}",
        String::from_utf8_lossy(&why.stderr)
    );
    assert!(String::from_utf8_lossy(&why.stdout).contains("file:vendor/pkg.tgz#sha256-"));
    for flags in [
        vec!["install", "--offline", "--frozen"],
        vec!["ci", "--offline"],
    ] {
        fs::remove_dir_all(dir.join("node_modules")).unwrap();
        let mut args = flags;
        args.extend([
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ]);
        let output = run(dir, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
        assert_eq!(
            fs::read(dir.join("node_modules/copied/index.js")).unwrap(),
            fs::read(dir.join("node_modules/registry/index.js")).unwrap()
        );
    }
    fs::write(dir.join("vendor/pkg.tgz"), b"changed").unwrap();
    let output = run(
        dir,
        &[
            "install",
            "--offline",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("digest mismatch"));
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
    assert!(dir.join("node_modules/copied/index.js").is_file());
}

#[test]
fn pinned_git_dependencies_replay_exact_commits_after_refs_move() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let project = tapid_test_support::TempProject::new("pinned-git").unwrap();
    let dir = project.path();
    let original: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    let mut packages = Vec::new();
    for (version, commit, reference) in [
        ("1.0.0", "a".repeat(40), Some("main")),
        ("2.0.0", "b".repeat(40), None),
    ] {
        let mut package = original["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|package| package["name"] == "h3" && package["version"] == version)
            .unwrap()
            .clone();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                package["artifact"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64:")
                    .unwrap(),
            )
            .unwrap();
        let digest = format!("sha256-{:x}", Sha256::digest(&bytes));
        package["registry"] = serde_json::json!(format!(
            "git+https://example.test/repo.git#{commit}!{digest}"
        ));
        if let Some(reference) = reference {
            package["git_ref"] = serde_json::json!(reference);
        }
        packages.push(package);
    }
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"copied":"git+https://example.test/repo.git#main","untouched":"git+https://example.test/repo.git#main"}}"#).unwrap();
    let fixture_path = project
        .write(
            "registry.json",
            &serde_json::to_vec(&serde_json::json!({"packages":packages})).unwrap(),
        )
        .unwrap();
    let store = dir.join("store");
    let install = run(
        dir,
        &[
            "install",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let locked = fs::read(dir.join("tapid.lock")).unwrap();
    let lock = Lockfile::from_json(std::str::from_utf8(&locked).unwrap()).unwrap();
    assert!(lock.root_bindings()["copied"].contains(&"a".repeat(40)));
    packages[0].as_object_mut().unwrap().remove("git_ref");
    packages[1]["git_ref"] = serde_json::json!("main");
    fs::write(
        &fixture_path,
        serde_json::to_vec(&serde_json::json!({"packages":packages})).unwrap(),
    )
    .unwrap();
    for flags in [
        vec!["install"],
        vec!["install", "--offline", "--frozen"],
        vec!["ci", "--offline"],
    ] {
        fs::remove_dir_all(dir.join("node_modules")).unwrap();
        let mut args = flags;
        args.extend([
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ]);
        let output = run(dir, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
        assert!(
            fs::read_to_string(dir.join("node_modules/copied/package.json"))
                .unwrap()
                .contains("1.0.0")
        );
    }
    for command in ["install", "ci"] {
        let cold = dir.join(format!("cold-{command}"));
        let mut args = vec![command];
        if command == "install" {
            args.push("--frozen");
        }
        args.extend([
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            cold.to_str().unwrap(),
        ]);
        let output = run(dir, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
        assert!(
            fs::read_to_string(dir.join("node_modules/copied/package.json"))
                .unwrap()
                .contains("1.0.0")
        );
    }
    // A named Git refresh must preserve the other binding after the reference moves.
    let update = run(
        dir,
        &[
            "update",
            "copied",
            "--latest",
            "--save-exact",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert!(
        fs::read_to_string(dir.join("node_modules/copied/package.json"))
            .unwrap()
            .contains("2.0.0")
    );
    assert!(
        fs::read_to_string(dir.join("tapid.lock"))
            .unwrap()
            .contains(&"b".repeat(40))
    );
    let named_lock =
        Lockfile::from_json(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(named_lock.root_bindings()["untouched"].contains(&"a".repeat(40)));
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        saved["dependencies"]["copied"],
        "git+https://example.test/repo.git#main"
    );
    let updated_lock = fs::read(dir.join("tapid.lock")).unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"copied":"git+https://example.test/repo.git#missing-ref"}}"#).unwrap();
    let changed_reference = run(
        dir,
        &[
            "install",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        !changed_reference.status.success(),
        "a changed declaration must resolve its new reference"
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), updated_lock);
}

#[test]
fn copied_dependency_rejections_preserve_project_and_store_and_redact_credentials() {
    for spec in [
        "file:../outside.tgz",
        "file:/outside.tgz",
        "file:directory.tgz",
        "file:missing.tgz",
        "git+ssh://example.test/repo#main",
        "git+https://user:never-log-this@example.test/repo#main",
        "git+https://example.test/repo?token=never-log-this#main",
        "git+https://example.test/repo#semver:^1",
    ] {
        let project = tapid_test_support::TempProject::new("copied-rejection").unwrap();
        let dir = project.path();
        let manifest =
            serde_json::json!({"name":"demo","version":"1.0.0","dependencies":{"copied":spec}})
                .to_string();
        project.write("package.json", manifest.as_bytes()).unwrap();
        project.write("tapid.lock", b"previous lock").unwrap();
        project
            .write("node_modules/keep", b"previous install")
            .unwrap();
        fs::create_dir(dir.join("directory.tgz")).unwrap();
        let fixture = project
            .write("registry.json", br#"{"packages":[]}"#)
            .unwrap();
        let store = dir.join("store");
        let output = run(
            dir,
            &[
                "install",
                "--registry-fixture",
                fixture.to_str().unwrap(),
                "--store-dir",
                store.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success(), "{spec}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("copied"), "{error}");
        assert!(!error.contains("never-log-this"));
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), b"previous lock");
        assert_eq!(
            fs::read(dir.join("node_modules/keep")).unwrap(),
            b"previous install"
        );
        assert!(!store.exists());
    }
}

#[test]
fn copied_file_tarballs_materialize_bins_and_cold_replay() {
    let project = tapid_test_support::TempProject::new("file-bins").unwrap();
    let dir = project.path();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    let package = fixture["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "@actual/scoped")
        .unwrap();
    let bytes = STANDARD
        .decode(
            package["artifact"]
                .as_str()
                .unwrap()
                .strip_prefix("base64:")
                .unwrap(),
        )
        .unwrap();
    project.write("vendor/tool.tgz", &bytes).unwrap();
    project
        .write("package.json", br#"{"name":"demo","version":"1.0.0"}"#)
        .unwrap();
    let store = dir.join("store");
    let output = run(
        dir,
        &[
            "add",
            "local@file:vendor/tool.tgz",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let locked = fs::read(dir.join("tapid.lock")).unwrap();
    let manifest = fs::read(dir.join("package.json")).unwrap();
    for (command, flags) in [
        ("install", vec!["--offline", "--frozen"]),
        ("install", vec!["--frozen"]),
        ("ci", Vec::new()),
    ] {
        fs::remove_dir_all(dir.join("node_modules")).unwrap();
        let destination = if flags.contains(&"--offline") {
            store.clone()
        } else {
            dir.join(format!("cold-{command}"))
        };
        let mut args = vec![command];
        args.extend(flags);
        args.extend(["--store-dir", destination.to_str().unwrap()]);
        let replay = run(dir, &args);
        assert!(
            replay.status.success(),
            "{}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), locked);
        assert!(
            dir.join("node_modules/.bin/scoped-tool").exists()
                || dir.join("node_modules/.bin/scoped-tool.cmd").exists()
        );
        assert!(dir.join("node_modules/local/index.js").is_file());
    }
    let update = run(
        dir,
        &["update", "--latest", "--store-dir", store.to_str().unwrap()],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert_eq!(fs::read(dir.join("package.json")).unwrap(), manifest);
    let outdated = run(dir, &["outdated"]);
    assert!(
        outdated.status.success(),
        "{}",
        String::from_utf8_lossy(&outdated.stderr)
    );
    assert!(String::from_utf8_lossy(&outdated.stdout).contains("declared=file:vendor/tool.tgz"));
}

#[test]
fn copied_git_archives_use_the_same_safe_extraction_and_bin_lane() {
    use sha2::{Digest, Sha256};
    let project = tapid_test_support::TempProject::new("git-archive").unwrap();
    let repo = tapid_test_support::TempProject::new("git-archive-source").unwrap();
    repo.write(
        "package.json",
        br#"{"name":"git-tool","version":"1.0.0","bin":{"git-tool":"bin.js"},"dependencies":{"nested":"npm:h3@next"}}"#,
    )
    .unwrap();
    repo.write("bin.js", b"#!/usr/bin/env node\nconsole.log('fixture');\n")
        .unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .env_clear()
            .env("HOME", repo.path())
            .env("USERPROFILE", repo.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repo.path().join("empty-config"))
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "core.hooksPath=disabled-hooks",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    repo.write("empty-config", b"").unwrap();
    git(&["init", "--template="]);
    git(&["add", "package.json", "bin.js"]);
    git(&["commit", "-m", "Fixture"]);
    let commit = String::from_utf8(git(&["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let bytes = git(&["archive", "--format=tar", "--prefix=package/", "HEAD"]);
    let source = format!(
        "git+https://example.test/tool.git#{commit}!sha256-{:x}",
        Sha256::digest(&bytes)
    );
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"local":"git+https://example.test/tool.git#main"}}"#).unwrap();
    let original: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    let mut child = original["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "h3" && package["version"] == "2.0.1-rc.20")
        .unwrap()
        .clone();
    child["dist_tags"] = serde_json::json!(["next"]);
    let fixture = project.write("registry.json", &serde_json::to_vec(&serde_json::json!({"packages":[{"registry":source,"git_ref":"main","name":"git-tool","version":"1.0.0","artifact":format!("base64:{}", STANDARD.encode(&bytes)),"dependencies":{}}, child]})).unwrap()).unwrap();
    let store = project.path().join("store");
    let install = run(
        project.path(),
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        project.path().join("node_modules/.bin/git-tool").exists()
            || project
                .path()
                .join("node_modules/.bin/git-tool.cmd")
                .exists()
    );
    assert!(project.path().join("node_modules/local/bin.js").is_file());
    assert!(
        fs::read_to_string(project.path().join("node_modules/nested/index.js"))
            .unwrap()
            .contains("prerelease")
    );
    if let Some(node) = node_assertion_output(Command::new("node").args(["-e", "console.log(require(require.resolve('nested', {paths:[require('path').resolve('node_modules/local')]})))"]).current_dir(project.path()), std::env::var_os("TAPID_REQUIRE_NODE_ASSERTIONS").is_some(), "copied-git-transitive-alias") {
        assert!(node.status.success(), "{}", String::from_utf8_lossy(&node.stderr));
        assert_eq!(String::from_utf8_lossy(&node.stdout).trim(), "prerelease");
    }
    let locked = fs::read(project.path().join("tapid.lock")).unwrap();
    let mut altered: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture).unwrap()).unwrap();
    altered["packages"][0]["artifact"] =
        serde_json::json!(format!("base64:{}", STANDARD.encode(b"changed archive")));
    fs::write(&fixture, serde_json::to_vec(&altered).unwrap()).unwrap();
    let cold = project.path().join("cold-store");
    let replay = run(
        project.path(),
        &[
            "install",
            "--frozen",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            cold.to_str().unwrap(),
        ],
    );
    assert!(!replay.status.success());
    assert!(String::from_utf8_lossy(&replay.stderr).contains("digest mismatch"));
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), locked);
    assert!(project.path().join("node_modules/local/bin.js").is_file());
}

#[test]
fn copied_file_tarballs_reject_hostile_entries_without_replacing_the_project() {
    for (path, kind) in [("../escape", b'0'), ("package/link", b'2')] {
        let project = tapid_test_support::TempProject::new("file-hostile").unwrap();
        let mut header = [0u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[124..136].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = kind;
        header[157..169].copy_from_slice(b"../../escape");
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum = header.iter().map(|byte| *byte as u32).sum::<u32>();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let bytes = [header.as_slice(), &[0u8; 1024]].concat();
        project.write("vendor/hostile.tar", &bytes).unwrap();
        project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"local":"file:vendor/hostile.tar"}}"#).unwrap();
        project
            .write("node_modules/keep", b"previous install")
            .unwrap();
        let store = project.path().join("store");
        let install = run(
            project.path(),
            &["install", "--store-dir", store.to_str().unwrap()],
        );
        assert!(!install.status.success());
        assert!(String::from_utf8_lossy(&install.stderr).contains("ARCHIVE_INVALID"));
        assert_eq!(
            fs::read(project.path().join("node_modules/keep")).unwrap(),
            b"previous install"
        );
        assert!(!project.path().join("tapid.lock").exists());
        assert!(!project.path().join("escape").exists());
    }
}

#[cfg(unix)]
#[test]
fn copied_file_paths_cannot_follow_symlink_components() {
    let project = tapid_test_support::TempProject::new("file-symlink").unwrap();
    let outside = tapid_test_support::TempProject::new("file-symlink-target").unwrap();
    outside.write("tool.tgz", b"archive").unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join("vendor")).unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"local":"file:vendor/tool.tgz"}}"#,
        )
        .unwrap();
    let store = project.path().join("store");
    let install = run(
        project.path(),
        &["install", "--store-dir", store.to_str().unwrap()],
    );
    assert!(!install.status.success());
    assert!(String::from_utf8_lossy(&install.stderr).contains("symlink"));
    assert!(!store.exists());
    assert_eq!(
        fs::read(outside.path().join("tool.tgz")).unwrap(),
        b"archive"
    );
}

#[test]
fn dist_tag_workspace_peers_validate_exact_targets_during_resolution_and_replay() {
    let project = tapid_test_support::TempProject::new("workspace-tag-peer").unwrap();
    let manifest = br#"{"name":"demo","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"h3":"1.0.0","member":"workspace:*"}}"#;
    project.write("package.json", manifest).unwrap();
    project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"1.0.0","peerDependencies":{"h3":"next"}}"#,
        )
        .unwrap();
    let mut fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    for package in fixture["packages"].as_array_mut().unwrap() {
        if package["name"] == "h3" && package["version"] == "1.0.0" {
            package["dist_tags"] = serde_json::json!(["next"]);
        }
    }
    let fixture_path = project
        .write("registry.json", &serde_json::to_vec(&fixture).unwrap())
        .unwrap();
    let store = project.path().join("store");
    let args = [
        "install",
        "--registry-fixture",
        fixture_path.to_str().unwrap(),
        "--store-dir",
        store.to_str().unwrap(),
    ];
    let installed = run(project.path(), &args);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    project
        .write("package.json", &[manifest.as_slice(), b"\n"].concat())
        .unwrap();
    let changed = run(project.path(), &args);
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let locked = fs::read(project.path().join("tapid.lock")).unwrap();
    for package in fixture["packages"].as_array_mut().unwrap() {
        package.as_object_mut().unwrap().remove("dist_tags");
        if package["name"] == "h3" && package["version"] == "2.0.0" {
            package["dist_tags"] = serde_json::json!(["next"]);
        }
    }
    fs::write(&fixture_path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let replay = run(
        project.path(),
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
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), locked);
    let refresh = run(
        project.path(),
        &[
            "update",
            "--registry-fixture",
            fixture_path.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!refresh.status.success());
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), locked);
}

#[test]
fn why_traverses_a_persisted_peer_edge_without_treating_manifest_peer_as_a_root() {
    use tapid_core::PeerContext;
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-persisted-peer-edge").unwrap();
    let manifest = br#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"},"peerDependencies":{"peer":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    let mut lock = lock_for_manifest(std::str::from_utf8(manifest).unwrap());
    let peer_context =
        PeerContext::default().with("peer".parse().unwrap(), "1.0.0".parse().unwrap());
    let plugin = LockedPackage::new_with_context_and_provenance(
        "https://registry.npmjs.org",
        "plugin",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        (
            &peer_context,
            &tapid_core::PlatformContext::new(None, None, None).unwrap(),
        ),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let peer = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "peer",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "d".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let plugin_key = plugin.key();
    lock.insert_packages([plugin, peer]).unwrap();
    lock.set_roots([plugin_key.clone()]).unwrap();
    lock.set_root_bindings(std::collections::BTreeMap::from([(
        "plugin".to_owned(),
        plugin_key,
    )]))
    .unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();

    let text = run(project.path(), &["why", "peer"]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("peer (peer)"));

    let json = run(project.path(), &["why", "peer", "--json"]);
    assert!(json.status.success());
    let result: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(result["data"]["paths"][0]["steps"][1]["edgeKind"], "peer");
    assert_eq!(result["data"]["paths"][0]["steps"][1]["via"], "peer");
    assert_eq!(result["data"]["graph_warnings"], serde_json::json!([]));
    assert_eq!(result["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
}

#[test]
fn why_warns_when_generic_transitive_edge_kinds_are_unavailable() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-generic-transitive-warning").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"app":"1.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let mut app = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "app",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let nested = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "nested",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "b".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    app.add_dependency("nested", &nested.key()).unwrap();
    let app_key = app.key();
    lock.insert_packages([app, nested]).unwrap();
    lock.set_roots([app_key.clone()]).unwrap();
    lock.set_root_bindings(std::collections::BTreeMap::from([(
        "app".to_owned(),
        app_key,
    )]))
    .unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();

    let output = run(project.path(), &["why", "nested", "--json"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["data"]["graph_warnings"],
        serde_json::json!([
            "transitive dependency edge kinds are not recorded in the lockfile; those edges are labeled dependency"
        ])
    );
}

#[test]
fn why_rejects_a_legacy_root_outside_the_manifest_version_range() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-stale-legacy-root").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"app":"^2.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let mut app = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "app",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let target = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "target",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "b".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    app.add_dependency("target", &target.key()).unwrap();
    let app_key = app.key();
    lock.insert_packages([app, target]).unwrap();
    lock.set_roots([app_key]).unwrap();
    let lock_bytes = lock.to_json().unwrap();
    project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();

    let output = run(project.path(), &["why", "target", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], "failure");
    assert_eq!(result["errors"][0]["code"], "LOCKFILE_INVALID");
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_bytes.as_bytes()
    );
}

#[test]
fn why_rejects_a_stale_recorded_root_binding() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-stale-root-binding").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"app":"^2.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let app = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "app",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let app_key = app.key();
    lock.insert_packages([app]).unwrap();
    lock.set_roots([app_key.clone()]).unwrap();
    lock.set_root_bindings(std::collections::BTreeMap::from([(
        "app".to_owned(),
        app_key,
    )]))
    .unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();

    let output = run(project.path(), &["why", "app", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], "failure");
    assert_eq!(result["errors"][0]["code"], "LOCKFILE_INVALID");
}

#[test]
fn why_reports_an_incomplete_empty_search_as_success() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-truncated-empty").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"package-0":"1.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let mut packages = (0..=129)
        .map(|index| {
            LockedPackage::new_with_provenance(
                "https://registry.npmjs.org",
                &format!("package-{index}"),
                "1.0.0",
                &format!("sha512-{}==", "A".repeat(86)),
                &format!("sha256-{index:064x}"),
                RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let keys = packages.iter().map(LockedPackage::key).collect::<Vec<_>>();
    for index in 0..129 {
        packages[index]
            .add_dependency(&format!("package-{}", index + 1), &keys[index + 1])
            .unwrap();
    }
    let root_key = packages[0].key();
    lock.insert_packages(packages).unwrap();
    lock.set_roots([root_key.clone()]).unwrap();
    lock.set_root_bindings(std::collections::BTreeMap::from([(
        "package-0".to_owned(),
        root_key,
    )]))
    .unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();

    let output = run(project.path(), &["why", "missing"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No complete path to 'missing' was found before the search safety bound"),
        "{stdout}"
    );
    assert!(stdout.contains("result is incomplete"), "{stdout}");
}

#[test]
fn imported_recorded_omissions_replay_without_restoring_absent_placements() {
    use tapid_lockfile::*;
    let project = tapid_test_support::TempProject::new("imported-recorded-omissions").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","optionalDependencies":{"native":"^1"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{"":serde_json::from_str::<serde_json::Value>(manifest).unwrap()}});
    let mut lock = ImportedNpmLockfile::import(
        &npm.to_string(),
        manifest,
        lock_for_manifest(manifest).root_manifest_digest(),
    )
    .unwrap();
    lock.set_placement_selection(ImportedSelection {
        target: SelectionTarget {
            os: std::env::consts::OS.into(),
            cpu: std::env::consts::ARCH.into(),
            libc: if cfg!(all(target_os = "linux", target_env = "gnu")) {
                Some("glibc".into())
            } else if cfg!(all(target_os = "linux", target_env = "musl")) {
                Some("musl".into())
            } else {
                None
            },
        },
        profile: SelectionProfile::Development,
        omissions: vec![ImportedOmission {
            declaring_placement: "".into(),
            name: "native".into(),
            requirement: "^1".into(),
            reason: OmissionReason::MetadataAbsent,
        }],
    })
    .unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let verified = run(project.path(), &["lock", "verify", "--json"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stdout)
    );
    for args in [
        vec!["install", "--offline", "--json"],
        vec!["install", "--frozen", "--json"],
    ] {
        let output = run(project.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let wire: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(wire["selection"]["omissions"][0]["declaringPlacement"], "");
        assert_eq!(wire["selection"]["omissions"][0]["name"], "native");
        let restored = ImportedNpmLockfile::from_json(
            &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
        )
        .unwrap();
        assert_eq!(restored.placement_selection(), lock.placement_selection());
    }
    let bytes = fs::read_to_string(project.path().join("tapid.lock")).unwrap();
    let marker = fs::read(project.path().join(".tapid-managed")).unwrap();
    let ci = run(project.path(), &["ci", "--offline", "--json"]);
    assert!(!ci.status.success());
    assert_eq!(
        fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
        bytes
    );
    for field in ["profile", "os"] {
        let mut wire: serde_json::Value = serde_json::from_str(&bytes).unwrap();
        if field == "profile" {
            wire["placementSelection"][field] = "production".into();
        } else {
            wire["placementSelection"]["target"][field] = "different_os".into();
        }
        let changed = serde_json::to_string(&wire).unwrap();
        project.write("tapid.lock", changed.as_bytes()).unwrap();
        let output = run(project.path(), &["install", "--offline", "--json"]);
        assert!(!output.status.success(), "{field}");
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            changed
        );
        assert_eq!(
            fs::read(project.path().join(".tapid-managed")).unwrap(),
            marker
        );
    }
}

#[test]
fn named_update_preserves_unrelated_optional_root_when_metadata_disappears() {
    let project = tapid_test_support::TempProject::new("optional-update-scope").unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"@actual/scoped":"1.2.0"},"optionalDependencies":{"h3":"1.0.0"}}"#).unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    project
        .write("registry.json", registry.to_string().as_bytes())
        .unwrap();
    let installed = run(
        project.path(),
        &["install", "--registry-fixture", "registry.json"],
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    registry["packages"]
        .as_array_mut()
        .unwrap()
        .retain(|package| package["name"] != "h3");
    project
        .write("registry.json", registry.to_string().as_bytes())
        .unwrap();
    let updated = run(
        project.path(),
        &[
            "update",
            "@actual/scoped",
            "--registry-fixture",
            "registry.json",
        ],
    );
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    assert!(
        project
            .path()
            .join("node_modules/h3/package.json")
            .is_file()
    );
    let lock = Lockfile::from_json(&fs::read_to_string(project.path().join("tapid.lock")).unwrap())
        .unwrap();
    assert!(lock.root_bindings().contains_key("h3"));
}

#[test]
fn install_records_absent_optional_root_but_preserves_required_overlap() {
    for required in [false, true] {
        let project = tapid_test_support::TempProject::new("optional-root-selection").unwrap();
        let mut manifest = serde_json::json!({"name":"demo","version":"1.0.0","optionalDependencies":{"local":"npm:@vendor/native@^1"}});
        if required {
            manifest["dependencies"] = serde_json::json!({"other":"npm:@vendor/native@^1"});
        }
        project
            .write("package.json", manifest.to_string().as_bytes())
            .unwrap();
        project
            .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
            .unwrap();
        let output = run(
            project.path(),
            &["install", "--registry-fixture", "registry.json", "--json"],
        );
        assert_eq!(
            output.status.success(),
            !required,
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        if required {
            assert!(!project.path().join("tapid.lock").exists());
        } else {
            let bytes = fs::read(project.path().join("tapid.lock")).unwrap();
            let lock: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(lock["selection"]["omissions"][0]["name"], "local");
            assert_eq!(
                lock["selection"]["omissions"][0]["reason"],
                "metadataAbsent"
            );
            let replay = run(project.path(), &["ci", "--offline", "--json"]);
            assert!(
                replay.status.success(),
                "{}",
                String::from_utf8_lossy(&replay.stdout)
            );
            assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
        }
    }
}

#[test]
fn install_records_unusable_optional_roots_without_weakening_required_aliases() {
    for (case, reason) in [
        ("platform", "incompatiblePlatform"),
        ("range", "noUsableCandidate"),
        ("unsupported-child", "noUsableCandidate"),
    ] {
        for required in [false, true] {
            let project = tapid_test_support::TempProject::new("optional-root-candidates").unwrap();
            let requirement = if case == "range" {
                "npm:h3@^9"
            } else {
                "npm:h3@1.0.0"
            };
            let mut manifest = serde_json::json!({"name":"demo","version":"1.0.0","optionalDependencies":{"local":requirement}});
            if required {
                manifest["dependencies"] = serde_json::json!({"other":requirement});
            }
            project
                .write("package.json", manifest.to_string().as_bytes())
                .unwrap();
            let mut registry: serde_json::Value =
                serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
            for package in registry["packages"].as_array_mut().unwrap() {
                if package["name"] == "h3" {
                    if case == "platform" {
                        package["os"] = serde_json::json!(["tapid-incompatible-test-os"]);
                    } else if case == "unsupported-child" {
                        package["dependencies"] =
                            serde_json::json!({"child":"git+ssh://example.test/child"});
                    }
                }
            }
            project
                .write("registry.json", registry.to_string().as_bytes())
                .unwrap();
            let output = run(
                project.path(),
                &["install", "--registry-fixture", "registry.json", "--json"],
            );
            assert_eq!(
                output.status.success(),
                !required,
                "{case}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            if required {
                assert!(!project.path().join("tapid.lock").exists());
                assert!(!project.path().join(".tapid-managed").exists());
                continue;
            }
            let bytes = fs::read(project.path().join("tapid.lock")).unwrap();
            let lock: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                lock["selection"]["omissions"][0]["reason"], reason,
                "{case}"
            );
            assert!(!project.path().join("node_modules/local").exists());
            // Recovery of metadata must not cause cache-dependent reselection.
            project
                .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
                .unwrap();
            for args in [
                vec!["ci", "--offline", "--json"],
                vec!["install", "--frozen", "--json"],
            ] {
                let replay = run(project.path(), &args);
                assert!(
                    replay.status.success(),
                    "{}",
                    String::from_utf8_lossy(&replay.stdout)
                );
                assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
            }
        }
    }
}

#[test]
fn optional_root_candidate_normalization_preserves_supported_overrides() {
    let project = tapid_test_support::TempProject::new("optional-root-overrides").unwrap();
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","optionalDependencies":{"h3":"1.0.0"},"overrides":{"@actual/scoped":"1.2.0"}}"#).unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    for package in registry["packages"].as_array_mut().unwrap() {
        if package["name"] == "h3" {
            package["dependencies"] =
                serde_json::json!({"@actual/scoped":"git+ssh://example.test/child"});
        }
    }
    project
        .write("registry.json", registry.to_string().as_bytes())
        .unwrap();
    let output = run(
        project.path(),
        &["install", "--registry-fixture", "registry.json", "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        project
            .path()
            .join("node_modules/h3/package.json")
            .is_file()
    );
    let lock = Lockfile::from_json(&fs::read_to_string(project.path().join("tapid.lock")).unwrap())
        .unwrap();
    assert!(lock.root_bindings().contains_key("h3"));
}

#[test]
fn install_records_transitive_optional_metadata_absence_for_replay() {
    let project = tapid_test_support::TempProject::new("optional-first-selection").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-aliases.json")).unwrap();
    registry["packages"][0]["optionalDependencies"] =
        serde_json::json!({"native":"npm:@vendor/native@^1"});
    project
        .write("registry.json", registry.to_string().as_bytes())
        .unwrap();
    let output = run(
        project.path(),
        &["install", "--registry-fixture", "registry.json", "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let bytes = fs::read(project.path().join("tapid.lock")).unwrap();
    let lock: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(lock["lockfileVersion"], 11);
    assert_eq!(lock["selection"]["omissions"][0]["name"], "native");
    assert_eq!(
        lock["selection"]["omissions"][0]["reason"],
        "metadataAbsent"
    );
    let installed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        installed["selection"]["omissions"][0]["requirement"],
        "npm:@vendor/native@^1"
    );
    for args in [
        vec!["install", "--offline", "--json"],
        vec![
            "ci",
            "--offline",
            "--registry-fixture",
            "registry.json",
            "--json",
        ],
        vec!["install", "--frozen", "--json"],
    ] {
        let output = run(project.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
        assert!(
            project
                .path()
                .join("node_modules/h3/package.json")
                .is_file()
        );
        assert!(!project.path().join("node_modules/native").exists());
    }
    // Recovered metadata must not reselect an omitted branch, even with a cold store.
    let mut recovered = registry["packages"][0].clone();
    recovered["name"] = "@vendor/native".into();
    recovered["optionalDependencies"] = serde_json::json!({});
    registry["packages"].as_array_mut().unwrap().push(recovered);
    project
        .write("registry.json", registry.to_string().as_bytes())
        .unwrap();
    let cold = project.path().join("cold-store");
    let output = run(
        project.path(),
        &[
            "ci",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            cold.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
    let why = run(project.path(), &["why", "@vendor/native", "--json"]);
    assert!(
        why.status.success(),
        "{}",
        String::from_utf8_lossy(&why.stdout)
    );
}

#[test]
fn recorded_optional_omissions_replay_offline_and_remain_visible() {
    use tapid_lockfile::*;
    let project = tapid_test_support::TempProject::new("recorded-omission-replay").unwrap();
    project
        .write("registry.json", br#"{"packages":[]}"#)
        .unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","optionalDependencies":{"native":"^1"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    lock.set_selection(LockedSelection {
        target: SelectionTarget {
            os: std::env::consts::OS.into(),
            cpu: std::env::consts::ARCH.into(),
            libc: if cfg!(all(target_os = "linux", target_env = "gnu")) {
                Some("glibc".into())
            } else if cfg!(all(target_os = "linux", target_env = "musl")) {
                Some("musl".into())
            } else {
                None
            },
        },
        profile: SelectionProfile::Development,
        declarations: vec![],
        omissions: vec![AcceptedOmission {
            origin: OmissionOrigin::Root,
            name: "native".into(),
            requirement: "^1".into(),
            registry: "https://registry.npmjs.org".into(),
            target: None,
            reason: OmissionReason::MetadataAbsent,
        }],
    })
    .unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    for args in [
        vec!["install", "--offline", "--json"],
        vec!["ci", "--offline", "--json"],
        vec!["install", "--frozen", "--json"],
        vec!["install", "--registry-fixture", "registry.json", "--json"],
    ] {
        let output = run(project.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let wire: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(wire["selection"]["omissions"][0]["name"], "native");
        assert_eq!(
            wire["selection"]["omissions"][0]["reason"],
            "metadataAbsent"
        );
        assert_eq!(wire["selection"]["details_complete"], true);
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            bytes
        );
    }
    let output = run(project.path(), &["install", "--offline"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Optional dependency native@^1 from project root omitted: MetadataAbsent.")
    );

    let layout = fs::read(project.path().join(".tapid-managed")).unwrap();
    let override_attempt = run(
        project.path(),
        &[
            "install",
            "--allow-unapproved-dependency-scripts",
            "--registry-fixture",
            "registry.json",
            "--json",
        ],
    );
    assert!(
        !override_attempt.status.success(),
        "execution overrides must not silently reselect recorded omissions"
    );
    assert_eq!(
        fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
        bytes
    );
    for (field, value) in [
        ("profile", "production"),
        ("os", "different_os"),
        ("cpu", "different_cpu"),
        ("libc", "different_libc"),
    ] {
        let mut wire: serde_json::Value = serde_json::from_str(&bytes).unwrap();
        if field == "profile" {
            wire["selection"][field] = value.into();
        } else {
            wire["selection"]["target"][field] = value.into();
        }
        let changed = serde_json::to_string(&wire).unwrap();
        project.write("tapid.lock", changed.as_bytes()).unwrap();
        let output = run(project.path(), &["install", "--offline", "--json"]);
        assert!(!output.status.success(), "{field}");
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            changed
        );
        assert_eq!(
            fs::read(project.path().join(".tapid-managed")).unwrap(),
            layout
        );
    }
}

#[test]
fn why_reports_recorded_omissions_in_human_and_json_output_without_mutation() {
    use tapid_lockfile::{
        AcceptedOmission, LockedSelection, OmissionOrigin, OmissionReason, SelectionProfile,
        SelectionTarget,
    };
    let project = tapid_test_support::TempProject::new("why-omission-output").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","optionalDependencies":{"native":"^1"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    lock.set_selection(LockedSelection {
        target: SelectionTarget {
            os: "linux".into(),
            cpu: "x86_64".into(),
            libc: Some("gnu".into()),
        },
        profile: SelectionProfile::Development,
        declarations: vec![],
        omissions: vec![AcceptedOmission {
            origin: OmissionOrigin::Root,
            name: "native".into(),
            requirement: "^1".into(),
            registry: "https://registry.npmjs.org".into(),
            target: None,
            reason: OmissionReason::MetadataAbsent,
        }],
    })
    .unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    let human = run(project.path(), &["why", "native"]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("Optional dependency native@^1 from project root omitted: metadata absent"),
        "{text}"
    );
    assert!(!text.contains("Dependency paths"), "{text}");
    assert!(
        text.contains("Recorded selection: linux/x86_64, libc=gnu, profile=development."),
        "{text}"
    );
    let json = run(project.path(), &["why", "native", "--json"]);
    assert!(json.status.success());
    let wire: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(wire["data"]["not_found"], false);
    assert_eq!(wire["data"]["omissions"][0]["reason"], "metadataAbsent");
    assert_eq!(wire["data"]["selection"]["target"]["os"], "linux");
    assert_eq!(wire["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
        bytes
    );
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn why_confirmed_absent_package_still_exits_one() {
    let project = tapid_test_support::TempProject::new("why-confirmed-absent").unwrap();
    let manifest = br#"{"name":"demo","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    project
        .write(
            "tapid.lock",
            lock_for_manifest(std::str::from_utf8(manifest).unwrap())
                .to_json()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();

    let output = run(project.path(), &["why", "missing"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("No path to 'missing' was found in the resolved dependency graph.")
    );
}

#[test]
fn why_reports_a_deterministic_path_from_a_direct_root_without_mutation() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-transitive").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"app":"1.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let mut app = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "app",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let target = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "target",
        "2.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "b".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    app.add_dependency("target", &target.key()).unwrap();
    let app_key = app.key();
    lock.insert_packages([app, target]).unwrap();
    lock.set_roots([app_key.clone()]).unwrap();
    lock.set_root_bindings(std::collections::BTreeMap::from([(
        "app".to_owned(),
        app_key,
    )]))
    .unwrap();
    let lock_bytes = lock.to_json().unwrap();
    project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
    let before = fs::read(project.path().join("tapid.lock")).unwrap();
    let text = run(project.path(), &["why", "target"]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("dependencies (app)"));
    assert!(String::from_utf8_lossy(&text.stdout).contains("dependency (target)"));
    let json = run(project.path(), &["why", "target", "--json"]);
    assert!(json.status.success());
    let result: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        result["data"]["paths"][0]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(result["changes"]["state"], "unchanged");
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
}

#[test]
fn why_reconstructs_only_manifest_matching_roots_and_reports_nested_instances() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let project = tapid_test_support::TempProject::new("why-legacy-roots").unwrap();
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"foo":"^1.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let foo_one = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "foo",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "1".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let foo_two = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "foo",
        "2.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "2".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let root_key = foo_one.key();
    lock.insert_packages([foo_one, foo_two]).unwrap();
    lock.set_roots([root_key]).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("rootBindings");
    project
        .write("tapid.lock", serde_json::to_vec(&value).unwrap().as_slice())
        .unwrap();
    let output = run(project.path(), &["--json", "why", "foo"]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let paths = result["data"]["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]["steps"][0]["key"]
            .as_str()
            .unwrap()
            .contains("|foo@1.0.0|")
    );
}

#[test]
fn sandbox_help_exposes_one_time_guard_management() {
    let project = tapid_test_support::TempProject::new("sandbox-help").unwrap();
    let output = isolated_command(project.path(), &["sandbox", "--help"], true)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for command in ["install", "status", "uninstall"] {
        assert!(help.contains(command), "{help}");
    }
}

#[test]
fn sandbox_json_rejects_install_before_privileged_management() {
    let project = tapid_test_support::TempProject::new("sandbox-json").unwrap();
    let output = isolated_command(project.path(), &["--json", "sandbox", "install"], true)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("JSON_UNSUPPORTED_COMMAND"),
        "{output:?}"
    );
}
#[path = "cli_cases/preview.rs"]
mod preview_tests;

#[test]
fn workspace_optional_omissions_preserve_local_links_and_exact_origins() {
    let project = tapid_test_support::TempProject::new("workspace-optional-ledger").unwrap();
    project.write("package.json", br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"app":"workspace:*"},"optionalDependencies":{"missing":"^1"}}"#).unwrap();
    project.write("packages/app/package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"lib":"workspace:^"},"optionalDependencies":{"missing":"^2"}}"#).unwrap();
    project
        .write(
            "packages/lib/package.json",
            br#"{"name":"lib","version":"1.0.0"}"#,
        )
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let output = run(
        project.path(),
        &["install", "--registry-fixture", "registry.json", "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let bytes = fs::read(project.path().join("tapid.lock")).unwrap();
    let lock = tapid_lockfile::Lockfile::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let selection = lock.selection().expect("workspace omission ledger");
    assert_eq!(selection.omissions.len(), 2);
    assert_eq!(
        selection.omissions[0].origin,
        tapid_lockfile::OmissionOrigin::Root
    );
    assert_eq!(selection.omissions[0].requirement, "^1");
    let app = lock
        .workspace_packages()
        .values()
        .find(|p| {
            p.key()
                .parse::<tapid_lockfile::LockfilePackageKey>()
                .unwrap()
                .name
                .as_str()
                == "app"
        })
        .unwrap();
    assert_eq!(
        selection.omissions[1].origin,
        tapid_lockfile::OmissionOrigin::Workspace { key: app.key() }
    );
    assert_eq!(selection.omissions[1].requirement, "^2");
    assert!(
        selection
            .declarations
            .iter()
            .any(|d| d.requirement == "workspace:*")
    );
    assert!(
        selection
            .declarations
            .iter()
            .any(|d| d.requirement == "workspace:^")
    );
    for args in [
        vec!["ci", "--offline", "--json"],
        vec!["install", "--frozen", "--json"],
    ] {
        let output = run(project.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
    }
    let marker = fs::read(project.path().join(".tapid-managed")).unwrap();
    for changed in [
        br#"{"name":"app","version":"1.0.0","dependencies":{"lib":"workspace:^"},"optionalDependencies":{"missing":"^3"}}"#.as_slice(),
        br#"{"name":"app","version":"1.0.0","dependencies":{"lib":"workspace:^","missing":"^2"}}"#.as_slice(),
    ] {
        project.write("packages/app/package.json", changed).unwrap();
        let output = run(project.path(), &["ci", "--offline", "--json"]);
        assert!(!output.status.success());
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
        assert_eq!(fs::read(project.path().join(".tapid-managed")).unwrap(), marker);
    }
}

#[test]
fn workspace_required_alias_prevents_optional_omission_and_publication() {
    let project = tapid_test_support::TempProject::new("workspace-required-optional").unwrap();
    project.write("package.json", br#"{"name":"root","version":"1.0.0","workspaces":["member"],"optionalDependencies":{"local":"npm:missing@^1"}}"#).unwrap();
    project
        .write(
            "member/package.json",
            br#"{"name":"member","version":"1.0.0","dependencies":{"required":"npm:missing@^1"}}"#,
        )
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let output = run(
        project.path(),
        &["install", "--registry-fixture", "registry.json", "--json"],
    );
    assert!(!output.status.success());
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join(".tapid-managed").exists());
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn workspace_optional_candidate_pruning_keeps_other_declaration_origins() {
    for root_omitted in [false, true] {
        let project = tapid_test_support::TempProject::new("workspace-optional-origins").unwrap();
        let (root_range, member_range) = if root_omitted {
            ("npm:h3@^9", "npm:h3@1.0.0")
        } else {
            ("npm:h3@1.0.0", "npm:h3@^9")
        };
        let root = serde_json::json!({"name":"root", "version":"1.0.0", "workspaces":["member"], "optionalDependencies":{"local":root_range}});
        let member = serde_json::json!({"name":"member", "version":"1.0.0", "optionalDependencies":{"local":member_range}});
        project
            .write("package.json", root.to_string().as_bytes())
            .unwrap();
        project
            .write("member/package.json", member.to_string().as_bytes())
            .unwrap();
        project
            .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
            .unwrap();
        let output = run(
            project.path(),
            &["install", "--registry-fixture", "registry.json", "--json"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let bytes = fs::read(project.path().join("tapid.lock")).unwrap();
        let lock =
            tapid_lockfile::Lockfile::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let selection = lock.selection().unwrap();
        assert_eq!(selection.omissions.len(), 1);
        assert_eq!(
            matches!(
                selection.omissions[0].origin,
                tapid_lockfile::OmissionOrigin::Root
            ),
            root_omitted
        );
        assert_eq!(
            selection.omissions[0].reason,
            tapid_lockfile::OmissionReason::NoUsableCandidate
        );
        assert_eq!(lock.root_bindings().contains_key("local"), !root_omitted);
        assert_eq!(
            lock.workspace_packages()
                .values()
                .next()
                .unwrap()
                .dependencies()
                .contains_key("local"),
            root_omitted
        );
        let output = run(
            project.path(),
            &[
                "ci",
                "--offline",
                "--registry-fixture",
                "registry.json",
                "--json",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), bytes);
    }
}

#[test]
fn workspace_overridden_optional_omission_fails_before_publication() {
    let project = tapid_test_support::TempProject::new("workspace-override-omission").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"root","version":"1.0.0","workspaces":["member"],"overrides":{"h3":"^9"}}"#,
        )
        .unwrap();
    project
        .write(
            "member/package.json",
            br#"{"name":"member","version":"1.0.0","optionalDependencies":{"local":"npm:h3@^1"}}"#,
        )
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let output = run(
        project.path(),
        &["install", "--registry-fixture", "registry.json"],
    );
    assert!(
        !output.status.success(),
        "overridden provenance must be rejected until both original and effective requirements can be retained"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("overridden workspace declarations"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join(".tapid-managed").exists());
}
