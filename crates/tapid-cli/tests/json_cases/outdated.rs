use super::{command, invoke};
use std::{fs, process::Command};
use tapid_test_support::{TempHome, TempProject};

#[test]
fn outdated_schema_6_uses_direct_root_instead_of_newer_transitive_version() {
    let project = TempProject::new("outdated-legacy-root").unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"^1.0.0","alias-parent":"1.0.0"}}"#).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/npm-aliases.json")).unwrap();
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let store = project.path().join("store");
    let (output, installed) = invoke(
        &project,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{installed}");
    let lock_path = project.path().join("tapid.lock");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    lock["lockfileVersion"] = 6.into();
    lock.as_object_mut().unwrap().remove("rootBindings");
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        if let Some(aliases) = package.as_object_mut().unwrap().remove("dependencyAliases") {
            for (local, actual) in aliases.as_object().unwrap() {
                let dependencies = package["dependencies"].as_object_mut().unwrap();
                let target = dependencies.remove(local).unwrap();
                dependencies.insert(actual.as_str().unwrap().to_owned(), target);
            }
        }
    }
    fs::write(&lock_path, lock.to_string()).unwrap();
    let before = fs::read(&lock_path).unwrap();
    let (output, report) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{report}");
    let entry = report["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["identity"] == "h3")
        .unwrap();
    assert_eq!(entry["locked"], "1.0.0");
    assert_eq!(
        entry["compatible_impact"],
        serde_json::json!({"version_change":"unchanged","lockfile":"unchanged","manifest":"unchanged"})
    );
    assert_eq!(
        entry["available_impact"],
        serde_json::json!({"version_change":"major","lockfile":"changed","manifest":"changed"})
    );
    assert_eq!(fs::read(&lock_path).unwrap(), before);
}

#[test]
fn outdated_peer_declarations_do_not_claim_direct_lockfile_changes() {
    let project = TempProject::new("outdated-peer-only").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project
        .write(
            "registry.json",
            include_bytes!("../fixtures/npm-aliases.json"),
        )
        .unwrap();
    let store = project.path().join("store");
    let (output, added) = invoke(
        &project,
        &[
            "--json",
            "add",
            "h3@^1.0.0",
            "--peer",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{added}");
    assert_eq!(added["data"]["package_count"], 0);
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let (output, report) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{report}");
    let entry = &report["data"]["entries"][0];
    assert_eq!(entry["kind"], "peerDependencies");
    assert!(entry["locked"].is_null());
    assert_eq!(
        entry["compatible_impact"],
        serde_json::json!({"version_change":"unlocked", "lockfile":"unchanged", "manifest":"unchanged"})
    );
    assert_eq!(
        entry["available_impact"],
        serde_json::json!({"version_change":"unlocked", "lockfile":"unchanged", "manifest":"changed"})
    );
    let home = TempHome::new("json-outdated-human").unwrap();
    let human = command(&project, &home, &[])
        .current_dir(project.path())
        .args(["outdated", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("compatible-change=unlocked compatible-lockfile=unchanged"));
    assert!(stdout.contains(
        "available-change=unlocked available-lockfile=unchanged available-manifest=changed"
    ));
    let (output, offline) = invoke(&project, &["--json", "outdated", "--offline"]);
    assert!(output.status.success(), "{offline}");
    assert_eq!(offline["outcome"], "partial");
    assert_eq!(
        offline["data"]["entries"][0]["available_impact"],
        serde_json::json!({"version_change":"unknown", "lockfile":"unchanged", "manifest":"unknown"})
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
    // A provider installed by another section still is not selected by its peer range.
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"^1.0.0"},"peerDependencies":{"h3":"^1.0.0"}}"#).unwrap();
    let (output, added) = invoke(
        &project,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{added}");
    let (output, overlapping) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{overlapping}");
    let entries = overlapping["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert_eq!(entry["locked"], "1.0.0");
        assert_eq!(entry["available_impact"]["version_change"], "major");
        assert_eq!(entry["available_impact"]["manifest"], "changed");
        assert_eq!(
            entry["available_impact"]["lockfile"],
            if entry["kind"] == "peerDependencies" {
                "unchanged"
            } else {
                "changed"
            }
        );
    }
}

#[test]
fn json_outdated_failures_do_not_label_locked_versions_as_selected() {
    let project = TempProject::new("json-outdated-failure-version").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    project
        .write(
            "registry.json",
            include_bytes!("../fixtures/npm-aliases.json"),
        )
        .unwrap();
    project.write("empty.json", br#"{"packages":[]}"#).unwrap();
    project.write("invalid.json", b"not json").unwrap();
    let store = project.path().join("store");
    let (output, installed) = invoke(
        &project,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{installed}");
    for (requirement, options, code) in [
        ("1.0.0", vec!["--offline"], "REGISTRY_METADATA_UNAVAILABLE"),
        (
            "1.0.0",
            vec!["--registry-fixture", "empty.json"],
            "REGISTRY_METADATA_INVALID",
        ),
        (
            "1.0.0",
            vec!["--registry-fixture", "invalid.json"],
            "REGISTRY_METADATA_INVALID",
        ),
        (
            "invalid requirement ??",
            vec!["--registry-fixture", "registry.json"],
            "INVALID_REQUEST",
        ),
    ] {
        project.write("package.json", serde_json::json!({"name":"app","version":"1.0.0","dependencies":{"h3":requirement}}).to_string().as_bytes()).unwrap();
        let mut args = vec!["--json", "outdated"];
        args.extend(options);
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["outcome"], "partial");
        let entry = &result["data"]["entries"][0];
        assert_eq!(entry["error"]["code"], code);
        assert_eq!(entry["locked"], "1.0.0");
        assert_eq!(entry["error"]["context"]["package"]["name"], "h3");
        assert!(
            entry["error"]["context"]["package"]["version"].is_null(),
            "{entry}"
        );
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
