use super::*;

#[test]
fn audit_result_file_uses_the_json_protocol_and_preserves_human_output() {
    let project = tapid_test_support::TempProject::new("audit-result-project").unwrap();
    let results = tapid_test_support::TempProject::new("audit-result-owner").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    let path = results.path().join("result.json");
    let output = run(
        project.path(),
        &["audit", "--result-file", path.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Audit: not checked"));
    let result = fs::read(&path).unwrap();
    assert!(result.ends_with(b"\n"));
    let value: serde_json::Value = serde_json::from_slice(&result).unwrap();
    let json = run(project.path(), &["audit", "--json"]);
    assert_eq!(
        value,
        serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap()
    );
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["operation"], "audit");
    assert_eq!(value["data"]["assessment"], "not_checked");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let existing = run(
        project.path(),
        &["audit", "--result-file", path.to_str().unwrap()],
    );
    assert_eq!(existing.status.code(), Some(3));
    assert!(existing.stdout.is_empty());
    assert_eq!(fs::read(&path).unwrap(), result);

    let inside = project.path().join("audit-result.json");
    let denied = run(
        project.path(),
        &["audit", "--result-file", inside.to_str().unwrap()],
    );
    assert_eq!(denied.status.code(), Some(3));
    assert!(!inside.exists());
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
}

#[test]
fn audit_result_file_records_input_failures_and_rejects_conflicting_channels() {
    let project = tapid_test_support::TempProject::new("audit-result-failure").unwrap();
    let results = tapid_test_support::TempProject::new("audit-result-failure-owner").unwrap();
    let path = results.path().join("failure.json");
    let output = run(
        project.path(),
        &["audit", "--result-file", path.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["outcome"], "failure");
    assert_eq!(value["errors"][0]["code"], "AUDIT_MANIFEST_UNREADABLE");
    let conflict_path = results.path().join("conflict.json");
    let conflict = run(
        project.path(),
        &[
            "audit",
            "--json",
            "--result-file",
            conflict_path.to_str().unwrap(),
        ],
    );
    assert_eq!(conflict.status.code(), Some(2));
    assert!(!conflict_path.exists());
    #[cfg(unix)]
    {
        let link = results.path().join("linked.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let denied = run(
            project.path(),
            &["audit", "--result-file", link.to_str().unwrap()],
        );
        assert_eq!(denied.status.code(), Some(3));
        assert!(denied.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(),
            value
        );
    }
}

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
    assert_eq!(value["data"]["reachable_subjects"], 0);
    assert_eq!(
        value["data"]["graph_coverage"]["requestable_occurrences"],
        0
    );
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
        value["data"]["graph_coverage"]["requestable_occurrences"],
        1
    );
    assert_eq!(value["data"]["graph_coverage"]["excluded_occurrences"], 0);
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
}

#[test]
fn audit_thresholds_never_turn_unchecked_coverage_into_success() {
    let project = tapid_test_support::TempProject::new("audit-threshold").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    for threshold in ["any", "low", "moderate", "high", "critical"] {
        let output = run(
            project.path(),
            &["audit", "--threshold", threshold, "--json"],
        );
        assert_eq!(
            output.status.code(),
            Some(3),
            "{threshold}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["data"]["assessment"], "not_checked");
        assert_eq!(value["data"]["live_requests"], 0);
        assert_eq!(value["data"]["threshold"], threshold);
    }
    let results = tapid_test_support::TempProject::new("audit-threshold-results").unwrap();
    let path = results.path().join("critical.json");
    let output = run(
        project.path(),
        &[
            "audit",
            "--threshold",
            "critical",
            "--result-file",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    let file: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(file["data"]["threshold"], "critical");
    let output = run(project.path(), &["audit", "--threshold", "none"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
}

#[test]
fn audit_explicit_provider_inputs_fail_closed_without_project_mutation() {
    let project = tapid_test_support::TempProject::new("audit-explicit-provider").unwrap();
    let inputs = tapid_test_support::TempProject::new("audit-provider-inputs").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest)))
        .unwrap()
        .to_json()
        .unwrap();
    project.write("tapid.lock", lock.as_bytes()).unwrap();
    inputs
        .write(
            "provider.json",
            br#"{"version":1,"origin":"https://cloud.example","cache":"disabled"}"#,
        )
        .unwrap();
    inputs
        .write(
            "consent.json",
            br#"{"version":1,"origin":"https://other.example","requests":[]}"#,
        )
        .unwrap();
    inputs.write("trust.json", b"{}").unwrap();
    let config = inputs.path().join("provider.json");
    let consent = inputs.path().join("consent.json");
    let trust = inputs.path().join("trust.json");
    let state = inputs.path().join("replay");
    let output = run(
        project.path(),
        &[
            "audit",
            "--json",
            "--provider-config",
            config.to_str().unwrap(),
            "--inventory-consent",
            consent.to_str().unwrap(),
            "--provider-trust",
            trust.to_str().unwrap(),
            "--replay-state",
            state.to_str().unwrap(),
            "--credential-mode",
            "headless",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "AUDIT_CONSENT_INVALID");
    assert!(!state.exists());
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock.as_bytes()
    );
    let missing = run(
        project.path(),
        &["audit", "--provider-config", config.to_str().unwrap()],
    );
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn audit_configured_empty_consent_never_contacts_provider_and_rejects_project_inputs() {
    use base64::Engine;
    let project = tapid_test_support::TempProject::new("audit-configured-denial").unwrap();
    let inputs = tapid_test_support::TempProject::new("audit-configured-owner").unwrap();
    let manifest = br#"{"name":"audit","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    let package = tapid_lockfile::LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "dependency",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let key = package.key();
    lock.insert_package(package).unwrap();
    lock.set_roots([key]).unwrap();
    let bytes = lock.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    let config = br#"{"version":1,"origin":"https://cloud.invalid","headlessCredentialEnv":"TAPID_FIXTURE_MISSING_TOKEN","cache":"disabled"}"#;
    inputs.write("provider.json", config).unwrap();
    inputs
        .write(
            "consent.json",
            br#"{"version":1,"origin":"https://cloud.invalid","requests":[]}"#,
        )
        .unwrap();
    let public =
        hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
    let trust = serde_json::json!({"version":1,"issuer":"fixture.example","validUntil":4102444800u64,"maxAge":300,"keyring":{"version":"tapid-release-keyring-v1","keys":[{"key_id":"fixture","algorithm":"ed25519","public_key":base64::engine::general_purpose::STANDARD.encode(&public),"fingerprint":format!("sha256-{:x}",Sha256::digest(&public))}]}});
    inputs
        .write("trust.json", &serde_json::to_vec(&trust).unwrap())
        .unwrap();
    let config_path = inputs.path().join("provider.json");
    let consent = inputs.path().join("consent.json");
    let trust_path = inputs.path().join("trust.json");
    let state = inputs.path().join("replay");
    let invoke = |config_path: &std::path::Path| {
        run(
            project.path(),
            &[
                "audit",
                "--json",
                "--provider-config",
                config_path.to_str().unwrap(),
                "--inventory-consent",
                consent.to_str().unwrap(),
                "--provider-trust",
                trust_path.to_str().unwrap(),
                "--replay-state",
                state.to_str().unwrap(),
                "--credential-mode",
                "headless",
            ],
        )
    };
    let output = invoke(&config_path);
    assert_eq!(output.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    #[cfg(unix)]
    {
        assert!(value["errors"].as_array().unwrap().is_empty(), "{value}");
        assert_eq!(value["data"]["failures"][0]["code"], "not_authorized");
        assert!(state.is_dir());
    }
    #[cfg(not(unix))]
    assert_eq!(
        value["errors"][0]["code"],
        "AUDIT_REPLAY_PLATFORM_UNSUPPORTED"
    );
    let inside = project.write("provider.json", config).unwrap();
    let output = invoke(&inside);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["errors"][0]["code"],
        "AUDIT_PROVIDER_INPUT_IN_PROJECT"
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
}
