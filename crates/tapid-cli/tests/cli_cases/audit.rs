use super::*;

#[test]
fn audit_without_provider_is_incomplete_and_does_not_recover_or_modify_state() {
    let project = tapid_test_support::TempProject::new("audit-unconfigured").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    let lock_bytes = lock.to_json().unwrap();
    project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
    project
        .write("node_modules/sentinel", b"installed")
        .unwrap();
    project
        .write(
            ".tapid-lifecycle-journal.json",
            b"pending recovery sentinel",
        )
        .unwrap();
    let home = tapid_test_support::TempHome::new("audit-home").unwrap();
    home.write("tapid/store/sentinel", b"store").unwrap();
    let output =
        command_support::command_in_home(project.path(), home.path(), &["audit", "--json"], true)
            .output()
            .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["operation"], "audit");
    assert_eq!(value["outcome"], "partial");
    assert_eq!(value["data"]["assessment"], "not_checked");
    assert_eq!(value["data"]["provider"], "unavailable");
    assert_eq!(value["data"]["live_requests"], 0);
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_bytes.as_bytes()
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/sentinel")).unwrap(),
        b"installed"
    );
    assert_eq!(
        fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        b"pending recovery sentinel"
    );
    assert_eq!(
        fs::read(home.path().join("tapid/store/sentinel")).unwrap(),
        b"store"
    );
}

#[test]
fn audit_rejects_malformed_and_mismatched_locks_without_repair() {
    for bytes in ["malformed", &prior_empty_lock()] {
        let project = tapid_test_support::TempProject::new("audit-invalid-lock").unwrap();
        project
            .write("package.json", br#"{"name":"audit","version":"1.0.0"}"#)
            .unwrap();
        project.write("tapid.lock", bytes.as_bytes()).unwrap();
        let output = run(project.path(), &["--json", "audit"]);
        assert_eq!(output.status.code(), Some(3));
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["outcome"], "failure");
        assert_eq!(value["data"]["assessment"], "not_checked");
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            bytes.as_bytes()
        );
    }
}

#[test]
fn audit_validates_imported_graph_without_migrating_it_or_claiming_clean_coverage() {
    let project = tapid_test_support::TempProject::new("audit-imported").unwrap();
    let manifest = r#"{"name":"audit","version":"1.0.0"}"#;
    let digest = format!("sha256-{:x}", Sha256::digest(manifest));
    let npm = r#"{"name":"audit","version":"1.0.0","lockfileVersion":3,"packages":{"":{"name":"audit","version":"1.0.0"}}}"#;
    let lock = tapid_lockfile::ImportedNpmLockfile::import(npm, manifest, &digest).unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("package.json", manifest.as_bytes()).unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    let output = run(project.path(), &["audit", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["lock_format"], "imported_npm");
    assert_eq!(value["data"]["coverage"]["complete"], false);
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
    let human = run(project.path(), &["audit"]);
    assert_eq!(human.status.code(), Some(3));
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .contains("not checked")
    );
}

#[test]
fn audit_distinguishes_reachable_native_subjects_from_orphan_records() {
    let project = tapid_test_support::TempProject::new("audit-reachable").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0","dependencies":{"root":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    let packages = ["root", "orphan"].map(|name| {
        tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org",
            name,
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "a".repeat(64)),
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap()
    });
    let root = packages[0].key();
    lock.insert_packages(packages).unwrap();
    lock.set_roots([root]).unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    let output = run(project.path(), &["audit", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["recorded_subjects"], 2);
    assert_eq!(value["data"]["reachable_subjects"], 1);
    assert_eq!(value["data"]["graph_coverage"]["roots_recorded"], true);
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
}
