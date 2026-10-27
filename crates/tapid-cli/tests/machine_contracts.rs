use serde_json::{Value, json};
use std::{fs, process::Command};
use tapid_test_support::{TempHome, TempProject};

const PREFLIGHT: &str = include_str!("../schemas/cli/run-preflight.v1.schema.json");

fn invoke(project: &TempProject, args: &[&str]) -> (std::process::Output, Value) {
    let home = TempHome::new("machine-contract").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CACHE_HOME", home.path())
        .env("LOCALAPPDATA", home.path())
        .current_dir(project.path())
        .args(args);
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command.output().unwrap();
    assert!(output.stderr.is_empty(), "{output:?}");
    let value = serde_json::from_slice(&output.stdout).expect("single JSON result");
    (output, value)
}

#[test]
fn preflight_schema_validates_success_failure_and_truncated_payloads() {
    let schema: Value = serde_json::from_str(PREFLIGHT).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let example: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/preflight-blocked.v1.json"
    ))
    .unwrap();
    assert!(validator.is_valid(&example));
    let project = TempProject::new("preflight-schema").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"schema","version":"1.0.0","scripts":{"probe":"never execute"}}"#,
        )
        .unwrap();
    let runtime = TempProject::new("preflight-schema-runtime").unwrap();
    let node = runtime
        .path()
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    // Path validation deliberately must not execute this executable. Using the
    // test binary makes an accidental runtime probe fail rather than run Node.
    fs::copy(std::env::current_exe().unwrap(), &node).unwrap();
    let (output, success) = invoke(
        &project,
        &[
            "--json",
            "run",
            "probe",
            "--preflight",
            "--allow-read",
            ".",
            "--node-runtime",
            node.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{success}");
    assert!(
        validator.is_valid(&success["data"]),
        "{}",
        validator
            .iter_errors(&success["data"])
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (output, failure) = invoke(
        &project,
        &[
            "--json",
            "run",
            "probe",
            "--preflight",
            "--allow-read",
            ".",
            "--node-runtime",
            "missing",
        ],
    );
    assert!(!output.status.success());
    assert!(validator.is_valid(&failure["data"]), "{failure}");
    let long = "s".repeat(5000);
    project.write("package.json", serde_json::to_string(&json!({"name":"schema", "version":"1.0.0", "scripts": {long.clone(): "never execute"}})).unwrap().as_bytes()).unwrap();
    let (output, truncated) = invoke(
        &project,
        &[
            "--json",
            "run",
            &long,
            "--preflight",
            "--allow-read",
            ".",
            "--node-runtime",
            node.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        validator.is_valid(&truncated["data"]),
        "{}",
        validator
            .iter_errors(&truncated["data"])
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    for (pointer, invalid) in [
        ("/schema_version", json!(2)),
        ("/enforcement_established", json!(true)),
        ("/runtime/identity", json!("enforced")),
        ("/dimensions/0/support", json!("supported")),
        ("/blockers", json!([{"code":"UNKNOWN"}])),
    ] {
        let mut changed = success["data"].clone();
        *changed.pointer_mut(pointer).unwrap() = invalid;
        assert!(!validator.is_valid(&changed), "accepted invalid {pointer}");
    }
    let mut missing = success["data"].clone();
    missing.as_object_mut().unwrap().remove("assessment");
    assert!(!validator.is_valid(&missing));
    let mut additive = success["data"].clone();
    additive["future_field"] = json!({"ignored": true});
    assert!(validator.is_valid(&additive));
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn schema_catalog_exports_match_packaged_files_and_have_bounded_complete_output() {
    let project = TempProject::new("schema-catalog").unwrap();
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    let catalog = discovery["data"]["schema_catalog"].as_array().unwrap();
    assert_eq!(catalog.len(), 7);
    let advertised = catalog
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    let referenced = discovery["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|command| command["modes"].as_array().unwrap())
        .flat_map(|mode| {
            [mode["schema"].as_str(), mode["envelope_schema"].as_str()]
                .into_iter()
                .flatten()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(advertised, referenced);
    let wrapper: Value =
        serde_json::from_str(include_str!("../schemas/cli/schema-export.v1.schema.json")).unwrap();
    let wrapper_validator = jsonschema::validator_for(&wrapper).unwrap();
    for entry in catalog {
        let id = entry["id"].as_str().unwrap();
        let (output, exported) = invoke(&project, &["--json", "capabilities", "--schema", id]);
        assert!(output.status.success());
        assert!(output.stdout.len() < 64 * 1024);
        assert!(wrapper_validator.is_valid(&exported["data"]), "{exported}");
        let source: Value = serde_json::from_str(match id {
            "tapid.envelope.v1" => include_str!("../schemas/cli/envelope.v1.schema.json"),
            "tapid.schema-export.v1" => include_str!("../schemas/cli/schema-export.v1.schema.json"),
            "tapid.run-receipt.v1" => include_str!("../schemas/cli/run-receipt.v1.schema.json"),
            "tapid.run-result.v1" => include_str!("../schemas/cli/run-result.v1.schema.json"),
            "tapid.status.v1" => include_str!("../schemas/cli/status.v1.schema.json"),
            "tapid.run-preflight.v1" => PREFLIGHT,
            "tapid.capabilities.v1" => include_str!("../schemas/cli/capabilities.v1.schema.json"),
            _ => panic!("unrecognized advertised schema {id}"),
        })
        .unwrap();
        assert_eq!(exported["data"]["schema"], source);
        assert_eq!(exported["data"]["id"], id);
        assert!(jsonschema::validator_for(&source).is_ok());
    }
    assert!(serde_json::to_vec(&discovery).unwrap().len() < 512 * 1024);
}

#[test]
fn discovery_schema_validates_all_public_modes() {
    let project = TempProject::new("discovery-schema").unwrap();
    let (output, exported) = invoke(
        &project,
        &[
            "--json",
            "capabilities",
            "--schema",
            "tapid.capabilities.v1",
        ],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    assert!(validator.is_valid(&discovery["data"]), "{discovery}");
    for (pointer, invalid) in [
        ("/schema_version", json!(2)),
        ("/assurance/process_enforcement", json!("established")),
        ("/commands/0/modes/0/json", json!("unknown")),
        (
            "/commands/0/modes/0/effects/network_possible",
            json!("false"),
        ),
    ] {
        let mut changed = discovery["data"].clone();
        *changed.pointer_mut(pointer).unwrap() = invalid;
        assert!(!validator.is_valid(&changed), "accepted {pointer}");
    }
    let mut missing = discovery["data"].clone();
    missing["commands"][0]["modes"][0]
        .as_object_mut()
        .unwrap()
        .remove("effects");
    assert!(!validator.is_valid(&missing));
    let mut missing_reference = discovery["data"].clone();
    let command = missing_reference["commands"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|command| command["path"] == "capabilities")
        .unwrap();
    command["modes"][0]
        .as_object_mut()
        .unwrap()
        .remove("schema");
    assert!(!validator.is_valid(&missing_reference));
    let mut additive = discovery["data"].clone();
    additive["future"] = json!(true);
    assert!(validator.is_valid(&additive));
}

#[test]
fn status_schema_validates_local_and_blocked_reports() {
    let project = TempProject::new("status-schema").unwrap();
    let (output, exported) = invoke(
        &project,
        &["--json", "capabilities", "--schema", "tapid.status.v1"],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    for source in [
        include_str!("../schemas/cli/examples/status-blocked.v1.json"),
        include_str!("../schemas/cli/examples/status-manifest-only.v1.json"),
    ] {
        let example: Value = serde_json::from_str(source).unwrap();
        assert!(validator.is_valid(&example));
    }
    for manifest in [
        None,
        Some(br#"{"name":"app","version":"1.0.0"}"#.as_slice()),
    ] {
        if let Some(bytes) = manifest {
            project.write("package.json", bytes).unwrap();
            let lock = tapid_lockfile::Lockfile::new(&format!(
                "sha256-{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(bytes)
            ))
            .unwrap();
            project
                .write("tapid.lock", lock.to_json().unwrap().as_bytes())
                .unwrap();
        }
        let (output, report) = invoke(&project, &["--json", "status"]);
        assert_eq!(output.status.success(), manifest.is_some(), "{report}");
        assert!(
            validator.is_valid(&report["data"]),
            "{}",
            validator
                .iter_errors(&report["data"])
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
        for (pointer, invalid) in [
            ("/assessment", json!("compatible")),
            ("/snapshot/consistency", json!("atomic")),
            ("/lifecycle/enforcement", json!("established")),
            ("/operations/native_resolution/support", json!("supported")),
        ] {
            let mut changed = report["data"].clone();
            *changed.pointer_mut(pointer).unwrap() = invalid;
            assert!(!validator.is_valid(&changed), "accepted {pointer}");
        }
        let mut changed = report["data"].clone();
        changed
            .as_object_mut()
            .unwrap()
            .remove("checks_not_performed");
        assert!(!validator.is_valid(&changed));
        let mut additive = report["data"].clone();
        additive["future"] = json!(true);
        assert!(validator.is_valid(&additive));
    }
}

#[test]
fn schema_export_contract_validates_wrappers_and_rejects_invalid_identifiers() {
    let project = TempProject::new("schema-export-contract").unwrap();
    let (output, exported) = invoke(
        &project,
        &[
            "--json",
            "capabilities",
            "--schema",
            "tapid.schema-export.v1",
        ],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    assert!(validator.is_valid(&exported["data"]));
    let example: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/schema-export.v1.json"
    ))
    .unwrap();
    assert!(validator.is_valid(&example));
    for (key, value) in [
        ("id", json!("https://example.invalid/schema")),
        ("schema", json!(null)),
    ] {
        let mut invalid = exported["data"].clone();
        invalid[key] = value;
        assert!(!validator.is_valid(&invalid), "accepted {key}");
    }
    let mut missing = exported["data"].clone();
    missing.as_object_mut().unwrap().remove("schema");
    assert!(!validator.is_valid(&missing));
    let mut additive = exported["data"].clone();
    additive["future"] = json!(true);
    assert!(validator.is_valid(&additive));
}

#[test]
fn run_result_schema_validates_trusted_prelaunch_failure() {
    let project = TempProject::new("run-result-schema").unwrap();
    let (output, exported) = invoke(
        &project,
        &["--json", "capabilities", "--schema", "tapid.run-result.v1"],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    for source in [
        include_str!("../schemas/cli/examples/run-result-prelaunch.v1.json"),
        include_str!("../schemas/cli/examples/run-result-completed.v1.json"),
    ] {
        let example: Value = serde_json::from_str(source).unwrap();
        assert!(validator.is_valid(&example), "{example}");
    }
    let mut completed: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/run-result-completed.v1.json"
    ))
    .unwrap();
    completed["receipt"]["completion"]["cleanup_confidence"] = json!("Unknown");
    assert!(!validator.is_valid(&completed));
    completed["receipt"]["completion"]["cleanup_confidence"] = json!("NotGuaranteed");
    completed["receipt"]["enforced"]["network"] = json!("true");
    assert!(!validator.is_valid(&completed));
    let owner = TempProject::new("run-result-schema-owner").unwrap();
    let home = TempHome::new("run-result-schema").unwrap();
    let path = owner.path().join("result.json");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(project.path())
        .args(["run", "missing", "--result-file"])
        .arg(&path);
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command.output().unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(result["outcome"], "prelaunch_failure");
    assert!(validator.is_valid(&result), "{result}");
    for (key, value) in [
        ("schema_version", json!(2)),
        ("receipt", json!({})),
        ("error", Value::Null),
        ("outcome", json!("completed")),
    ] {
        let mut invalid = result.clone();
        invalid[key] = value;
        assert!(!validator.is_valid(&invalid), "accepted {key}");
    }
    let mut missing = result.clone();
    missing.as_object_mut().unwrap().remove("policy_inputs");
    assert!(!validator.is_valid(&missing));
    let mut additive = result;
    additive["future"] = json!(true);
    assert!(validator.is_valid(&additive));
}

#[test]
fn legacy_run_receipt_schema_preserves_channel_and_version_limits() {
    let project = TempProject::new("run-receipt-schema").unwrap();
    let (output, exported) = invoke(
        &project,
        &["--json", "capabilities", "--schema", "tapid.run-receipt.v1"],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    let completed: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/run-result-completed.v1.json"
    ))
    .unwrap();
    let example: Value =
        serde_json::from_str(include_str!("../schemas/cli/examples/run-receipt.v1.json")).unwrap();
    assert!(validator.is_valid(&example));
    let mut receipt = completed["receipt"].clone();
    assert!(validator.is_valid(&receipt));
    receipt["schema_version"] = json!(2);
    assert!(!validator.is_valid(&receipt));
    receipt["schema_version"] = json!(1);
    receipt.as_object_mut().unwrap().remove("completion");
    assert!(!validator.is_valid(&receipt));
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    let run = discovery["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|command| command["path"] == "run")
        .unwrap();
    let mode = run["modes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mode| mode["id"] == "receipt_json")
        .unwrap();
    assert_eq!(mode["json"], "unsupported");
    assert_eq!(mode["separate_channels"], json!(["receipt-json"]));
    assert_eq!(mode["trusted_channel"], false);
    let discovery_schema: Value =
        serde_json::from_str(include_str!("../schemas/cli/capabilities.v1.schema.json")).unwrap();
    let mode_schema =
        &discovery_schema["properties"]["commands"]["items"]["properties"]["modes"]["items"];
    let mode_validator = jsonschema::validator_for(mode_schema).unwrap();
    let mut invalid_mode = mode.clone();
    invalid_mode["trusted_channel"] = json!("false");
    assert!(!mode_validator.is_valid(&invalid_mode));

    assert_eq!(
        run["modes"][0]["when"],
        json!({"preflight": false, "result_file": "absent", "receipt_json": false})
    );
}

#[test]
fn envelope_schema_covers_protocol_results_without_claiming_payload_coverage() {
    let project = TempProject::new("envelope-schema").unwrap();
    let (output, exported) = invoke(
        &project,
        &["--json", "capabilities", "--schema", "tapid.envelope.v1"],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    let example: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/envelope-failure.v1.json"
    ))
    .unwrap();
    assert!(validator.is_valid(&example));
    for field in example.as_object().unwrap().keys() {
        let mut missing = example.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!validator.is_valid(&missing), "accepted missing {field}");
    }
    for args in [
        vec!["--json", "capabilities"],
        vec!["--json", "status"],
        vec!["--json", "--help"],
        vec!["--json", "capabilities", "--schema", "unknown"],
    ] {
        let (_, value) = invoke(&project, &args);
        assert!(validator.is_valid(&value), "{value}");
        for (pointer, invalid) in [
            ("/schema_version", json!(2)),
            ("/outcome", json!("compatible")),
            ("/changes/state", json!("maybe")),
            ("/errors", json!([{}])),
        ] {
            let mut changed = value.clone();
            *changed.pointer_mut(pointer).unwrap() = invalid;
            assert!(!validator.is_valid(&changed), "accepted {pointer}");
        }
        let mut unavailable_path = value.clone();
        unavailable_path["project_path"] = json!({"encoding": "utf8", "value": null});
        assert!(!validator.is_valid(&unavailable_path));
        unavailable_path["project_path"]["unavailable"] = json!("capacity_exceeded");
        assert!(validator.is_valid(&unavailable_path));
        let mut additive = value.clone();
        additive["future_field"] = json!(true);
        additive["data"] = json!({"untyped_payload": true});
        assert!(validator.is_valid(&additive));
    }
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    let install = discovery["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "install")
        .unwrap();
    assert_eq!(install["modes"][0]["schema_coverage"], "envelope_only");
    assert_eq!(install["modes"][0]["envelope_schema"], "tapid.envelope.v1");
    assert!(install["modes"][0].get("schema").is_none());
}
