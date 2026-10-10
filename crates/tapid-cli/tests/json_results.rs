use std::{fs, process::Command};
use tapid_test_support::TempProject;

fn invoke(project: &TempProject, args: &[&str]) -> (std::process::Output, serde_json::Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(args)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).expect("one JSON result on stdout");
    assert!(output.stderr.is_empty(), "{:?}", output);
    (output, value)
}

#[test]
fn json_install_success_and_handled_failure() {
    let project = TempProject::new("json-install").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project.write("registry.json", br#"{"packages":[{"registry":"https://jsr.io","name":"@scope/unused","version":"1.0.0","artifact":"https://jsr.io/@scope/unused/1.0.0.tgz"}]}"#).unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "install");
    assert_eq!(result["outcome"], "success");
    assert_eq!(result["data"]["package_count"], 0);
    assert_eq!(result["changes"]["state"], "committed");
    assert_eq!(result["project_path"]["encoding"], "utf8");
    assert_eq!(
        result["project_path"]["value"],
        std::fs::canonicalize(project.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(result["truncated_fields"], serde_json::json!([]));
    let native_files = result["changes"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| {
            assert_eq!(path["encoding"], "utf8");
            path["value"].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        native_files,
        *result["changes"]["files"].as_array().unwrap()
    );
    std::fs::remove_file(project.path().join("tapid.lock")).unwrap();
    let (output, result) = invoke(
        &project,
        &["--json", "install", "--offline", "--store-dir", "store"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["outcome"], "failure");
    assert_eq!(result["errors"][0]["code"], "LOCKFILE_MISSING");
    assert_eq!(result["retry"], "after_correction");
}

#[test]
fn json_parse_errors_and_unsupported_commands_do_not_echo_input() {
    let project = TempProject::new("json-parse").unwrap();
    let (output, result) = invoke(&project, &["--json", "--fixture-secret"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
    let (output, result) = invoke(&project, &["--json", "init"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
    assert!(!project.path().join("package.json").exists());
    let (output, result) = invoke(
        &project,
        &["--json", "import-package-lock", "package-lock.json"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["operation"], "import-package-lock");
    assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
    assert!(!project.path().join("tapid.lock").exists());
}

#[test]
fn json_install_replays_imported_npm_selections() {
    let project = TempProject::new("json-npm-import").unwrap();
    for (path, bytes) in [
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
        project.write(path, bytes).unwrap();
    }
    let import = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["import-package-lock", "package-lock.json"])
        .output()
        .unwrap();
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let store = project.path().join("store");
    for offline in [false, true] {
        let mut args = vec![
            "install",
            "--json",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ];
        if offline {
            args.push("--offline");
        } else {
            args.extend(["--registry-fixture", "registry.json"]);
        }
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], "install");
        assert_eq!(result["data"]["package_count"], 4);
        assert_eq!(result["outcome"], "success");
    }
}

#[test]
fn json_lifecycle_results_and_partial_outdated_metadata() {
    let project = TempProject::new("json-lifecycle").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let store = project.path().join("store");
    let store = store.to_str().unwrap();
    for (operation, packages) in [("add", vec!["foo@npm:h3@^1.0.0"]), ("update", vec![])] {
        let mut args = vec![
            "--json",
            operation,
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store,
        ];
        args.extend(packages);
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], operation);
        assert_eq!(result["outcome"], "success");
    }
    let (output, result) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["outcome"], "success");
    assert_eq!(result["data"]["truncated"], false);
    assert_eq!(
        result["data"]["entries"][0]["compatible_impact"],
        serde_json::json!({
            "version_change": "unchanged", "lockfile": "unchanged", "manifest": "unchanged"
        })
    );
    assert_eq!(
        result["data"]["entries"][0]["available_impact"],
        serde_json::json!({
            "version_change": "major", "lockfile": "changed", "manifest": "changed"
        })
    );
    assert_eq!(
        result["data"]["entries"][0]["newest_available"],
        "2.0.1-rc.20"
    );
    project
        .write("missing.json", br#"{"packages":[]}"#)
        .unwrap();
    let manifest_before = fs::read(project.path().join("package.json")).unwrap();
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let (output, offline) = invoke(&project, &["--json", "outdated", "--offline"]);
    assert!(output.status.success(), "{offline}");
    assert_eq!(offline["outcome"], "partial");
    assert_eq!(
        offline["data"]["entries"][0]["error"]["code"],
        "REGISTRY_METADATA_UNAVAILABLE"
    );
    assert!(offline["data"]["entries"][0]["newest_available"].is_null());
    assert_eq!(
        offline["data"]["entries"][0]["available_impact"]["lockfile"],
        "unknown"
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
    let (output, fixture_offline) = invoke(
        &project,
        &[
            "--json",
            "outdated",
            "--offline",
            "--registry-fixture",
            "registry.json",
        ],
    );
    assert!(output.status.success());
    assert_eq!(fixture_offline, result);
    let (output, result) = invoke(
        &project,
        &["outdated", "--json", "--registry-fixture", "missing.json"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["outcome"], "partial");
    assert_eq!(result["changes"]["state"], "unchanged");
    assert!(result["data"]["entries"][0]["error"]["code"].is_string());
    assert!(result["data"]["entries"][0]["newest_available"].is_null());
    assert_eq!(
        result["data"]["entries"][0]["available_impact"],
        serde_json::json!({
            "version_change": "unknown", "lockfile": "unknown", "manifest": "unknown"
        })
    );
    let (output, result) = invoke(&project, &["prune", "--json", "--store-dir", store]);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["operation"], "prune");
    let (output, result) = invoke(
        &project,
        &[
            "remove",
            "foo",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store,
        ],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["operation"], "remove");
}

#[test]
fn json_early_failures_are_single_objects() {
    let project = TempProject::new("json-early").unwrap();
    for args in [
        vec!["add"],
        vec!["remove"],
        vec!["install", "--workspace", "missing"],
        vec!["update"],
        vec!["outdated"],
        vec!["prune"],
    ] {
        let mut args = args;
        args.push("--json");
        let (output, result) = invoke(&project, &args);
        assert_eq!(output.status.code(), Some(1), "{result}");
        assert_eq!(result["outcome"], "failure");
        assert!(result["errors"][0]["code"].is_string());
    }
    // A forwarded token belongs to the child and does not select JSON parsing.
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["run", "--", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn json_help_and_version_are_successful_without_project_access() {
    let project = TempProject::new("json-information").unwrap();
    project.write("package.json", b"invalid manifest").unwrap();
    project
        .write("tapid.toml", b"invalid configuration")
        .unwrap();
    for (args, expected) in [
        (vec!["--json", "--help"], "Install dependencies"),
        (vec!["--help", "--json"], "Install dependencies"),
        (vec!["--json", "install", "--help"], "--registry-fixture"),
        (vec!["i", "--json", "-h"], "--registry-fixture"),
        (vec!["--json", "help", "run"], "--node-runtime"),
        (vec!["manifest", "validate", "--json", "--help"], "Usage:"),
    ] {
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["operation"], "help");
        assert_eq!(result["outcome"], "success");
        assert_eq!(result["errors"], serde_json::json!([]));
        assert!(result["project"].is_null());
        let help = result["data"]["text"].as_str().unwrap();
        assert!(help.contains(expected), "{help}");
        assert!(!help.contains('\u{1b}'));
    }
    for args in [["--json", "--version"], ["-V", "--json"]] {
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], "version");
        assert_eq!(result["outcome"], "success");
        assert_eq!(result["errors"], serde_json::json!([]));
        assert_eq!(result["data"]["name"], "tapid");
        assert_eq!(result["data"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(result["project"].is_null());
    }
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 2);
}

#[test]
fn json_workspace_selection_errors_have_consistent_codes() {
    let project = TempProject::new("json-workspace-codes").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    for operation in ["install", "update", "outdated", "prune"] {
        let (output, result) = invoke(&project, &["--json", operation, "--workspace", "missing"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], "INVALID_REQUEST", "{result}");
    }
}

#[test]
fn json_outdated_limit_can_retrieve_entries_beyond_the_default() {
    use sha2::{Digest, Sha256};
    let project = TempProject::new("json-outdated-limit").unwrap();
    let dependencies = (0..105)
        .map(|index| (format!("package-{index:03}"), serde_json::json!("^1.0.0")))
        .collect::<serde_json::Map<_, _>>();
    let manifest =
        serde_json::json!({"name":"app", "version":"1.0.0", "dependencies": dependencies})
            .to_string();
    project.write("package.json", manifest.as_bytes()).unwrap();
    let digest = format!(
        "sha256-{}",
        hex::encode(Sha256::digest(manifest.as_bytes()))
    );
    let lock = tapid_lockfile::Lockfile::new(&digest).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    project
        .write("registry.json", br#"{"packages":[]}"#)
        .unwrap();
    for (limit, expected, truncated) in [
        (None, 100, true),
        (Some("103"), 103, true),
        (Some("0"), 105, false),
    ] {
        let mut args = vec!["--json", "outdated", "--registry-fixture", "registry.json"];
        if let Some(limit) = limit {
            args.extend(["--json-limit", limit]);
        }
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(
            result["data"]["entries"].as_array().unwrap().len(),
            expected
        );
        assert_eq!(result["data"]["truncated"], truncated);
        assert_eq!(result["data"]["total_entries"], 105);
        assert_eq!(result["outcome"], "partial");
    }
    let (output, result) = invoke(
        &project,
        &[
            "outdated",
            "--json-limit",
            "0",
            "--json",
            "--registry-fixture",
            "registry.json",
        ],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["entries"].as_array().unwrap().len(), 105);
    let (output, result) = invoke(&project, &["--json", "outdated", "--json-limit", "-1"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["outdated", "--json-limit", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn json_unsupported_results_preserve_canonical_command_names() {
    let project = TempProject::new("json-unsupported-operation").unwrap();
    for (args, operation) in [
        (vec!["--json", "init"], "init"),
        (vec!["--json", "ci"], "ci"),
        (vec!["--json", "run", "test"], "run"),
        (vec!["--json", "manifest", "validate"], "manifest"),
        (vec!["--json", "lock", "verify"], "lock"),
        (vec!["--json", "license"], "license"),
        (vec!["--json", "upgrade"], "upgrade"),
        (vec!["--json"], "none"),
    ] {
        let (output, result) = invoke(&project, &args);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["operation"], operation);
        assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
    }
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 0);
}
