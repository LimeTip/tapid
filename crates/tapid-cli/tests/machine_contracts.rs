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
        .env("TAPID_DISCOVERY_TOKEN", "credential-value-canary")
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
    let mut declaration = success["data"].clone();
    declaration["dimensions"][0]["support"] = json!("unsupported");
    assert!(!validator.is_valid(&declaration));
    declaration["dimensions"][0]["evidence"] = json!("static_declaration");
    assert!(!validator.is_valid(&declaration));
    declaration["dimensions"][0]["reason"] = json!("PLATFORM_UNSUPPORTED");
    assert!(!validator.is_valid(&declaration));
    declaration["assessment"] = json!("blocked");
    declaration["blockers"] = failure["data"]["blockers"].clone();
    assert!(validator.is_valid(&declaration));
    declaration["dimensions"][0]["support"] = json!("unchecked");
    assert!(!validator.is_valid(&declaration));
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
    assert_eq!(catalog.len(), 10);
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
                .chain(
                    mode["input_schemas"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
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
            "tapid.dependency-preview.v1" => {
                include_str!("../schemas/cli/dependency-preview.v1.schema.json")
            }
            "tapid.manifest-validation.v1" => {
                include_str!("../schemas/cli/manifest-validation.v1.schema.json")
            }
            "tapid.package-manifest.v1" => {
                include_str!("../schemas/cli/package-manifest.v1.schema.json")
            }
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
    for command in discovery["data"]["commands"].as_array().unwrap() {
        let arguments = command["arguments"].as_array().unwrap();
        for mode in command["modes"].as_array().unwrap() {
            for predicate in mode.get("when").into_iter().chain(
                mode["conditional_effects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|rule| &rule["when"]),
            ) {
                for id in predicate.as_object().unwrap().keys() {
                    assert!(
                        arguments
                            .iter()
                            .any(|argument| argument["id"] == id.as_str()),
                        "{}: {id}",
                        command["path"]
                    );
                }
            }
        }
    }
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

#[test]
fn manifest_validation_schema_covers_success_without_claiming_failure_payloads() {
    let project = TempProject::new("manifest-contract").unwrap();
    let (output, exported) = invoke(
        &project,
        &[
            "--json",
            "capabilities",
            "--schema",
            "tapid.manifest-validation.v1",
        ],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let (output, success) = invoke(&project, &["--json", "manifest", "validate"]);
    assert!(output.status.success());
    assert!(validator.is_valid(&success["data"]));
    for invalid in [
        json!({"action":"validate","valid":false}),
        json!({"valid":true}),
        json!({"action":"install","valid":true}),
    ] {
        assert!(!validator.is_valid(&invalid));
    }
    let mut additive = success["data"].clone();
    additive["future"] = json!(true);
    assert!(validator.is_valid(&additive));
    project.write("package.json", br#"{"name":false}"#).unwrap();
    let (output, failure) = invoke(&project, &["--json", "manifest", "validate"]);
    assert!(!output.status.success());
    assert!(failure["data"].is_null());
    let envelope: Value =
        serde_json::from_str(include_str!("../schemas/cli/envelope.v1.schema.json")).unwrap();
    assert!(
        jsonschema::validator_for(&envelope)
            .unwrap()
            .is_valid(&failure)
    );
    assert!(!validator.is_valid(&failure["data"]));
}

#[test]
fn package_manifest_input_schema_checks_structure_without_claiming_semantics() {
    let project = TempProject::new("manifest-input-contract").unwrap();
    let (output, exported) = invoke(
        &project,
        &[
            "--json",
            "capabilities",
            "--schema",
            "tapid.package-manifest.v1",
        ],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    let input = discovery["data"]["schema_catalog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "tapid.package-manifest.v1")
        .unwrap();
    assert_eq!(input["target"], "input");
    assert_eq!(input["contract"], "tapid.package-manifest");
    for valid in [
        json!({"name":"app","version":"1.0.0"}),
        json!({"name":"@scope/app","version":"1.0.0","private":true,"dependencies":{"npm:kleur":"^4"},"scripts":{"test":"node test.js"},"bin":{"app":"./cli.js"},"workspaces":["packages/*"],"future":{"any":true}}),
    ] {
        assert!(tapid_manifest::PackageManifest::parse(&valid.to_string()).is_ok());
        assert!(validator.is_valid(&valid), "{valid}");
    }
    for invalid in [
        json!([]),
        json!({"name":"app"}),
        json!({"name":"app","version":1}),
        json!({"name":"app","version":"1.0.0","private":null}),
        json!({"name":"app","version":"1.0.0","scripts":{"test":false}}),
        json!({"name":"app","version":"1.0.0","bin":{}}),
    ] {
        assert!(tapid_manifest::PackageManifest::parse(&invalid.to_string()).is_err());
        assert!(!validator.is_valid(&invalid), "{invalid}");
    }
    let example: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/package-manifest.v1.json"
    ))
    .unwrap();
    assert!(validator.is_valid(&example));
    assert!(tapid_manifest::PackageManifest::parse(&example.to_string()).is_ok());
    let semantic_failure = json!({"name":"app","version":"not-semver","bin":"../escape"});
    assert!(validator.is_valid(&semantic_failure));
    assert!(tapid_manifest::PackageManifest::parse(&semantic_failure.to_string()).is_err());
}

#[test]
fn discovery_distinguishes_dependency_preview_effects_from_mutation() {
    let project = TempProject::new("preview-discovery").unwrap();
    let (_, report) = invoke(&project, &["--json", "capabilities"]);
    for path in ["install", "add", "remove", "update"] {
        let command = report["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == path)
            .unwrap();
        let modes = command["modes"].as_array().unwrap();
        let preview = modes
            .iter()
            .find(|mode| mode["id"] == "dry_run")
            .expect("preview mode");
        assert_eq!(preview["when"]["dry_run"], true);
        assert_eq!(modes[0]["when"]["dry_run"], false);
        for effect in [
            "project_writes_possible",
            "store_writes_possible",
            "other_writes_possible",
            "child_execution_possible",
            "hook_execution_possible",
        ] {
            assert_eq!(preview["effects"][effect], false, "{path}: {effect}");
        }
        assert_eq!(preview["effects"]["network_possible"], true);
        assert_eq!(preview["schema_coverage"], "payload");
        assert_eq!(preview["schema"], "tapid.dependency-preview.v1");
        // The real command rejects missing packages without project writes;
        // discovery describes a possible effect, not a claim it occurred.
        if matches!(path, "add" | "remove") {
            let (output, failure) = invoke(&project, &["--json", path, "--dry-run"]);
            assert!(!output.status.success());
            assert_eq!(failure["changes"]["state"], "unchanged");
        }
    }
}

#[test]
fn dependency_preview_schema_covers_read_only_resolution() {
    let project = TempProject::new("preview-contract").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let fixture = json!({"packages": [{
        "registry": "https://registry.npmjs.org", "name": "parent",
        "version": "1.0.0", "artifact": "never-download",
        "integrity": "sha512-AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ=="
    }]});
    project
        .write("registry.json", &serde_json::to_vec(&fixture).unwrap())
        .unwrap();
    let (output, exported) = invoke(
        &project,
        &[
            "--json",
            "capabilities",
            "--schema",
            "tapid.dependency-preview.v1",
        ],
    );
    assert!(output.status.success(), "{exported}");
    let validator = jsonschema::validator_for(&exported["data"]["schema"]).unwrap();
    let example: Value = serde_json::from_str(include_str!(
        "../schemas/cli/examples/dependency-preview.v1.json"
    ))
    .unwrap();
    assert!(validator.is_valid(&example));
    let (output, preview) = invoke(
        &project,
        &[
            "--json",
            "add",
            "parent@1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
    );
    assert!(output.status.success(), "{preview}");
    assert!(validator.is_valid(&preview["data"]), "{preview}");
    for (pointer, invalid) in [
        ("/effects/project_writes", json!(true)),
        ("/effects/artifact_requests", json!(1)),
        ("/artifacts_prepared", json!(true)),
        ("/packages", json!([{}])),
        ("/packages/0/after/dependencies", json!([])),
        ("/packages/0/reasons/0/graph", json!("launch")),
    ] {
        let mut broken = preview["data"].clone();
        *broken.pointer_mut(pointer).unwrap() = invalid;
        assert!(!validator.is_valid(&broken), "{pointer}");
    }
    let mut additive = preview["data"].clone();
    additive["future"] = json!(true);
    assert!(validator.is_valid(&additive));
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#,
        )
        .unwrap();
    for args in [vec!["install"], vec!["remove", "parent"], vec!["update"]] {
        let mut arguments = vec!["--json"];
        arguments.extend(args);
        arguments.extend(["--dry-run", "--registry-fixture", "registry.json"]);
        let (output, result) = invoke(&project, &arguments);
        assert!(output.status.success(), "{result}");
        assert!(validator.is_valid(&result["data"]), "{result}");
    }
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn discovery_describes_replay_effects_and_application_constraints() {
    let project = TempProject::new("discovery-replay-effects").unwrap();
    let (_, report) = invoke(&project, &["--json", "capabilities"]);
    for path in ["install", "ci"] {
        let command = report["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == path)
            .unwrap();
        for mode in command["modes"].as_array().unwrap() {
            let rules = mode["conditional_effects"]
                .as_array()
                .expect("conditional effects");
            for rule in rules {
                for argument in rule["when"].as_object().unwrap().keys() {
                    assert!(
                        command["arguments"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|arg| arg["id"] == argument.as_str())
                    );
                }
            }
            let offline = rules
                .iter()
                .find(|rule| rule["when"] == json!({"offline": true}))
                .unwrap();
            assert_eq!(offline["effects"]["network_possible"], false);
            assert_eq!(offline["effects"]["hook_execution_possible"], false);
            // Offline replay still writes the installed layout; previews do not.
            assert_eq!(
                mode["effects"]["project_writes_possible"],
                mode["id"] != "dry_run"
            );
            if path == "install" {
                let frozen = rules
                    .iter()
                    .find(|rule| rule["when"] == json!({"frozen": true}))
                    .unwrap();
                assert_eq!(frozen["effects"]["hook_execution_possible"], false);
                assert!(frozen["effects"].get("network_possible").is_none());
            }
        }
    }
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/cli/capabilities.v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&report["data"]));
    let index = report["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .position(|command| command["path"] == "install")
        .unwrap();
    for invalid in [
        json!({"when": {"offline": "yes"}, "effects": {"network_possible": false}}),
        json!({"when": {"offline": true}, "effects": {"network_possible": true}}),
    ] {
        let mut broken = report["data"].clone();
        broken["commands"][index]["modes"][0]["conditional_effects"] = json!([invalid]);
        assert!(!validator.is_valid(&broken));
    }
    let constraints = report["data"]["constraints"].as_array().unwrap();
    for argument in ["package", "allow_unverified_registry_artifacts"] {
        assert!(constraints.iter().any(|rule| rule["command"] == "install"
            && rule["argument"] == argument
            && rule["conflicts_with_any"] == json!(["offline", "frozen"])));
        let args = if argument == "package" {
            vec!["--json", "install", "dep@1", "--offline"]
        } else {
            vec![
                "--json",
                "install",
                "--allow-unverified-registry-artifacts",
                "--offline",
            ]
        };
        project
            .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
            .unwrap();
        let (output, failure) = invoke(&project, &args);
        assert!(!output.status.success());
        assert_eq!(failure["errors"][0]["code"], "INVALID_REQUEST");
        assert!(!project.path().join("node_modules").exists());
    }
}

#[test]
fn discovery_guides_read_only_project_and_script_inspection() {
    let project = TempProject::new("discovery-inspection-flow").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0","scripts":{"postinstall":"never execute"}}"#;
    project.write("package.json", manifest).unwrap();
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    for (path, mode_id, arguments) in [
        ("status", "default", vec!["--json", "status"]),
        (
            "run",
            "preflight",
            vec!["--json", "run", "missing", "--preflight"],
        ),
    ] {
        let command = discovery["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == path)
            .unwrap();
        let mode = command["modes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mode| mode["id"] == mode_id)
            .unwrap();
        for effect in [
            "project_writes_possible",
            "store_writes_possible",
            "network_possible",
            "child_execution_possible",
        ] {
            assert_eq!(mode["effects"][effect], false);
        }
        let id = mode["schema"].as_str().unwrap();
        let (_, schema) = invoke(&project, &["--json", "capabilities", "--schema", id]);
        let (_, report) = invoke(&project, &arguments);
        assert!(
            jsonschema::validator_for(&schema["data"]["schema"])
                .unwrap()
                .is_valid(&report["data"]),
            "{report}"
        );
        if path == "status" {
            assert_eq!(
                report["data"]["lifecycle"]["enforcement"],
                "not_established"
            );
            assert_eq!(
                report["data"]["lifecycle"]["dependency_source_discovery"],
                "unchecked"
            );
        } else {
            assert_eq!(report["data"]["assessment"], "blocked");
        }
    }
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 1);

    // Use the same member/peer declarations as the focused status fixtures,
    // but select contracts through discovery as an automation client would.
    let root = br#"{"name":"root","version":"1.0.0","workspaces":["member","provider"]}"#;
    let member = br#"{"name":"member","version":"1.0.0","dependencies":{"provider":"workspace:^2"},"peerDependencies":{"react":"workspace:*"},"scripts":{"postinstall":"HOOK_BODY_CANARY"}}"#;
    let config = b"[registries.default]\nurl = \"https://registry.example\"\ntoken-env = \"TAPID_DISCOVERY_TOKEN\"\n";
    project.write("package.json", root).unwrap();
    project.write("member/package.json", member).unwrap();
    project
        .write(
            "provider/package.json",
            br#"{"name":"provider","version":"1.0.0"}"#,
        )
        .unwrap();
    project.write("tapid.toml", config).unwrap();
    project
        .write(".tapid-lifecycle-journal.json", b"interrupted")
        .unwrap();
    for (path, mode_id, args) in [
        (
            "status",
            "default",
            vec!["--json", "status", "--workspace", "member"],
        ),
        (
            "run",
            "preflight",
            vec![
                "--json",
                "run",
                "missing",
                "--preflight",
                "--workspace",
                "member",
            ],
        ),
    ] {
        let command = discovery["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == path)
            .unwrap();
        let mode = command["modes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mode| mode["id"] == mode_id)
            .unwrap();
        let (_, schema) = invoke(
            &project,
            &[
                "--json",
                "capabilities",
                "--schema",
                mode["schema"].as_str().unwrap(),
            ],
        );
        let (output, report) = invoke(&project, &args);
        assert!(!output.status.success(), "{report}");
        assert!(
            jsonschema::validator_for(&schema["data"]["schema"])
                .unwrap()
                .is_valid(&report["data"]),
            "{report}"
        );
        assert!(!report.to_string().contains("credential-value-canary"));
        assert!(!report.to_string().contains("HOOK_BODY_CANARY"));
        let data = &report["data"];
        if path == "status" {
            assert_eq!(data["workspace"]["selected"], "member");
            assert_eq!(data["workspace"]["linking"], "not_verified");
            assert_eq!(data["capabilities"]["runner"], "not_assessed");
            assert_eq!(
                data["registries"]["default_origin"],
                "https://registry.example"
            );
            assert_eq!(
                data["registries"]["credential_environment_names"],
                json!(["TAPID_DISCOVERY_TOKEN"])
            );
            assert_eq!(
                data["lifecycle"]["backend_and_toolchain_prerequisites"],
                "unchecked"
            );
            assert_eq!(
                data["lifecycle"]["manifest_declarations"][1]["hooks"],
                json!(["postinstall"])
            );
            for field in ["/dependencies/provider", "/peerDependencies/react"] {
                let blocker = data["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|b| {
                        b["code"] == "UNSUPPORTED_SPEC"
                            && b["action"]["parameters"]["field"] == field
                    })
                    .unwrap();
                assert_eq!(
                    blocker["operations"],
                    json!(["native_resolution", "dependency_mutation"])
                );
                assert_eq!(blocker["action"]["id"], "review_manifest_requirement");
                assert!(blocker["location"].as_str().unwrap().contains("member"));
            }
        } else {
            assert_eq!(data["enforcement_established"], false);
            assert_eq!(data["blockers"][0]["code"], "SCRIPT_MISSING");
            assert!(
                data["blockers"][0]["location"]["selected_manifest"]
                    .as_str()
                    .unwrap()
                    .ends_with(
                        &std::path::Path::new("member")
                            .join("package.json")
                            .display()
                            .to_string()
                    )
            );
        }
    }
    let home = TempHome::new("discovery-human").unwrap();
    let human = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("TAPID_DISCOVERY_TOKEN", "credential-value-canary")
        .current_dir(project.path())
        .args(["status", "--workspace", "member"])
        .output()
        .unwrap();
    let text = String::from_utf8(human.stdout).unwrap();
    for expected in [
        "UNSUPPORTED_SPEC",
        "review_manifest_requirement",
        "Workspace selection: member",
        "Workspace linking: not verified",
        "Migration guide:",
        "unchecked",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    assert!(!text.contains("credential-value-canary"));
    assert!(!text.contains("HOOK_BODY_CANARY"));
    for (path, bytes) in [
        ("package.json", root.as_slice()),
        ("member/package.json", member.as_slice()),
        ("tapid.toml", config.as_slice()),
        (".tapid-lifecycle-journal.json", b"interrupted".as_slice()),
    ] {
        assert_eq!(fs::read(project.path().join(path)).unwrap(), bytes);
    }
    assert!(!project.path().join("node_modules").exists());
    assert!(!project.path().join("tapid.lock").exists());
}

#[test]
fn discovery_distinguishes_cache_preview_from_authorized_removal() {
    let project = TempProject::new("discovery-cache-modes").unwrap();
    let (_, report) = invoke(&project, &["--json", "capabilities"]);
    for path in ["cache clean", "clean"] {
        let command = report["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["path"] == path)
            .unwrap();
        let modes = command["modes"].as_array().unwrap();
        let preview = modes.iter().find(|mode| mode["id"] == "default").unwrap();
        assert_eq!(preview["when"], json!({"yes": false}));
        assert_eq!(preview["effects"]["store_writes_possible"], false);
        let removal = modes.iter().find(|mode| mode["id"] == "remove").unwrap();
        assert_eq!(removal["when"], json!({"yes": true}));
        assert_eq!(removal["effects"]["store_writes_possible"], true);
        let yes = command["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|argument| argument["id"] == "yes")
            .unwrap();
        assert!(
            yes["conflicts"]
                .as_array()
                .unwrap()
                .contains(&json!("dry_run"))
        );
        for mode in modes {
            assert_eq!(mode["schema_coverage"], "envelope_only");
            assert_eq!(mode["effects"]["network_possible"], false);
            assert_eq!(mode["effects"]["child_execution_possible"], false);
        }
        let mut args = vec!["--json"];
        args.extend(path.split(' '));
        args.extend(["--store-dir", "cache", "--dry-run"]);
        let (output, _) = invoke(&project, &args);
        assert!(output.status.success());
        assert!(!project.path().join("cache").exists());
        args.pop();
        args.push("--yes");
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
    }
}

#[test]
fn discovery_guides_imported_replay_without_promising_native_mutation() {
    use sha2::{Digest, Sha256};
    let project = TempProject::new("discovery-imported-operations").unwrap();
    // Reuse the focused status fixture's imported graph with a native-only
    // override exclusion. Import acceptance must not bless later mutations.
    let root = r#"{"name":"root","version":"1.0.0","overrides":{"jsr:@std/path":"1"}}"#;
    let npm =
        json!({"lockfileVersion":3,"packages":{"":{"name":"root","version":"1.0.0"}}}).to_string();
    let lock = tapid_lockfile::ImportedNpmLockfile::import(
        &npm,
        root,
        &format!("sha256-{:x}", Sha256::digest(root.as_bytes())),
    )
    .unwrap()
    .to_json()
    .unwrap();
    project.write("package.json", root.as_bytes()).unwrap();
    project.write("package-lock.json", npm.as_bytes()).unwrap();
    project.write("tapid.lock", lock.as_bytes()).unwrap();
    let (_, discovery) = invoke(&project, &["--json", "capabilities"]);
    let status = discovery["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|command| command["path"] == "status")
        .unwrap();
    let id = status["modes"][0]["schema"].as_str().unwrap();
    let (_, schema) = invoke(&project, &["--json", "capabilities", "--schema", id]);
    let (output, report) = invoke(&project, &["--json", "status"]);
    assert!(!output.status.success());
    assert!(
        jsonschema::validator_for(&schema["data"]["schema"])
            .unwrap()
            .is_valid(&report["data"])
    );
    let data = &report["data"];
    assert_eq!(data["manifest_lock_agreement"], "match");
    assert_eq!(
        data["operations"]["imported_replay"]["support"],
        "supported"
    );
    assert_eq!(
        data["operations"]["imported_replay"]["scope"],
        "manifest_receipts_only"
    );
    for operation in ["native_resolution", "dependency_mutation"] {
        assert_eq!(data["operations"][operation]["support"], "unsupported");
    }
    let blocker = data["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["code"] == "UNSUPPORTED_OVERRIDE")
        .unwrap();
    assert_eq!(blocker["action"]["parameters"]["field"], "/overrides");
    assert_eq!(blocker["action"]["id"], "review_manifest_requirement");
    assert_eq!(
        blocker["operations"],
        json!(["native_resolution", "dependency_mutation"])
    );
    for (path, bytes) in [
        ("package.json", root.as_bytes()),
        ("package-lock.json", npm.as_bytes()),
        ("tapid.lock", lock.as_bytes()),
    ] {
        assert_eq!(fs::read(project.path().join(path)).unwrap(), bytes);
    }
    assert!(!project.path().join("node_modules").exists());
}
