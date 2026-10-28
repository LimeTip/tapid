use super::command_support::CliFixture;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs;

#[test]
fn native_pending_receipt_hydration_recovers_with_activation() {
    for point in [
        "store_published",
        "lockfile_replaced",
        "activation_complete",
        "commit_decision",
    ] {
        let scenario = CliFixture::new("pending-hydration-recovery");
        scenario
            .project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#,
            )
            .unwrap();
        scenario
            .project
            .write(
                "registry.json",
                include_bytes!("../fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let warm = scenario.project.path().join("warm");
        let args = [
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            warm.to_str().unwrap(),
        ];
        let output = scenario.command(&args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = scenario.project.path().join("tapid.lock");
        let legacy =
            tapid_lockfile::Lockfile::from_json(&fs::read_to_string(&path).unwrap()).unwrap();
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        lock["lockfileVersion"] = 11.into();
        // Preserve one complete receipt and leave the other pending.
        for (index, (key, package)) in lock["packages"]
            .as_object_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let mut selected = package.clone();
            let record = selected.as_object_mut().unwrap();
            record.remove("treeDigest");
            record.remove("unpackedDigest");
            let original = &legacy.packages()[key];
            let receipt = if index == 0 {
                serde_json::to_value(original.source_tree_receipt()).unwrap()
            } else {
                serde_json::Value::Null
            };
            record.insert("sourceTreeReceipt".into(), receipt);
            // Live selections pin URLs; fixture artifact retrieval remains local.
            if record.get("artifactUrl").is_none() {
                record.insert(
                    "artifactUrl".into(),
                    format!(
                        "https://registry.npmjs.org/{}/-/{}-{}.tgz",
                        original.name(),
                        original.name(),
                        original.version()
                    )
                    .into(),
                );
                // A retained receipt must bind the exact artifact URL.
                if index == 0 {
                    record["sourceTreeReceipt"]["artifactUrl"] = record["artifactUrl"].clone();
                }
            }
            *package = selected;
        }
        let before = lock.to_string();
        fs::write(&path, &before).unwrap();
        scenario
            .project
            .write("node_modules/sentinel", b"prior application")
            .unwrap();
        let cold = scenario.project.path().join("cold");
        let args = [
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            cold.to_str().unwrap(),
        ];
        let crash = scenario
            .command(&args)
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                crash.status.signal(),
                Some(6),
                "{point}: {}",
                String::from_utf8_lossy(&crash.stderr)
            );
        }
        #[cfg(not(unix))]
        assert!(!crash.status.success());
        if point != "commit_decision" {
            let selected = fs::read(&path).unwrap();
            let journal_path = scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json");
            let journal = fs::read(&journal_path).unwrap();
            for command in [vec!["ci"], vec!["install", "--offline"]] {
                let output = scenario.command(&command).output().unwrap();
                assert!(!output.status.success());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("pending source-tree receipt"),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(fs::read(&path).unwrap(), selected);
                assert_eq!(fs::read(&journal_path).unwrap(), journal);
            }
        }
        let recovery = scenario
            .command(&args)
            .env("TAPID_TEST_RECOVER_ONLY", "1")
            .output()
            .unwrap();
        assert!(
            recovery.status.success(),
            "{point}: {}",
            String::from_utf8_lossy(&recovery.stderr)
        );
        let completed =
            tapid_lockfile::Lockfile::from_json(&fs::read_to_string(&path).unwrap()).unwrap();
        let committed = point == "commit_decision";
        assert_eq!(!completed.has_pending_receipts(), committed, "{point}");
        assert_eq!(
            scenario
                .project
                .path()
                .join("node_modules/sentinel")
                .exists(),
            !committed,
            "{point}"
        );
        if !committed {
            assert_eq!(fs::read(&path).unwrap(), before.as_bytes());
        }
        assert!(
            !scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}

#[test]
fn lockfile_only_registry_selection_preserves_installation() {
    for installed in [false, true] {
        let scenario = CliFixture::new("lockfile-only-selection");
        let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"^1.0.0"},"scripts":{"postinstall":"must-not-run"}}"#;
        scenario.project.write("package.json", manifest).unwrap();
        let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
        // These artifacts deliberately do not exist: selection must only read metadata.
        let fixture = serde_json::json!({"packages":[
            {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":integrity, "artifact":"never-download", "dependencies":{"child":"^1.0.0"}},
            {"registry":"https://registry.npmjs.org", "name":"child", "version":"1.2.0", "integrity":integrity, "artifact":"never-download"}
        ]});
        scenario
            .project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        if installed {
            scenario
                .project
                .write("node_modules/sentinel", b"running application")
                .unwrap();
            scenario
                .project
                .write("node_modules/.bin/sentinel", b"existing shim")
                .unwrap();
            scenario
                .project
                .write(".tapid-managed", b"existing owner")
                .unwrap();
        }
        let output = scenario
            .command(&[
                "install",
                "--lockfile-only",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                "store",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Persisted dependency selections")
        );
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            manifest
        );
        assert!(!scenario.project.path().join("store").exists());
        assert!(!scenario.project.path().join("never-download").exists());
        let lock: serde_json::Value =
            serde_json::from_slice(&fs::read(scenario.project.path().join("tapid.lock")).unwrap())
                .unwrap();
        assert_eq!(lock["lockfileVersion"], 11);
        let packages = lock["packages"].as_object().unwrap();
        assert_eq!(packages.len(), 2);
        let mut selections = packages
            .values()
            .map(|p| (p["name"].as_str().unwrap(), p["version"].as_str().unwrap()))
            .collect::<Vec<_>>();
        selections.sort();
        assert_eq!(selections, [("child", "1.2.0"), ("parent", "1.0.0")]);
        if installed {
            assert_eq!(
                fs::read(scenario.project.path().join("node_modules/sentinel")).unwrap(),
                b"running application"
            );
            assert_eq!(
                fs::read(scenario.project.path().join("node_modules/.bin/sentinel")).unwrap(),
                b"existing shim"
            );
            assert_eq!(
                fs::read(scenario.project.path().join(".tapid-managed")).unwrap(),
                b"existing owner"
            );
        } else {
            assert!(!scenario.project.path().join("node_modules").exists());
            assert!(!scenario.project.path().join(".tapid-managed").exists());
        }
        assert!(
            !scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}

#[test]
fn lockfile_only_dependency_commands_report_selection_and_preserve_running_tree() {
    let scenario = CliFixture::new("lockfile-only-edits");
    scenario
        .project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    scenario
        .project
        .write("node_modules/sentinel", b"running")
        .unwrap();
    let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
    let fixture = serde_json::json!({"packages":[
        {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":integrity, "artifact":"unavailable", "dependencies":{"child":"1.0.0"}},
        {"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.2.0", "integrity":integrity, "artifact":"unavailable", "dependencies":{"child":"2.0.0"}},
        {"registry":"https://registry.npmjs.org", "name":"child", "version":"1.0.0", "integrity":integrity, "artifact":"unavailable"},
        {"registry":"https://registry.npmjs.org", "name":"child", "version":"2.0.0", "integrity":integrity, "artifact":"unavailable"}
    ]});
    scenario
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    for (operation, specs, expected_version, expected_count) in [
        ("install", vec!["parent@1.0.0"], Some("1.0.0"), 2),
        (
            "update",
            vec!["parent", "--latest", "--save-exact"],
            Some("1.2.0"),
            2,
        ),
        ("remove", vec!["parent"], None, 0),
        ("add", vec!["parent@^1", "--dev"], Some("^1"), 2),
    ] {
        let mut args = vec![
            "--json",
            operation,
            "--lockfile-only",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ];
        args.extend(specs);
        let output = scenario.command(&args).output().unwrap();
        assert!(
            output.status.success(),
            "{operation}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["operation"], operation);
        assert_eq!(result["changes"]["state"], "committed");
        assert_eq!(result["data"]["mode"], "lockfile_only");
        if operation == "update" {
            assert!(
                result["warning_details"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w["code"] == "UPDATE_GRAPH_CHANGE"
                        && w["package"].as_str().is_some_and(|p| p.contains("child")))
            );
        }

        assert_eq!(result["data"]["packages"], expected_count);
        assert_eq!(result["data"]["pending_receipts"], expected_count);
        for field in [
            "registry_artifact_requests",
            "store_writes",
            "hooks_executed",
        ] {
            assert_eq!(result["data"][field], 0);
        }
        assert_eq!(result["data"]["installed_layout_changed"], false);
        assert_eq!(result["lifecycle"], serde_json::Value::Null);
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(scenario.project.path().join("package.json")).unwrap(),
        )
        .unwrap();
        let section = if operation == "add" {
            "devDependencies"
        } else {
            "dependencies"
        };
        assert_eq!(manifest[section]["parent"].as_str(), expected_version);
        assert_eq!(
            fs::read(scenario.project.path().join("node_modules/sentinel")).unwrap(),
            b"running"
        );
        assert!(!scenario.project.path().join("store").exists());
    }
    let before = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
    let manifest = fs::read(scenario.project.path().join("package.json")).unwrap();
    let preview = scenario
        .command(&[
            "--json",
            "install",
            "--lockfile-only",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "{} {}",
        String::from_utf8_lossy(&preview.stdout),
        String::from_utf8_lossy(&preview.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(result["changes"]["state"], "unchanged");
    let failed_edit = scenario
        .command(&[
            "--json",
            "add",
            "missing",
            "--lockfile-only",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(!failed_edit.status.success());
    let failure: serde_json::Value = serde_json::from_slice(&failed_edit.stdout).unwrap();
    assert_eq!(failure["changes"]["state"], "unchanged");
    let preview = scenario
        .command(&[
            "--json",
            "add",
            "missing",
            "--lockfile-only",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(!preview.status.success());
    let failure: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(failure["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(scenario.project.path().join("package.json")).unwrap(),
        manifest
    );
}

#[test]
fn lockfile_only_rejects_replay_and_execution_overrides_before_project_access() {
    let scenario = CliFixture::new("lockfile-only-conflicts");
    for flag in [
        "--offline",
        "--frozen",
        "--allow-unverified-registry-artifacts",
        "--allow-unapproved-dependency-scripts",
        "--unsafe-no-dependency-sandbox",
    ] {
        let output = scenario
            .command(&["install", "--lockfile-only", flag])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for operation in ["add", "remove", "update"] {
        let output = scenario
            .command(&[
                operation,
                "parent",
                "--lockfile-only",
                "--allow-unverified-registry-artifacts",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let output = scenario
        .command(&["ci", "--lockfile-only"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!scenario.project.path().join("tapid.lock").exists());
    assert!(!scenario.project.path().join("node_modules").exists());
}

#[test]
fn lockfile_only_command_recovers_metadata_decisions_without_touching_workspace_installations() {
    for point in [
        "metadata_prepared",
        "metadata_committed",
        "metadata_output_published",
    ] {
        let scenario = CliFixture::new("selection-metadata-restart");
        let root = br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#;
        let member = br#"{"name":"member","version":"1.0.0"}"#;
        scenario.project.write("package.json", root).unwrap();
        scenario
            .project
            .write("packages/member/package.json", member)
            .unwrap();
        for path in [
            "node_modules/sentinel",
            "packages/member/node_modules/sentinel",
        ] {
            scenario.project.write(path, b"running").unwrap();
        }
        let integrity = format!("sha512-{}", STANDARD.encode([1; 64]));
        let fixture = serde_json::json!({"packages":[{"registry":"https://registry.npmjs.org", "name":"parent", "version":"1.0.0", "integrity":integrity, "artifact":"unavailable"}]});
        scenario
            .project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let output = scenario
            .command(&[
                "add",
                "parent@1.0.0",
                "--workspace",
                "member",
                "--lockfile-only",
                "--registry-fixture",
                "registry.json",
            ])
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "crash point {point} was not reached"
        );
        assert!(
            scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .is_file()
        );
        if point != "metadata_prepared" {
            let paths = [
                "package.json",
                "packages/member/package.json",
                "tapid.lock",
                ".tapid-lifecycle-journal.json",
            ];
            let before = paths.map(|p| fs::read(scenario.project.path().join(p)).ok());
            for args in [vec!["--json", "ci"], vec!["--json", "install", "--offline"]] {
                let output = scenario.command(&args).output().unwrap();
                assert!(!output.status.success());
                let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["changes"]["state"], "unchanged");
                for (path, bytes) in paths.iter().zip(&before) {
                    assert_eq!(
                        &fs::read(scenario.project.path().join(path)).ok(),
                        bytes,
                        "{point}: {path}"
                    );
                }
            }
        }
        let output = scenario
            .command(&[
                "install",
                "--workspace",
                "member",
                "--lockfile-only",
                "--registry-fixture",
                "registry.json",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "restart {point}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(scenario.project.path().join("packages/member/package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["dependencies"]["parent"].as_str(),
            (point != "metadata_prepared").then_some("1.0.0")
        );
        assert_eq!(
            fs::read(scenario.project.path().join("package.json")).unwrap(),
            root
        );
        for path in [
            "node_modules/sentinel",
            "packages/member/node_modules/sentinel",
        ] {
            assert_eq!(
                fs::read(scenario.project.path().join(path)).unwrap(),
                b"running"
            );
        }
        assert!(
            !scenario
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}

#[test]
fn lockfile_only_copied_source_preparation_and_import_preservation_use_real_commands() {
    let copied = CliFixture::new("selection-copied-command");
    copied.project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"file:vendor/parent.tgz"}}"#).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/npm-import/registry.json")).unwrap();
    let archive = STANDARD
        .decode(
            fixture["packages"][0]["artifact"]
                .as_str()
                .unwrap()
                .strip_prefix("base64:")
                .unwrap(),
        )
        .unwrap();
    copied.project.write("vendor/parent.tgz", &archive).unwrap();
    copied
        .project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    copied
        .project
        .write("node_modules/sentinel", b"running")
        .unwrap();
    let output = copied
        .command(&[
            "--json",
            "install",
            "--lockfile-only",
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
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["copied_source_preparations"], 1);
    assert_eq!(result["data"]["registry_artifact_requests"], 0);
    let bytes = fs::read(copied.project.path().join("tapid.lock")).unwrap();
    let lock = tapid_lockfile::Lockfile::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert!(lock.root_bindings()["parent"].starts_with("file:vendor/parent.tgz#sha256-"));
    assert!(lock.has_pending_receipts());
    let output = copied
        .command(&[
            "install",
            "--lockfile-only",
            "--dry-run",
            "--registry-fixture",
            "registry.json",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "copied previews retain their read-only refusal"
    );
    assert_eq!(
        fs::read(copied.project.path().join("tapid.lock")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read(copied.project.path().join("node_modules/sentinel")).unwrap(),
        b"running"
    );
    assert!(!copied.project.path().join("store").exists());

    let imported = CliFixture::new("selection-imported-command");
    let manifest = include_str!("../fixtures/npm-import/package.json");
    imported
        .project
        .write("package.json", manifest.as_bytes())
        .unwrap();
    imported
        .project
        .write(
            "package-lock.json",
            include_bytes!("../fixtures/npm-import/package-lock.json"),
        )
        .unwrap();
    let output = imported
        .command(&["import-package-lock", "package-lock.json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    imported
        .project
        .write("node_modules/sentinel", b"running")
        .unwrap();
    let bytes = fs::read(imported.project.path().join("tapid.lock")).unwrap();
    let output = imported
        .command(&["--json", "install", "--lockfile-only"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["imported"], true);
    assert_eq!(result["data"]["metadata_lookups"], 0);
    assert_eq!(result["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read(imported.project.path().join("tapid.lock")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read(imported.project.path().join("node_modules/sentinel")).unwrap(),
        b"running"
    );
}
