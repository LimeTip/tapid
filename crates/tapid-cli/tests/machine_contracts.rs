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
    assert_eq!(catalog.len(), 3);
    let advertised = catalog
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    let referenced = discovery["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|command| command["modes"].as_array().unwrap())
        .filter_map(|mode| mode["schema"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(advertised, referenced);
    for entry in catalog {
        let id = entry["id"].as_str().unwrap();
        let (output, exported) = invoke(&project, &["--json", "capabilities", "--schema", id]);
        assert!(output.status.success());
        assert!(output.stdout.len() < 64 * 1024);
        let source: Value = serde_json::from_str(match id {
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
