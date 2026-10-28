use super::{command_support::CliFixture, lock_for_manifest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs;

#[test]
fn dependency_dry_run_resolves_transitives_without_writing_or_fetching_artifacts() {
    let scenario = CliFixture::new("dependency-preview");
    let manifest = br#"{"name":"app","version":"1.0.0","description":"preserve"}"#;
    scenario.project.write("package.json", manifest).unwrap();
    let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
    let fixture = serde_json::json!({"packages":[
        {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":integrity, "artifact":"never-download", "dependencies":{"child":"^1.0.0"}},
        {"registry":"https://registry.npmjs.org", "name":"child", "version":"1.2.0", "integrity":integrity, "artifact":"never-download"}
    ]});
    scenario
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = scenario
        .command(&[
            "--json",
            "add",
            "parent@^1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["operation"], "add");
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(value["data"]["preview_kind"], "resolved_graph");
    assert_eq!(value["data"]["declarations"][0]["after"], "^1.0.0");
    let packages = value["data"]["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 2);
    let child = packages.iter().find(|p| p["name"] == "child").unwrap();
    assert_eq!(child["change"], "added");
    assert!(child["reasons"].to_string().contains("parent"));
    assert_eq!(value["data"]["effects"]["artifact_requests"], 0);
    assert_eq!(value["data"]["effects"]["cache_writes"], false);
    assert_eq!(value["data"]["effects"]["network_allowed"], false);
    assert_eq!(
        fs::read(scenario.project.path().join("package.json")).unwrap(),
        manifest
    );
    for path in [
        "tapid.lock",
        "node_modules",
        "store",
        ".tapid-activation.lock",
    ] {
        assert!(!scenario.project.path().join(path).exists(), "{path}");
    }
}

fn preview_project(label: &str) -> CliFixture {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    let scenario = CliFixture::new(label);
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0"}}"#;
    scenario
        .project
        .write("package.json", manifest.as_bytes())
        .unwrap();
    let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
    let mut lock = lock_for_manifest(manifest);
    let child = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "child",
        "1.0.0",
        &integrity,
        &format!("sha256-{}", "01".repeat(32)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let mut parent = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "parent",
        "1.0.0",
        &integrity,
        &format!("sha256-{}", "02".repeat(32)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    parent.add_dependency("child", &child.key()).unwrap();
    let root = parent.key();
    lock.insert_package(child).unwrap();
    lock.insert_package(parent).unwrap();
    lock.set_roots([root.clone()]).unwrap();
    lock.set_root_bindings(std::collections::BTreeMap::from([("parent".into(), root)]))
        .unwrap();
    scenario
        .project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let fixture = serde_json::json!({"packages":[
        {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.1.0", "integrity":integrity, "artifact":"never-download", "dependencies":{"child":"^2.0.0"}},
        {"registry":"https://registry.npmjs.org", "name":"child", "version":"2.0.0", "integrity":integrity, "artifact":"never-download"}
    ]});
    scenario
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    scenario
}

#[test]
fn dependency_dry_run_update_and_remove_explain_transitive_changes() {
    for operation in ["update", "remove"] {
        let scenario = preview_project(operation);
        let before = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
        let manifest = fs::read(scenario.project.path().join("package.json")).unwrap();
        let output = scenario
            .command(&[
                "--json",
                operation,
                "parent",
                "--dry-run",
                "--registry-fixture",
                "registry.json",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let packages = value["data"]["packages"].as_array().unwrap();
        let removed = packages
            .iter()
            .find(|p| p["name"] == "child" && p["change"] == "removed")
            .unwrap();
        assert!(removed["reasons"].to_string().contains("parent"));
        if operation == "update" {
            assert!(value["data"]["declarations"].as_array().unwrap().is_empty());
            assert!(packages.iter().any(|p| p["name"] == "child"
                && p["version"] == "2.0.0"
                && p["change"] == "added"));
        } else {
            assert!(value["data"]["declarations"][0]["after"].is_null());
            assert_eq!(packages.len(), 2);
        }
        assert_eq!(
            fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
            before
        );
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            manifest
        );
        assert!(
            !scenario
                .project
                .path()
                .join(".tapid-activation.lock")
                .exists()
        );
    }
}

#[test]
fn dependency_dry_run_offline_install_uses_lock_without_inspecting_store() {
    let scenario = preview_project("offline-preview");
    let output = scenario
        .command(&[
            "--json",
            "install",
            "--dry-run",
            "--offline",
            "--store-dir",
            "missing-store",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["effects"]["network_allowed"], false);
    assert_eq!(value["data"]["effects"]["cache_reads"], false);
    assert!(
        value["data"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["change"] == "unchanged")
    );
    assert!(!scenario.project.path().join("missing-store").exists());
    assert!(!scenario.project.path().join("node_modules").exists());
}

#[test]
fn dependency_dry_run_refuses_pending_recovery_without_changing_it() {
    let scenario = preview_project("pending-preview");
    let journal = b"incomplete journal retained";
    scenario
        .project
        .write(".tapid-lifecycle-journal.json", journal)
        .unwrap();
    let output = scenario
        .command(&[
            "--json",
            "update",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "RECOVERY_FAILED");
    assert_eq!(
        fs::read(
            scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
        )
        .unwrap(),
        journal
    );
    assert!(
        !scenario
            .project
            .path()
            .join(".tapid-activation.lock")
            .exists()
    );
}

#[test]
fn dependency_dry_run_workspace_edits_resolve_the_member_graph_in_memory() {
    let scenario = preview_project("workspace-preview");
    scenario
        .project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    let member = br#"{"name":"member","version":"1.0.0","scripts":{"prepare":"must not execute"}}"#;
    scenario
        .project
        .write("packages/member/package.json", member)
        .unwrap();
    fs::remove_file(scenario.project.path().join("tapid.lock")).unwrap();
    let output = scenario
        .command(&[
            "--json",
            "add",
            "parent@^1.0.0",
            "--workspace",
            "member",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = value["data"]["packages"].as_array().unwrap();
    let receipt = &value["data"]["starting_state"];
    assert_eq!(
        receipt["selected_manifest"]["path"],
        "packages/member/package.json"
    );
    assert_eq!(receipt["selected_manifest"], receipt["workspace"][0]);
    assert_ne!(receipt["selected_manifest"], receipt["root_manifest"]);
    let member_node = packages.iter().find(|p| p["name"] == "member").unwrap();
    let target: tapid_lockfile::LockfilePackageKey = member_node["dependencies"]["parent"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(target.name.as_str(), "parent");
    assert_eq!(target.version.to_string(), "1.1.0");
    let child = packages.iter().find(|p| p["name"] == "child").unwrap();
    assert_eq!(child["version"], "2.0.0");
    assert_eq!(
        fs::read(scenario.project.path().join("packages/member/package.json")).unwrap(),
        member
    );
    assert!(!scenario.project.path().join("tapid.lock").exists());
    assert!(
        !scenario
            .project
            .path()
            .join("packages/member/node_modules")
            .exists()
    );
}

#[test]
fn dependency_dry_run_install_preserves_matching_lock_and_reports_no_metadata_lookups() {
    let scenario = preview_project("preserve-preview");
    let output = scenario
        .command(&[
            "--json",
            "install",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["effects"]["metadata_source"], "lockfile");
    assert_eq!(value["data"]["effects"]["metadata_lookups"], 0);
    assert!(
        value["data"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["change"] == "unchanged")
    );
    assert_eq!(value["data"]["artifacts_prepared"], false);
}

#[test]
fn dependency_dry_run_resolution_failure_preserves_existing_project_and_store() {
    let scenario = preview_project("failed-preview");
    let manifest = fs::read(scenario.project.path().join("package.json")).unwrap();
    let lock = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
    scenario
        .project
        .write("store/unchanged", b"cached content")
        .unwrap();
    scenario
        .project
        .write("node_modules/user-content", b"installed content")
        .unwrap();
    let output = scenario
        .command(&[
            "--json",
            "add",
            "missing@1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "RESOLUTION_FAILED");
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read(scenario.project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
        lock
    );
    assert_eq!(
        fs::read(scenario.project.path().join("store/unchanged")).unwrap(),
        b"cached content"
    );
    assert_eq!(
        fs::read(scenario.project.path().join("node_modules/user-content")).unwrap(),
        b"installed content"
    );
    assert_eq!(
        fs::read_dir(scenario.project.path().join("store"))
            .unwrap()
            .count(),
        1
    );
    assert!(
        !scenario
            .project
            .path()
            .join(".tapid-activation.lock")
            .exists()
    );
}

#[test]
fn dependency_dry_run_rejects_copied_sources_before_replay_or_artifact_access() {
    let scenario = CliFixture::new("copied-preview");
    let manifest =
        br#"{"name":"app","version":"1.0.0","dependencies":{"local":"file:missing.tgz"}}"#;
    scenario.project.write("package.json", manifest).unwrap();
    let output = scenario
        .command(&["--json", "install", "--offline", "--dry-run"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "INVALID_REQUEST");
    assert_eq!(
        fs::read(scenario.project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(
        !scenario
            .project
            .path()
            .join(".tapid-activation.lock")
            .exists()
    );
}

#[test]
fn dependency_dry_run_install_package_and_latest_show_declaration_edits() {
    for args in [
        vec!["install", "child@^2.0.0"],
        vec!["update", "parent", "--latest"],
    ] {
        let scenario = preview_project("declaration-preview");
        let manifest = fs::read(scenario.project.path().join("package.json")).unwrap();
        let mut command_args = vec!["--json"];
        command_args.extend(args.iter().copied());
        command_args.extend(["--dry-run", "--registry-fixture", "registry.json"]);
        let output = scenario.command(&command_args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let after = if args[0] == "install" {
            "^2.0.0"
        } else {
            "^1.1.0"
        };
        assert_eq!(value["data"]["declarations"][0]["after"], after);
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            manifest
        );
    }
}

#[test]
fn dependency_dry_run_compatibility_install_resolves_unverified_pins_again() {
    let scenario = preview_project("unverified-preview");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(scenario.project.path().join("tapid.lock")).unwrap())
            .unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["registryIntegrityDeclared"] = false.into();
    }
    scenario
        .project
        .write("tapid.lock", lock.to_string().as_bytes())
        .unwrap();
    let output = scenario
        .command(&[
            "--json",
            "install",
            "--dry-run",
            "--allow-unverified-registry-artifacts",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["effects"]["metadata_source"], "fixture");
    assert!(
        value["data"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "parent" && p["version"] == "1.1.0" && p["change"] == "added")
    );
    assert_eq!(
        value["warnings"][0],
        "UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED"
    );
}

#[test]
fn dependency_dry_run_does_not_present_locked_pins_as_declared_transitive_ranges() {
    let scenario = preview_project("locked-reasons-preview");
    let output = scenario
        .command(&[
            "--json",
            "add",
            "react@^18.0.0",
            "--peer",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let child = value["data"]["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "child")
        .unwrap();
    assert_eq!(child["version"], "1.0.0");
    let reason = child["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["graph"] == "after")
        .unwrap();
    assert!(
        reason["requirement"].is_null(),
        "lockfile pins do not preserve declared ranges: {reason}"
    );
}

#[test]
fn dependency_dry_run_reports_new_root_binding_for_an_existing_transitive_package() {
    let scenario = preview_project("root-binding-preview");
    let output = scenario
        .command(&[
            "--json",
            "add",
            "child@1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let roots = value["data"]["root_changes"].as_array().unwrap();
    let child = roots.iter().find(|r| r["name"] == "child").unwrap();
    assert!(child["before"].is_null());
    assert!(child["after"].is_string());
    let node = value["data"]["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "child")
        .unwrap();
    assert_eq!(node["change"], "unchanged");
}

#[test]
fn dependency_dry_run_truncation_pointers_are_sorted_and_unique() {
    let scenario = preview_project("preview-truncation-order");
    let path = scenario.project.path().join("package.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let requirement = format!("^1.0.0{}", " ".repeat(5000));
    for index in 0..12 {
        manifest["peerDependencies"][format!("peer-{index:02}")] = requirement.clone().into();
    }
    scenario
        .project
        .write("package.json", manifest.to_string().as_bytes())
        .unwrap();
    let fixture_path = scenario.project.path().join("registry.json");
    let mut fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture_path).unwrap()).unwrap();
    for index in 0..12 {
        fixture["packages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "registry":"https://registry.npmjs.org",
                "name":format!("peer-{index:02}"),
                "version":"1.0.0",
                "integrity":format!("sha512-{}", STANDARD.encode([1; 64])),
                "artifact":"never-download"
            }));
    }
    scenario
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = scenario
        .command(&[
            "--json",
            "update",
            "--latest",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let pointers = result["truncated_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(pointers.contains(&"/data/declarations/10/before"));
    assert!(
        pointers.windows(2).all(|pair| pair[0] < pair[1]),
        "{pointers:?}"
    );
}

#[test]
fn dependency_dry_run_named_update_preserves_alias_pin_and_reports_save_policy() {
    for (flag, saved) in [
        ("--save-exact", "1.1.0"),
        ("--save-caret", "^1.1.0"),
        ("--save-tilde", "~1.1.0"),
    ] {
        let scenario = preview_project("named-update-save-preview");
        let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0","unrelated":"npm:parent@*"}} "#;
        scenario
            .project
            .write("package.json", manifest.as_bytes())
            .unwrap();
        let mut lock = tapid_lockfile::Lockfile::from_json(
            &fs::read_to_string(scenario.project.path().join("tapid.lock")).unwrap(),
        )
        .unwrap();
        // Rebind the fixture lock to the expanded manifest and retain both aliases.
        let mut expanded = lock_for_manifest(manifest);
        for package in lock.packages().values() {
            expanded.insert_package(package.clone()).unwrap();
        }
        expanded.set_roots(lock.roots().iter().cloned()).unwrap();
        let root = lock.root_bindings()["parent"].clone();
        expanded
            .set_root_bindings(std::collections::BTreeMap::from([
                ("parent".into(), root.clone()),
                ("unrelated".into(), root),
            ]))
            .unwrap();
        lock = expanded;
        let lock_bytes = lock.to_json().unwrap();
        scenario
            .project
            .write("tapid.lock", lock_bytes.as_bytes())
            .unwrap();
        let output = scenario
            .command(&[
                "--json",
                "update",
                "parent",
                "--latest",
                flag,
                "--dry-run",
                "--registry-fixture",
                "registry.json",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["data"]["declarations"][0]["after"], saved);
        let changes = value["data"]["root_changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1, "{value}");
        assert_eq!(changes[0]["name"], "parent");
        assert!(changes[0]["after"].as_str().unwrap().contains("1.1.0"));
        assert_eq!(value["data"]["effects"]["project_writes"], false);
        assert_eq!(value["data"]["effects"]["cache_writes"], false);
        assert_eq!(
            fs::read_to_string(scenario.project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(scenario.project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
        for path in ["node_modules", ".tapid-activation.lock", ".tapid"] {
            assert!(!scenario.project.path().join(path).exists(), "{path}");
        }
    }
}

#[test]
fn dependency_dry_run_reports_starting_state() {
    let scenario = CliFixture::new("preview-starting-state");
    scenario
        .project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let inspect = || {
        let output = scenario
            .command(&["--json", "install", "--dry-run"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["data"]["starting_state"].clone()
    };
    let first = inspect();
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["root_manifest"]["path"], "package.json");
    assert_eq!(first["root_manifest"], first["selected_manifest"]);
    assert_eq!(
        first["root_manifest"]["digest"],
        "sha256-fee412776ee7d466e425de58510ba8d794ec7091437d22a4f1955bdfd40a9291"
    );
    assert_eq!(first["lock"]["state"], "absent");
    assert_eq!(first, inspect());
    let human = scenario
        .command(&["install", "--dry-run"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("rechecked, non-atomic observation"));
    assert!(human.contains(first["root_manifest"]["digest"].as_str().unwrap()));
    assert!(human.contains("Artifacts are not prepared"));

    scenario
        .project
        .write(
            "package.json",
            b"{\"name\":\"app\",\"version\":\"1.0.0\"}\n",
        )
        .unwrap();
    assert_ne!(
        first["root_manifest"]["digest"],
        inspect()["root_manifest"]["digest"]
    );
}

#[test]
fn dependency_dry_run_receipt_covers_workspace_routing_policy_and_lock() {
    let scenario = CliFixture::new("preview-receipt-coverage");
    let root = br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#;
    scenario.project.write("package.json", root).unwrap();
    scenario
        .project
        .write("registry.json", serde_json::json!({"packages":[{"registry":"https://registry.npmjs.org","name":"unused","version":"1.0.0","integrity":format!("sha512-{}==", "A".repeat(86)),"artifact":"never-download"}]}).to_string().as_bytes())
        .unwrap();
    for name in ["a", "b"] {
        scenario
            .project
            .write(
                format!("packages/{name}/package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0"}}"#).as_bytes(),
            )
            .unwrap();
    }
    scenario.project.write("tapid.toml", b"[registries.default]\nurl='https://registry.npmjs.org'\ntoken-env='TAPID_PREVIEW_RECEIPT_SECRET'\n").unwrap();
    let inspect = |secret: &str| {
        let output = scenario
            .command(&[
                "--json",
                "install",
                "--dry-run",
                "--registry-fixture",
                "registry.json",
            ])
            .env("TAPID_PREVIEW_RECEIPT_SECRET", secret)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["data"]["starting_state"].clone()
    };
    let first = inspect("first-secret-value");
    assert_eq!(first, inspect("second-secret-value"));
    assert_eq!(first["workspace"][0]["path"], "packages/a/package.json");
    assert_eq!(first["workspace"][1]["path"], "packages/b/package.json");
    assert_eq!(first["lifecycle_policy"]["state"], "absent_default_deny");
    scenario
        .project
        .write(
            "packages/b/package.json",
            br#"{"name":"b","version":"2.0.0"}"#,
        )
        .unwrap();
    let member_changed = inspect("third-secret-value");
    assert_eq!(first["root_manifest"], member_changed["root_manifest"]);
    assert_ne!(
        first["workspace"][1]["digest"],
        member_changed["workspace"][1]["digest"]
    );
    scenario
        .project
        .write(
            "packages/c/package.json",
            br#"{"name":"c","version":"1.0.0"}"#,
        )
        .unwrap();
    assert_eq!(
        inspect("fourth-secret-value")["workspace"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    scenario
        .project
        .write("tapid.lifecycle.toml", b"schema=1\n")
        .unwrap();
    let policy = inspect("fifth-secret-value");
    assert_eq!(policy["lifecycle_policy"]["state"], "present");
    scenario
        .project
        .write("tapid.lifecycle.toml", b"schema=1\n# comment\n")
        .unwrap();
    assert_ne!(
        policy["lifecycle_policy"]["digest"],
        inspect("sixth-secret-value")["lifecycle_policy"]["digest"]
    );
    scenario.project.write("tapid.toml", b"[registries.default]\nurl='https://mirror.example'\ntoken-env='TAPID_PREVIEW_RECEIPT_SECRET'\n").unwrap();
    assert_ne!(
        first["registry"],
        inspect("seventh-secret-value")["registry"]
    );

    let locked = preview_project("preview-receipt-lock");
    let read = |flag: &str| {
        let output = locked
            .command(&["--json", "install", "--dry-run", flag])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["data"]["starting_state"].clone()
    };
    let offline = read("--offline");
    assert_eq!(offline["lock"]["state"], "present");
    assert_eq!(offline["operation"]["mode"], "offline");
    let frozen = read("--frozen");
    assert_eq!(offline["lock"], frozen["lock"]);
    assert_eq!(frozen["operation"]["mode"], "frozen");
    let path = locked.project.path().join("tapid.lock");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
    assert_ne!(
        offline["lock"]["digest"],
        read("--offline")["lock"]["digest"]
    );
}

#[test]
fn dependency_dry_run_matches_real_operations_on_equivalent_projects() {
    for operation in ["install", "add", "remove", "update"] {
        let preview = CliFixture::new("preview-parity-observation");
        let actual = CliFixture::new("preview-parity-operation");
        for scenario in [&preview, &actual] {
            let manifest = if operation == "add" {
                br#"{"name":"app","version":"1.0.0"}"#.as_slice()
            } else {
                br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"^1.0.0","unrelated":"npm:h3@1.0.0"}}"#.as_slice()
            };
            scenario.project.write("package.json", manifest).unwrap();
            scenario
                .project
                .write(
                    "registry.json",
                    include_bytes!("../fixtures/npm-aliases.json"),
                )
                .unwrap();
            if matches!(operation, "remove" | "update") {
                let output = scenario
                    .command(&["install", "--registry-fixture", "registry.json"])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
            }
        }
        let mut args = match operation {
            "install" => vec!["install"],
            "add" => vec!["add", "h3@^1.0.0"],
            "remove" => vec!["remove", "h3"],
            "update" => vec!["update", "h3", "--latest", "--save-exact"],
            _ => unreachable!(),
        };
        args.extend(["--registry-fixture", "registry.json", "--json"]);
        let manifest_before = fs::read(preview.project.path().join("package.json")).unwrap();
        let lock_before = fs::read(preview.project.path().join("tapid.lock")).ok();
        let mut preview_args = args.clone();
        preview_args.push("--dry-run");
        let output = preview.command(&preview_args).output().unwrap();
        assert!(output.status.success(), "{operation}: {output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            fs::read(preview.project.path().join("package.json")).unwrap(),
            manifest_before
        );
        assert_eq!(
            fs::read(preview.project.path().join("tapid.lock")).ok(),
            lock_before
        );
        let output = actual.command(&args).output().unwrap();
        assert!(output.status.success(), "{operation}: {output:?}");
        let lock = tapid_lockfile::Lockfile::from_json(
            &fs::read_to_string(actual.project.path().join("tapid.lock")).unwrap(),
        )
        .unwrap();
        let predicted = report["data"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| !p["after"].is_null())
            .map(|p| p["key"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            predicted,
            lock.packages().keys().cloned().collect(),
            "{operation}"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(actual.project.path().join("package.json")).unwrap())
                .unwrap();
        for edit in report["data"]["declarations"].as_array().unwrap() {
            assert_eq!(
                manifest[edit["section"].as_str().unwrap()][edit["name"].as_str().unwrap()],
                edit["after"],
                "{operation}"
            );
        }
        for package in report["data"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| !p["after"].is_null())
        {
            assert_eq!(
                serde_json::to_value(
                    lock.packages()[package["key"].as_str().unwrap()].dependencies()
                )
                .unwrap(),
                package["after"]["dependencies"],
                "{operation}"
            );
        }
        if operation == "update" {
            assert!(lock.root_bindings()["unrelated"].contains("1.0.0"));
            assert_eq!(
                report["data"]["starting_state"]["operation"]["update"]["save_policy"],
                "exact"
            );
        }
    }
}

#[test]
fn dependency_dry_run_retains_recorded_optional_omissions() {
    use tapid_lockfile::*;
    let scenario = CliFixture::new("preview-omissions");
    let manifest = r#"{"name":"app","version":"1.0.0","optionalDependencies":{"missing":"^1"}}"#;
    scenario
        .project
        .write("package.json", manifest.as_bytes())
        .unwrap();
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
            name: "missing".into(),
            requirement: "^1".into(),
            original_requirement: None,
            registry: "https://registry.npmjs.org".into(),
            target: None,
            reason: OmissionReason::MetadataAbsent,
        }],
    })
    .unwrap();
    let bytes = lock.to_json().unwrap();
    scenario
        .project
        .write("tapid.lock", bytes.as_bytes())
        .unwrap();
    for flags in [vec![], vec!["--offline"], vec!["--frozen"]] {
        let mut args = vec!["--json", "install", "--dry-run"];
        args.extend(flags);
        let output = scenario.command(&args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["selection"]["omissions"][0]["name"], "missing");
        assert_eq!(value["data"]["effects"]["metadata_source"], "lockfile");
        assert_eq!(value["data"]["effects"]["metadata_lookups"], 0);
    }
    let output = scenario
        .command(&["install", "--dry-run", "--offline"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("missing"));
    assert_eq!(
        fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
    for other_profile in [false, true] {
        let mut facts = lock.selection().unwrap().clone();
        if other_profile {
            facts.profile = SelectionProfile::Production;
        } else {
            facts.target.os = if cfg!(target_os = "windows") {
                "linux"
            } else {
                "windows"
            }
            .into();
            facts.target.libc = None;
        }
        let mut mismatched = lock.clone();
        mismatched.set_selection(facts).unwrap();
        let rejected_bytes = mismatched.to_json().unwrap();
        scenario
            .project
            .write("tapid.lock", rejected_bytes.as_bytes())
            .unwrap();
        for flag in ["--offline", "--frozen"] {
            let output = scenario
                .command(&["--json", "install", "--dry-run", flag])
                .output()
                .unwrap();
            assert!(!output.status.success());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(value["selection"].is_null());
            assert_eq!(
                fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
                rejected_bytes.as_bytes()
            );
        }
    }
    assert!(!scenario.project.path().join("node_modules").exists());
}

#[test]
fn dependency_dry_run_reports_new_optional_omissions_without_artifact_access() {
    let scenario = CliFixture::new("preview-new-omissions");
    let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"},"optionalDependencies":{"local":"npm:missing@^1.0.0"}}"#;
    scenario.project.write("package.json", manifest).unwrap();
    let fixture = serde_json::json!({"packages":[{"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":format!("sha512-{}", STANDARD.encode([1;64])), "artifact":"never-download", "optionalDependencies":{"native":"1.0.0"}}, {"registry":"https://registry.npmjs.org", "name":"native", "version":"1.0.0", "os":["unsupported-test-os"], "integrity":format!("sha512-{}", STANDARD.encode([1;64])), "artifact":"never-download"}]});
    scenario
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    for json in [true, false] {
        let mut args = vec![
            "install",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ];
        if json {
            args.insert(0, "--json");
        }
        let output = scenario.command(&args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        if json {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let omissions = result["selection"]["omissions"].as_array().unwrap();
            assert_eq!(omissions.len(), 2);
            let root = omissions.iter().find(|o| o["name"] == "local").unwrap();
            assert_eq!(root["reason"], "metadataAbsent");
            let nested = omissions.iter().find(|o| o["name"] == "native").unwrap();
            assert_eq!(nested["reason"], "incompatiblePlatform");
            assert_eq!(result["selection"]["details_complete"], true);
            assert_eq!(result["data"]["effects"]["artifact_requests"], 0);
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("local"));
        }
    }
    assert_eq!(
        fs::read(scenario.project.path().join("package.json")).unwrap(),
        manifest
    );
    for path in [
        "tapid.lock",
        "node_modules",
        "store",
        ".tapid-activation.lock",
    ] {
        assert!(!scenario.project.path().join(path).exists(), "{path}");
    }
}
