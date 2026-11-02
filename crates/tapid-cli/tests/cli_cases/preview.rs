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
fn overridden_preview_does_not_claim_a_locked_pin_is_the_original_range() {
    let scenario = preview_project("locked-override-reasons");
    scenario.project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0"},"overrides":{"parent":{"child":"1.0.0"}}}"#).unwrap();
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
        .find(|package| package["name"] == "child")
        .unwrap();
    let reason = child["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reason| reason["graph"] == "after")
        .unwrap();
    assert_eq!(reason["requirement"], "1.0.0");
    assert!(reason["original_requirement"].is_null(), "{reason}");
    assert_eq!(
        reason["override_rules"],
        serde_json::json!(["/overrides/parent/child"])
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
fn targeted_override_preview_changes_only_the_matching_parent_without_writes() {
    let scenario = CliFixture::new("targeted-override-preview");
    let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"other":"1"},"overrides":{"selected@^1":{"local":"2"}}}"#;
    scenario.project.write("package.json", manifest).unwrap();
    let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
    let fixture = serde_json::json!({"packages":[
        {"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","integrity":integrity,"artifact":"never-download","dependencies":{"local":"npm:actual@1"}},
        {"registry":"https://registry.npmjs.org","name":"other","version":"1.0.0","integrity":integrity,"artifact":"never-download","dependencies":{"local":"npm:actual@1"}},
        {"registry":"https://registry.npmjs.org","name":"actual","version":"1.0.0","integrity":integrity,"artifact":"never-download"},
        {"registry":"https://registry.npmjs.org","name":"actual","version":"2.0.0","integrity":integrity,"artifact":"never-download"}
    ]});
    scenario
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = scenario
        .command(&[
            "--json",
            "add",
            "selected@npm:parent@1",
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
    let packages = value["data"]["packages"].as_array().unwrap();
    let actual = packages
        .iter()
        .filter(|package| package["name"] == "actual")
        .collect::<Vec<_>>();
    assert_eq!(actual.len(), 2, "{value}");
    let changed = actual
        .iter()
        .find(|package| package["version"] == "2.0.0")
        .unwrap();
    let reason = changed["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reason| reason["graph"] == "after")
        .unwrap();
    assert_eq!(reason["requirement"], "npm:actual@2");
    assert_eq!(reason["original_requirement"], "npm:actual@1");
    assert_eq!(
        reason["override_rules"],
        serde_json::json!(["/overrides/selected@^1/local"])
    );
    assert!(
        actual.iter().any(|package| package["version"] == "1.0.0"),
        "{value}"
    );
    assert!(
        actual.iter().any(|package| package["version"] == "2.0.0"),
        "{value}"
    );
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

#[test]
fn targeted_override_install_and_frozen_workspace_replay_keep_alias_identity() {
    let scenario = CliFixture::new("targeted-workspace-replay");
    scenario.project.write("package.json", br#"{"name":"app","version":"1.0.0","private":true,"workspaces":["packages/*"],"overrides":{"selected@^1":{"h3-v2":"2.0.0"}}}"#).unwrap();
    scenario.project.write("packages/member/package.json", br#"{"name":"member","version":"1.0.0","dependencies":{"selected":"npm:alias-parent@1"},"overrides":{"h3-v2":"9"}}"#).unwrap();
    scenario
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/npm-aliases.json"),
        )
        .unwrap();
    let store = scenario.project.path().join("store");
    let store = store.to_str().unwrap();
    let output = scenario
        .command(&[
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let why = scenario
        .command(&["--json", "why", "h3", "--workspace", "member"])
        .output()
        .unwrap();
    assert!(
        why.status.success(),
        "{}",
        String::from_utf8_lossy(&why.stderr)
    );
    let why: serde_json::Value = serde_json::from_slice(&why.stdout).unwrap();
    let step = &why["data"]["paths"][0]["steps"][1];
    assert_eq!(
        step["currentPolicy"]["rules"],
        serde_json::json!(["/overrides/selected@^1/h3-v2"])
    );
    assert_eq!(step["currentPolicy"]["requirement"], "2.0.0");
    assert!(
        why["data"]["graph_warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("historical"))
    );
    let unchanged = scenario
        .command(&["--json", "why", "@actual/scoped", "--workspace", "member"])
        .output()
        .unwrap();
    assert!(unchanged.status.success());
    let unchanged: serde_json::Value = serde_json::from_slice(&unchanged.stdout).unwrap();
    assert!(
        unchanged["data"]["paths"]
            .as_array()
            .unwrap()
            .iter()
            .all(|path| path["steps"]
                .as_array()
                .unwrap()
                .iter()
                .all(|step| step.get("currentPolicy").is_none()))
    );
    let human = scenario
        .command(&["why", "h3", "--workspace", "member"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(
        human.contains("current policy: 2.0.0 via /overrides/selected@^1/h3-v2"),
        "{human}"
    );
    assert!(human.contains("historical"));
    let lock_bytes = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
    let lock: serde_json::Value = serde_json::from_slice(&lock_bytes).unwrap();
    let packages = lock["packages"].as_object().unwrap();
    let parent = packages
        .values()
        .find(|package| package["name"] == "alias-parent")
        .unwrap();
    assert!(
        parent["dependencies"]["h3-v2"]
            .as_str()
            .unwrap()
            .contains("h3@2.0.0"),
        "{lock}"
    );
    assert!(
        !packages
            .values()
            .any(|package| package["name"] == "h3" && package["version"] == "2.0.1-rc.20")
    );
    for flag in ["--frozen", "--offline"] {
        let replay = scenario
            .command(&["install", flag, "--store-dir", store])
            .output()
            .unwrap();
        assert!(
            replay.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert_eq!(
            fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
    }
    scenario
        .project
        .write("packages/member/node_modules/KEEP", b"prior member layout")
        .unwrap();
    let mut changed: serde_json::Value =
        serde_json::from_slice(&fs::read(scenario.project.path().join("package.json")).unwrap())
            .unwrap();
    changed["overrides"]["selected@^1"]["h3-v2"] = serde_json::json!("1.0.0");
    let changed = serde_json::to_vec(&changed).unwrap();
    scenario.project.write("package.json", &changed).unwrap();
    for flag in ["--frozen", "--offline"] {
        let stale = scenario
            .command(&["install", flag, "--store-dir", store])
            .output()
            .unwrap();
        assert!(!stale.status.success(), "{flag}: stale override must fail");
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            changed
        );
        assert_eq!(
            fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
        assert_eq!(
            fs::read(
                scenario
                    .project
                    .path()
                    .join("packages/member/node_modules/KEEP")
            )
            .unwrap(),
            b"prior member layout"
        );
    }
}

#[test]
fn targeted_override_crash_recovery_settles_manifest_lock_and_layout() {
    for point in [
        "store_published",
        "lockfile_replaced",
        "activation_complete",
        "commit_decision",
    ] {
        let scenario = CliFixture::new("targeted-edit-recovery");
        let original =
            br#"{"name":"app","version":"1.0.0","overrides":{"selected@^1":{"h3-v2":"2.0.0"}}}"#;
        scenario.project.write("package.json", original).unwrap();
        scenario
            .project
            .write(
                "registry.json",
                include_bytes!("../fixtures/npm-aliases.json"),
            )
            .unwrap();
        let store_path = scenario.project.path().join("store");
        let store = store_path.to_str().unwrap();
        let install = [
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store,
        ];
        let initial = scenario.command(&install).output().unwrap();
        assert!(
            initial.status.success(),
            "{}",
            String::from_utf8_lossy(&initial.stderr)
        );
        let original_lock = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
        scenario
            .project
            .write("node_modules/KEEP", b"prior layout")
            .unwrap();
        let crashed = scenario
            .command(&[
                "add",
                "selected@npm:alias-parent@1",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store,
            ])
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(!crashed.status.success(), "{point}");
        let recovered = scenario
            .command(&["install", "--offline", "--store-dir", store])
            .env("TAPID_TEST_RECOVER_ONLY", "1")
            .output()
            .unwrap();
        assert!(
            recovered.status.success(),
            "{point}: {}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        if point == "commit_decision" {
            let manifest: serde_json::Value = serde_json::from_slice(
                &fs::read(scenario.project.path().join("package.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(manifest["dependencies"]["selected"], "npm:alias-parent@1");
            let lock: serde_json::Value = serde_json::from_slice(
                &fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
            )
            .unwrap();
            let parent = lock["packages"]
                .as_object()
                .unwrap()
                .values()
                .find(|package| package["name"] == "alias-parent")
                .unwrap();
            assert!(
                parent["dependencies"]["h3-v2"]
                    .as_str()
                    .unwrap()
                    .contains("h3@2.0.0")
            );
            assert!(
                scenario
                    .project
                    .path()
                    .join("node_modules/selected/package.json")
                    .is_file()
            );
            assert!(!scenario.project.path().join("node_modules/KEEP").exists());
            let replay = scenario
                .command(&["install", "--offline", "--frozen", "--store-dir", store])
                .output()
                .unwrap();
            assert!(
                replay.status.success(),
                "{}",
                String::from_utf8_lossy(&replay.stderr)
            );
        } else {
            assert_eq!(
                fs::read(scenario.project.path().join("package.json")).unwrap(),
                original
            );
            assert_eq!(
                fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
                original_lock
            );
            assert_eq!(
                fs::read(scenario.project.path().join("node_modules/KEEP")).unwrap(),
                b"prior layout"
            );
            assert!(
                !scenario
                    .project
                    .path()
                    .join("node_modules/selected")
                    .exists()
            );
        }
    }
}

#[test]
fn targeted_override_failed_edits_restore_manifest_lock_and_installed_layout() {
    for topology in [false, true] {
        let scenario = CliFixture::new("targeted-edit-rollback");
        let overrides = if topology {
            serde_json::json!({"selected":{"h3-v2":"2.0.0"}})
        } else {
            serde_json::json!({"selected@^1":{"h3-v2":"2.0.0"},"selected@>=1":{"h3-v2":"1.0.0"}})
        };
        let manifest =
            serde_json::json!({"name":"app","version":"1.0.0","overrides":overrides}).to_string();
        scenario
            .project
            .write("package.json", manifest.as_bytes())
            .unwrap();
        scenario
            .project
            .write(
                "registry.json",
                include_bytes!("../fixtures/npm-aliases.json"),
            )
            .unwrap();
        let store = scenario.project.path().join("store");
        let store = store.to_str().unwrap();
        let install = scenario
            .command(&[
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store,
            ])
            .output()
            .unwrap();
        assert!(
            install.status.success(),
            "{}",
            String::from_utf8_lossy(&install.stderr)
        );
        let lock = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
        scenario
            .project
            .write("node_modules/KEEP", b"prior installed layout")
            .unwrap();
        let mut args = vec!["add", "selected@npm:alias-parent@1"];
        if topology {
            args.push("other@npm:alias-parent@1");
        }
        args.extend(["--registry-fixture", "registry.json", "--store-dir", store]);
        let result = scenario.command(&args).output().unwrap();
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            error.contains(if topology {
                "OverrideTopology"
            } else {
                "OverrideConflict"
            }),
            "{error}"
        );
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(
            fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
            lock
        );
        assert_eq!(
            fs::read(scenario.project.path().join("node_modules/KEEP")).unwrap(),
            b"prior installed layout"
        );
    }
}
