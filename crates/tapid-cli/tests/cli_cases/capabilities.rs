use super::*;

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
