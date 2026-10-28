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
