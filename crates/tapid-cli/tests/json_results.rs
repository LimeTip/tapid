use std::process::Command;
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
        result["data"]["entries"][0]["newest_available"],
        "2.0.1-rc.20"
    );
    project
        .write("missing.json", br#"{"packages":[]}"#)
        .unwrap();
    let (output, result) = invoke(
        &project,
        &["outdated", "--json", "--registry-fixture", "missing.json"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["outcome"], "partial");
    assert_eq!(result["changes"]["state"], "unchanged");
    assert!(result["data"]["entries"][0]["error"]["code"].is_string());
    assert!(result["data"]["entries"][0]["newest_available"].is_null());
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
fn json_early_failures_and_help_are_single_objects() {
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
    let (output, result) = invoke(&project, &["--json", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(result["errors"][0]["code"], "JSON_HELP_UNSUPPORTED");
    // A forwarded token belongs to the child and does not select JSON parsing.
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["run", "--", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
