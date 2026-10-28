use super::*;

#[test]
fn lifecycle_rebuild_baseline_install_and_update_have_no_exact_force_interface() {
    let fixture = command_support::CliFixture::new("rebuild-baseline-interface");
    fixture
        .project
        .write("package.json", b"invalid manifest")
        .unwrap();
    fixture
        .project
        .write("tapid.lock", b"unchanged lock")
        .unwrap();
    for args in [
        vec!["install", "--force"],
        vec!["update", "native-demo", "--force"],
        vec!["install", "--instance", "exact-locked-instance"],
        vec!["update", "--instance", "exact-locked-instance"],
    ] {
        let result = fixture.command(&args).output().unwrap();
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("unexpected argument"));
        assert_eq!(
            fs::read(fixture.project.path().join("package.json")).unwrap(),
            b"invalid manifest"
        );
        assert_eq!(
            fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
            b"unchanged lock"
        );
        assert!(!fixture.project.path().join("node_modules").exists());
    }
}

#[test]
fn lifecycle_rebuild_baseline_named_update_resolves_new_versions_within_requirements() {
    let fixture = command_support::CliFixture::new("rebuild-baseline-update");
    let manifest = br#"{"name":"demo","version":"1.0.0","dependencies":{"b":"^1.0.0"}}"#;
    fixture.project.write("package.json", manifest).unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/aliased-root-cycle.json")).unwrap();
    let mut package = registry["packages"][1].clone();
    package["dependencies"] = serde_json::json!({});
    registry["packages"] = serde_json::json!([package.clone()]);
    fixture
        .project
        .write("registry.json", &serde_json::to_vec(&registry).unwrap())
        .unwrap();
    let installed = fixture
        .command(&["install", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let before = Lockfile::from_json(
        &fs::read_to_string(fixture.project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        before.packages().values().next().unwrap().version(),
        "1.0.0"
    );
    package["version"] = "1.1.0".into();
    registry["packages"].as_array_mut().unwrap().push(package);
    fixture
        .project
        .write("registry.json", &serde_json::to_vec(&registry).unwrap())
        .unwrap();
    let updated = fixture
        .command(&["update", "b", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(updated.status.success(), "{updated:?}");
    let after = Lockfile::from_json(
        &fs::read_to_string(fixture.project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    assert_eq!(after.packages().values().next().unwrap().version(), "1.1.0");
    assert_eq!(
        fs::read(fixture.project.path().join("package.json")).unwrap(),
        manifest
    );
}

#[test]
fn lifecycle_reporting_distinguishes_unperformed_from_successful_empty_discovery() {
    let fixture = command_support::CliFixture::new("lifecycle-knowledge");
    fixture
        .project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"a":"1.0.0"}}"#,
        )
        .unwrap();
    fixture
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/aliased-root-cycle.json"),
        )
        .unwrap();
    let missing = fixture
        .command(&["install", "--offline", "--frozen", "--json"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let result: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(result["lifecycle"]["coverage"], "not_performed");
    assert_eq!(result["lifecycle"]["reason"], "before_discovery");
    assert_eq!(
        result["lifecycle"]["selected_packages"],
        serde_json::Value::Null
    );
    let installed = fixture
        .command(&["install", "--registry-fixture", "registry.json", "--json"])
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(result["lifecycle"]["coverage"], "complete");
    let packages = result["lifecycle"]["packages"].as_array().unwrap();
    assert!(!packages.is_empty());
    for package in packages {
        assert_eq!(package["discovery"], "succeeded");
        assert_eq!(package["hooks"], serde_json::json!([]));
    }
    assert_eq!(result["lifecycle"]["hooks_executed"], 0);
    assert_eq!(result["lifecycle"]["hooks_skipped"], 0);
}

#[test]
fn lifecycle_rebuild_requires_explicit_selection_before_project_access() {
    let fixture = command_support::CliFixture::new("rebuild-required-selection");
    fixture
        .project
        .write("package.json", b"invalid manifest")
        .unwrap();
    let result = fixture.command(&["rebuild"]).output().unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("rebuild requires at least one"),
        "{result:?}"
    );
    assert_eq!(
        fs::read(fixture.project.path().join("package.json")).unwrap(),
        b"invalid manifest"
    );
    assert!(!fixture.project.path().join("tapid.lock").exists());
}

#[test]
fn lifecycle_rebuild_rejects_imported_lock_without_conversion_or_activation() {
    let fixture = command_support::CliFixture::new("rebuild-imported-lock");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fixture
        .project
        .write("package.json", manifest.as_bytes())
        .unwrap();
    let npm = serde_json::json!({"lockfileVersion": 3, "packages": {
        "": {"name": "demo", "version": "1.0.0"}
    }});
    let lock = tapid_lockfile::ImportedNpmLockfile::import(
        &npm.to_string(),
        manifest,
        &format!("sha256-{:x}", Sha256::digest(manifest.as_bytes())),
    )
    .unwrap()
    .to_json()
    .unwrap();
    fixture
        .project
        .write("tapid.lock", lock.as_bytes())
        .unwrap();
    fixture
        .project
        .write("node_modules/KEEP", b"prior active layout")
        .unwrap();
    for flags in [vec![], vec!["--force"], vec!["--json"]] {
        let json = flags.contains(&"--json");
        let mut args = vec!["rebuild", "demo"];
        args.extend(flags);
        let result = fixture.command(&args).output().unwrap();
        assert!(!result.status.success(), "{result:?}");
        if json {
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(report["errors"][0]["code"], "INVALID_REQUEST");
            assert_eq!(report["lifecycle"]["hooks_executed"], 0);
            assert_eq!(report["changes"]["state"], "unchanged");
        } else {
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("does not support imported locks"),
                "{result:?}"
            );
        }
        assert_eq!(
            fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
            lock.as_bytes()
        );
        assert_eq!(
            fs::read(fixture.project.path().join("package.json")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(
            fs::read(fixture.project.path().join("node_modules/KEEP")).unwrap(),
            b"prior active layout"
        );
        assert!(
            !fixture
                .project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
    }
}

#[test]
fn lifecycle_rebuild_selects_exact_locked_instances_without_mutation_on_rejection() {
    let fixture = command_support::CliFixture::new("rebuild-exact-selection");
    let manifest =
        br#"{"name":"demo","version":"1.0.0","dependencies":{"a":"1.0.0","other":"npm:b@2.0.0"}}"#;
    fixture.project.write("package.json", manifest).unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/aliased-root-cycle.json")).unwrap();
    registry["packages"][1]["dependencies"] = serde_json::json!({});
    fixture
        .project
        .write("registry.json", &serde_json::to_vec(&registry).unwrap())
        .unwrap();
    let installed = fixture
        .command(&["install", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let lock_bytes = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
    let lock = Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
    let keys = lock
        .packages()
        .iter()
        .filter(|(_, package)| package.name() == "b")
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 2);
    for (args, diagnostic) in [
        (vec!["rebuild", "b"], "must identify one locked instance"),
        (vec!["rebuild", "missing"], "found 0"),
        (
            vec!["rebuild", "--instance", "not-a-key"],
            "instance is not locked",
        ),
        (
            vec!["rebuild", "--instance", keys[0]],
            "has no supported hooks",
        ),
        (
            vec!["rebuild", "--instance", keys[1], "--force"],
            "has no supported hooks",
        ),
    ] {
        let result = fixture.command(&args).output().unwrap();
        assert!(!result.status.success(), "{args:?}: {result:?}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(diagnostic), "{args:?}: {result:?}");
        if args == ["rebuild", "b"] {
            for key in &keys {
                assert!(stderr.contains(key), "{stderr}");
            }
        }
        assert_eq!(
            fs::read(fixture.project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
        assert!(fixture.project.path().join("node_modules/a").exists());
    }
}

#[test]
fn lifecycle_rebuild_rejects_hookless_selection_with_explicit_plan_in_json() {
    let fixture = command_support::CliFixture::new("rebuild-hookless-json");
    fixture
        .project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"b":"2.0.0"}}"#,
        )
        .unwrap();
    fixture
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/aliased-root-cycle.json"),
        )
        .unwrap();
    let installed = fixture
        .command(&["install", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let result = fixture
        .command(&["rebuild", "b", "--json"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["lifecycle"]["explicitly_selected_packages"], 1);
    assert_eq!(report["lifecycle"]["impacted_packages"], 1);
    assert_eq!(report["lifecycle"]["hooks_executed"], 0);
}

#[test]
fn lifecycle_rebuild_never_turns_default_denial_into_success_or_accepts_unsafe_modes() {
    let fixture = command_support::CliFixture::new("rebuild-denied");
    let manifest = br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0"}}"#;
    fixture.project.write("package.json", manifest).unwrap();
    write_dependency_script_fixture(
        &fixture.project,
        "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=",
    );
    let installed = fixture
        .command(&["install", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let lock = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
    for flags in [vec!["--json"], vec!["--force", "--json"]] {
        let mut args = vec!["rebuild", "native-demo"];
        args.extend(flags);
        let result = fixture.command(&args).output().unwrap();
        assert!(!result.status.success(), "{result:?}");
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["lifecycle"]["hooks_executed"], 0);
        assert_eq!(
            report["lifecycle"]["packages"][0]["hooks"][0]["authorization"],
            "not_approved"
        );
        assert_eq!(
            fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
            lock
        );
        assert_eq!(
            fs::read(fixture.project.path().join("package.json")).unwrap(),
            manifest
        );
        assert!(
            !fixture
                .project
                .path()
                .join("node_modules/native-demo/SHOULD_NOT_EXIST")
                .exists()
        );
    }
    for flag in [
        "--offline",
        "--frozen",
        "--allow-unapproved-dependency-scripts",
        "--unsafe-no-dependency-sandbox",
    ] {
        let result = fixture
            .command(&["rebuild", "native-demo", flag])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert_eq!(
            fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
            lock
        );
    }
}

#[test]
fn lifecycle_rebuild_impact_excludes_dependants_without_readable_generated_inputs() {
    let fixture = command_support::CliFixture::new("rebuild-readable-impact");
    fixture.project.write("package.json", br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0","consumer":"1.0.0"}}"#).unwrap();
    write_dependency_script_fixture(&fixture.project, "H4sIAAAAAAAC/+3QwUoDMRSF4TxKyFrHDFMquHCl0ILYRSu4K2F60dF2EpK0CKXvbqaCC1cWRBD/b3PCyb0EElz76p7kInxk9ZJ8r36YLcaj0TGLr2mby+bzfOzretzUSlv1C7Ypu1ieV//T3vRuI+ZKl8zdTs5XsvHmTJudxNT5fripK1vZoUtt7EJOpdubEKXry9+t18OItM9ey5u02ywrfa3nk9nD3c3yfrZY3j5O54th+8Tx4FM+dSVKcFG+NX44KAAAAAAAAAAAAAAAAAD4y94B7nP/yQAoAAA=");
    let mut registry: serde_json::Value = serde_json::from_slice(&fs::read(fixture.project.path().join("registry.json")).unwrap()).unwrap();
    let mut consumer = registry["packages"][0].clone();
    consumer["name"] = "consumer".into();
    consumer["dependencies"] = serde_json::json!({"native-demo":"1.0.0"});
    registry["packages"].as_array_mut().unwrap().push(consumer);
    fixture.project.write("registry.json", &serde_json::to_vec(&registry).unwrap()).unwrap();
    let installed = fixture.command(&["install", "--registry-fixture", "registry.json"]).output().unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let result = fixture.command(&["rebuild", "native-demo", "--json"]).output().unwrap();
    assert!(!result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["lifecycle"]["explicitly_selected_packages"], 1);
    assert_eq!(report["lifecycle"]["impacted_packages"], 1);
    assert_eq!(report["lifecycle"]["hooks_executed"], 0);
}

#[cfg(target_os = "linux")]
#[test]
fn lifecycle_rebuild_forces_only_selected_and_authenticates_outside_impact_before_hooks() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    for changed_output in [false, true] {
        let fixture = command_support::CliFixture::new("rebuild-impact");
        let manifest = br#"{"name":"demo","version":"1.0.0","dependencies":{"native-demo":"1.0.0","dependant":"1.0.0","unrelated":"1.0.0"}}"#;
        fixture.project.write("package.json", manifest).unwrap();
        let archive = if changed_output {
            "H4sIAAAAAAAAE+2UXWvCMBSGvfZXhAxsC5qm6jrQMRjTgXdj1jtBsvSo2dqkS9JOEf/78AMvZDAGTr3oc/OGcxLOB7zJGP9gM/CznZJ3o2TlxFBKadhub5VSeqy0FTYP5208CMLmXQXRUzfyE7mxTFfoWWpdISssWQq4g7BkVhTQiCFVuI5wAdoIJTeZgFBCNzHDtciswR20wkIay5Jk+1LFgBqAxlhM3UwrDsYQkAWJHl8GvUnUH0aTYf/ptR95h+xCWLfd9LoaPnOhwXWmxvHIlxYWnkUCw6XkrjMDCZpZiIldWKd+uMv1MrPK8YhmMlbpaDTouZ43xqhWQ35utP8mpD/jHM0hSRThqKHQbjy8Xl9649fF3vf+flP/UuM3/wf09sj/NKSt0v/n4EZInuQxoHtjY6HI/KEqpEUpE9ItlIi9VZZb4+5/B8RVmokEYrxxrs21RLS7rl56iJKSkpKSP/MNYUDXEwAMAAA="
        } else {
            NATIVE_BUILD_ARCHIVE
        };
        write_dependency_script_fixture(&fixture.project, archive);
        let mut registry: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.project.path().join("registry.json")).unwrap(),
        )
        .unwrap();
        for name in ["dependant", "unrelated"] {
            let mut entry = registry["packages"][0].clone();
            entry["name"] = name.into();
            if name == "dependant" {
                entry["dependencies"] = serde_json::json!({"native-demo":"1.0.0"});
            }
            registry["packages"].as_array_mut().unwrap().push(entry);
        }
        fixture
            .project
            .write("registry.json", &serde_json::to_vec(&registry).unwrap())
            .unwrap();
        let sh = fs::canonicalize("/bin/sh").unwrap();
        let node = fs::canonicalize("/usr/bin/node").unwrap();
        let script = r#"node -e "if(process.env.TAPID_TEST_SECRET)process.exit(42);require('fs').writeFileSync('generated.txt','generated')" && /usr/bin/gcc hello.c -o native"#;
        let script = if changed_output {
            script.replace(
                "writeFileSync('generated.txt','generated')",
                "writeFileSync('generated.txt',require('crypto').randomUUID())",
            )
        } else {
            script.to_owned()
        };
        let approvals = ["native-demo", "dependant", "unrelated"].map(|name| serde_json::json!({
        "package":name,"version":"1.0.0","archive-digest":registry["packages"][0]["integrity"],
        "hook":"install","script-digest":format!("sha256-{:x}", Sha256::digest(&script)),
        "system-toolchain":true,"process-memory-stats":true,"dependencies":name == "dependant",
        "read":["."],"write":["."],"network":false,"environment":{"TMPDIR":"."},
        "timeout-seconds":20,"max-output-bytes":8192,"max-processes":64,"max-memory-bytes":536870912,
        "tools":[{"name":"sh","path":sh,"digest":format!("sha256-{:x}",Sha256::digest(fs::read(&sh).unwrap()))},
                 {"name":"node","path":node,"digest":format!("sha256-{:x}",Sha256::digest(fs::read(&node).unwrap()))}]
    }));
        fixture
            .project
            .write(
                "tapid.lifecycle.toml",
                toml::to_string(&serde_json::json!({"schema":1,"approvals":approvals}))
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        let store = fixture.project.path().join("store");
        let mut install = fixture.command(&[
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ]);
        let installed = install.arg(&store).output().unwrap();
        assert!(installed.status.success(), "{installed:?}");
        let lock_bytes = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
        let before = Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
        let rebuild = || {
            let mut command =
                fixture.command(&["rebuild", "native-demo", "--force", "--json", "--store-dir"]);
            command.arg(&store);
            command
        };
        let result = rebuild().output().unwrap();
        assert!(result.status.success(), "{result:?}");
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["lifecycle"]["explicitly_selected_packages"], 1);
        assert_eq!(report["lifecycle"]["impacted_packages"], 2);
        assert_eq!(
            report["lifecycle"]["hooks_executed"],
            if changed_output { 2 } else { 1 }
        );
        assert_eq!(
            report["lifecycle"]["generated_outputs_reused"],
            if changed_output { 1 } else { 2 }
        );
        let details = report["lifecycle"]["packages"].as_array().unwrap();
        for (name, role, disposition) in [
            ("native-demo", "selected", "executed"),
            (
                "dependant",
                "dependant",
                if changed_output { "executed" } else { "reused" },
            ),
            ("unrelated", "prerequisite", "reused"),
        ] {
            let entry = details
                .iter()
                .find(|p| p["identity"]["name"] == name)
                .unwrap();
            assert_eq!(entry["rebuild_role"], role);
            assert_eq!(entry["hooks"][0]["disposition"], disposition);
        }
        let rebuilt_lock = fs::read(fixture.project.path().join("tapid.lock")).unwrap();
        let rebuilt = Lockfile::from_json(std::str::from_utf8(&rebuilt_lock).unwrap()).unwrap();
        assert_eq!(rebuilt.source_graph(), before.source_graph());
        for (key, package) in before.packages() {
            if changed_output && package.name() != "unrelated" {
                assert_ne!(
                    rebuilt.packages()[key].derived_hooks(),
                    package.derived_hooks()
                );
            } else {
                assert_eq!(
                    rebuilt.packages()[key].derived_hooks(),
                    package.derived_hooks()
                );
            }
        }
        let active_outputs = ["native-demo", "dependant", "unrelated"].map(|name| {
            fs::read(
                fixture
                    .project
                    .path()
                    .join(format!("node_modules/{name}/generated.txt")),
            )
            .unwrap()
        });
        assert_eq!(
            fs::read(fixture.project.path().join("package.json")).unwrap(),
            manifest
        );
        // Removing authentication cannot silently expand the execution set to the
        // unrelated hook. All prerequisites must authenticate before selected hooks.
        fs::remove_file(store.join(".tapid-lifecycle-key")).unwrap();
        let result = rebuild().output().unwrap();
        assert!(!result.status.success(), "{result:?}");
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["lifecycle"]["hooks_executed"], 0);
        assert_eq!(
            fs::read(fixture.project.path().join("tapid.lock")).unwrap(),
            rebuilt_lock
        );
        let after = Lockfile::from_json(
            &fs::read_to_string(fixture.project.path().join("tapid.lock")).unwrap(),
        )
        .unwrap();
        assert_eq!(after.source_graph(), before.source_graph());
        for (name, output) in ["native-demo", "dependant", "unrelated"]
            .into_iter()
            .zip(active_outputs)
        {
            assert_eq!(
                fs::read(
                    fixture
                        .project
                        .path()
                        .join(format!("node_modules/{name}/generated.txt"))
                )
                .unwrap(),
                output
            );
        }
    }
}
