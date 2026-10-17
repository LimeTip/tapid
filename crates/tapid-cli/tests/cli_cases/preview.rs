use super::*;
use tapid_test_support::TempProject;

#[test]
fn dependency_dry_run_resolves_transitives_without_writing_or_fetching_artifacts() {
    let project = TempProject::new("dependency-preview").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0","description":"preserve"}"#;
    project.write("package.json", manifest).unwrap();
    let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
    let fixture = serde_json::json!({"packages":[
        {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":integrity, "artifact":"never-download", "dependencies":{"child":"^1.0.0"}},
        {"registry":"https://registry.npmjs.org", "name":"child", "version":"1.2.0", "integrity":integrity, "artifact":"never-download"}
    ]});
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "add",
            "parent@^1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
        true,
    )
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
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    for path in [
        "tapid.lock",
        "node_modules",
        "store",
        ".tapid-activation.lock",
    ] {
        assert!(!project.path().join(path).exists(), "{path}");
    }
}

fn preview_project(label: &str) -> TempProject {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    let project = TempProject::new(label).unwrap();
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0"}}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
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
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let fixture = serde_json::json!({"packages":[
        {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.1.0", "integrity":integrity, "artifact":"never-download", "dependencies":{"child":"^2.0.0"}},
        {"registry":"https://registry.npmjs.org", "name":"child", "version":"2.0.0", "integrity":integrity, "artifact":"never-download"}
    ]});
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    project
}

#[test]
fn dependency_dry_run_update_and_remove_explain_transitive_changes() {
    for operation in ["update", "remove"] {
        let project = preview_project(operation);
        let before = fs::read(project.path().join("tapid.lock")).unwrap();
        let manifest = fs::read(project.path().join("package.json")).unwrap();
        let output = isolated_command(
            project.path(),
            &[
                "--json",
                operation,
                "parent",
                "--dry-run",
                "--registry-fixture",
                "registry.json",
            ],
            true,
        )
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
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert!(!project.path().join(".tapid-activation.lock").exists());
    }
}

#[test]
fn dependency_dry_run_offline_install_uses_lock_without_inspecting_store() {
    let project = preview_project("offline-preview");
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "install",
            "--dry-run",
            "--offline",
            "--store-dir",
            "missing-store",
        ],
        true,
    )
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
    assert!(!project.path().join("missing-store").exists());
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn dependency_dry_run_refuses_pending_recovery_without_changing_it() {
    let project = preview_project("pending-preview");
    let journal = b"incomplete journal retained";
    project
        .write(".tapid-lifecycle-journal.json", journal)
        .unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "update",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "RECOVERY_FAILED");
    assert_eq!(
        fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        journal
    );
    assert!(!project.path().join(".tapid-activation.lock").exists());
}

#[test]
fn dependency_dry_run_workspace_edits_resolve_the_member_graph_in_memory() {
    let project = preview_project("workspace-preview");
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    let member = br#"{"name":"member","version":"1.0.0","scripts":{"prepare":"must not execute"}}"#;
    project
        .write("packages/member/package.json", member)
        .unwrap();
    fs::remove_file(project.path().join("tapid.lock")).unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "add",
            "parent@^1.0.0",
            "--workspace",
            "member",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = value["data"]["packages"].as_array().unwrap();
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
        fs::read(project.path().join("packages/member/package.json")).unwrap(),
        member
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("packages/member/node_modules").exists());
}

#[test]
fn dependency_dry_run_install_preserves_matching_lock_and_reports_no_metadata_lookups() {
    let project = preview_project("preserve-preview");
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "install",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
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
    let project = preview_project("failed-preview");
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    project.write("store/unchanged", b"cached content").unwrap();
    project
        .write("node_modules/user-content", b"installed content")
        .unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "add",
            "missing@1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
        true,
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "RESOLUTION_FAILED");
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
    assert_eq!(
        fs::read(project.path().join("store/unchanged")).unwrap(),
        b"cached content"
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/user-content")).unwrap(),
        b"installed content"
    );
    assert_eq!(
        fs::read_dir(project.path().join("store")).unwrap().count(),
        1
    );
    assert!(!project.path().join(".tapid-activation.lock").exists());
}

#[test]
fn dependency_dry_run_rejects_copied_sources_before_replay_or_artifact_access() {
    let project = TempProject::new("copied-preview").unwrap();
    let manifest =
        br#"{"name":"app","version":"1.0.0","dependencies":{"local":"file:missing.tgz"}}"#;
    project.write("package.json", manifest).unwrap();
    let output = isolated_command(
        project.path(),
        &["--json", "install", "--offline", "--dry-run"],
        true,
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "INVALID_REQUEST");
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join(".tapid-activation.lock").exists());
}

#[test]
fn dependency_dry_run_install_package_and_latest_show_declaration_edits() {
    for args in [
        vec!["install", "child@^2.0.0"],
        vec!["update", "parent", "--latest"],
    ] {
        let project = preview_project("declaration-preview");
        let manifest = fs::read(project.path().join("package.json")).unwrap();
        let mut command_args = vec!["--json"];
        command_args.extend(args.iter().copied());
        command_args.extend(["--dry-run", "--registry-fixture", "registry.json"]);
        let output = isolated_command(project.path(), &command_args, true)
            .output()
            .unwrap();
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
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
    }
}

#[test]
fn dependency_dry_run_compatibility_install_resolves_unverified_pins_again() {
    let project = preview_project("unverified-preview");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("tapid.lock")).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["registryIntegrityDeclared"] = false.into();
    }
    project
        .write("tapid.lock", lock.to_string().as_bytes())
        .unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "install",
            "--dry-run",
            "--allow-unverified-registry-artifacts",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
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
    let project = preview_project("locked-reasons-preview");
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "add",
            "react@^18.0.0",
            "--peer",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
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
    let project = preview_project("root-binding-preview");
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "add",
            "child@1.0.0",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
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
    let project = preview_project("preview-truncation-order");
    let path = project.path().join("package.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let requirement = format!("^1.0.0{}", " ".repeat(5000));
    for index in 0..12 {
        manifest["peerDependencies"][format!("peer-{index:02}")] = requirement.clone().into();
    }
    project
        .write("package.json", manifest.to_string().as_bytes())
        .unwrap();
    let fixture_path = project.path().join("registry.json");
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
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = isolated_command(
        project.path(),
        &[
            "--json",
            "update",
            "--latest",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ],
        true,
    )
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
        let project = preview_project("named-update-save-preview");
        let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0","unrelated":"npm:parent@*"}} "#;
        project.write("package.json", manifest.as_bytes()).unwrap();
        let mut lock = tapid_lockfile::Lockfile::from_json(
            &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
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
        project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
        let output = isolated_command(
            project.path(),
            &[
                "--json",
                "update",
                "parent",
                "--latest",
                flag,
                "--dry-run",
                "--registry-fixture",
                "registry.json",
            ],
            true,
        )
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
            fs::read_to_string(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
        for path in ["node_modules", ".tapid-activation.lock", ".tapid"] {
            assert!(!project.path().join(path).exists(), "{path}");
        }
    }
}
