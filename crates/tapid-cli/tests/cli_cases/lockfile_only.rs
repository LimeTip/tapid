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
