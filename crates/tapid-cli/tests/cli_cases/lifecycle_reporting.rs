use super::*;

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
