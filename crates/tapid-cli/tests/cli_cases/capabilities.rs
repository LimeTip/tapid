use super::*;

#[test]
fn capabilities_export_only_advertised_embedded_schemas() {
    let project = tapid_test_support::TempProject::new("schema-export").unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "capabilities",
            "--schema",
            "tapid.run-preflight.v1",
        ],
        true,
    )
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["id"], "tapid.run-preflight.v1");
    assert_eq!(
        value["data"]["schema"]["properties"]["contract"]["const"],
        "tapid.run-preflight"
    );
    for id in [
        "../../package.json",
        "https://example.invalid/schema",
        "tapid.run-preflight.v2",
    ] {
        let output = isolated_command(
            project.path(),
            &["--json", "capabilities", "--schema", id],
            true,
        )
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["errors"][0]["code"], "SCHEMA_UNKNOWN");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(id));
    }
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 0);
}

#[test]
fn capabilities_are_offline_and_classify_public_commands_and_run_modes() {
    let project = tapid_test_support::TempProject::new("capabilities-offline").unwrap();
    project
        .write("package.json", b"invalid secret-project-body")
        .unwrap();
    project
        .write("tapid.toml", b"invalid secret-config-body")
        .unwrap();
    let output = isolated_command(project.path(), &["--json", "capabilities"], true)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &value["data"];
    assert_eq!(data["contract"], "tapid.capabilities");
    assert_eq!(data["schema_version"], 1);
    assert_eq!(data["tapid_version"], env!("CARGO_PKG_VERSION"));
    let commands = data["commands"].as_array().unwrap();
    for path in [
        "status",
        "run",
        "install",
        "tool install",
        "tool list",
        "sandbox status",
        "cache clean",
        "capabilities",
    ] {
        assert!(
            commands.iter().any(|command| command["path"] == path),
            "missing {path}"
        );
    }
    assert!(
        !commands
            .iter()
            .any(|command| command["path"].as_str().unwrap().contains("__"))
    );
    let run = commands
        .iter()
        .find(|command| command["path"] == "run")
        .unwrap();
    assert_eq!(run["modes"][0]["json"], "unsupported");
    assert_eq!(run["modes"][1]["id"], "preflight");
    assert_eq!(run["modes"][1]["json"], "stdout_envelope");
    let project_arg = run["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arg| arg["id"] == "project_dir")
        .unwrap();
    assert_eq!(project_arg["defaults"], serde_json::json!(["."]));
    let preflight = run["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arg| arg["id"] == "preflight")
        .unwrap();
    assert!(
        preflight["conflicts"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("result_file"))
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-"));
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 2);
    assert!(output.stdout.len() < 512 * 1024);
}

#[test]
fn capabilities_disclose_git_hydration_helpers_and_tool_list_recovery() {
    let project = tapid_test_support::TempProject::new("capabilities-effects").unwrap();
    let output = isolated_command(project.path(), &["--json", "capabilities"], true)
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let commands = value["data"]["commands"].as_array().unwrap();
    let ci = commands
        .iter()
        .find(|command| command["path"] == "ci")
        .unwrap();
    assert_eq!(ci["modes"][0]["effects"]["child_execution_possible"], true);
    let prune = commands
        .iter()
        .find(|command| command["path"] == "prune")
        .unwrap();
    assert_eq!(prune["modes"][0]["effects"]["network_possible"], false);
    assert_eq!(
        prune["modes"][0]["effects"]["child_execution_possible"],
        false
    );
    let list = commands
        .iter()
        .find(|command| command["path"] == "tool list")
        .unwrap();
    assert_eq!(list["modes"][0]["effects"]["other_writes_possible"], true);
}

#[test]
fn capabilities_distinguishes_schema_export_from_discovery_payload() {
    let project = tapid_test_support::TempProject::new("capabilities-export-mode").unwrap();
    let output = isolated_command(project.path(), &["--json", "capabilities"], true)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let command = value["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|command| command["path"] == "capabilities")
        .unwrap();
    let export = command["modes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mode| mode["id"] == "schema_export")
        .expect("separate schema-export payload mode");
    assert_eq!(export["when"]["schema"], "present");
    assert_eq!(export["json"], "stdout_envelope");
    assert_eq!(export["schema_coverage"], "payload");
    assert_eq!(export["schema"], "tapid.schema-export.v1");
    assert_eq!(export["effects"]["network_possible"], false);
    assert_eq!(export["effects"]["project_writes_possible"], false);
}
