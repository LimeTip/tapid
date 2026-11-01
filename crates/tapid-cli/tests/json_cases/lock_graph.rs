use super::*;

#[test]
fn explicit_graph_snapshots_are_inspected_compared_and_paged_without_project_state() {
    let project = TempProject::new("json-lock-graph").unwrap();
    let mut lock = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    let package = tapid_lockfile::LockedPackage::new_with_provenance(
        "https://registry.example.test",
        "actual",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "b".repeat(64)),
        tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    lock.insert_package(package.clone()).unwrap();
    lock.set_roots([package.key()]).unwrap();
    let original = lock.to_json().unwrap();
    project.write("before.lock", original.as_bytes()).unwrap();
    lock.set_root_bindings([("compat".into(), package.key())].into())
        .unwrap();
    project
        .write("after.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    project
        .write("tapid.toml", b"this configuration must never be parsed")
        .unwrap();
    let (output, first) = invoke(
        &project,
        &["--json", "lock", "inspect", "before.lock", "--limit", "1"],
    );
    assert!(output.status.success(), "{first}");
    assert_eq!(first["data"]["contract_version"], 1);
    assert_eq!(first["data"]["coverage"]["analysis_complete"], true);
    assert_eq!(first["data"]["records"].as_array().unwrap().len(), 1);
    let cursor = first["data"]["continuation"].as_str().unwrap();
    let (output, next) = invoke(
        &project,
        &[
            "--json",
            "lock",
            "inspect",
            "before.lock",
            "--limit",
            "1",
            "--cursor",
            cursor,
        ],
    );
    assert!(output.status.success(), "{next}");
    assert_ne!(first["data"]["records"], next["data"]["records"]);
    let (output, stale) = invoke(
        &project,
        &[
            "--json",
            "lock",
            "inspect",
            "after.lock",
            "--cursor",
            cursor,
        ],
    );
    assert!(!output.status.success());
    assert_eq!(stale["errors"][0]["code"], "LOCK_GRAPH_INVALID_CURSOR");
    let (output, comparison) = invoke(
        &project,
        &["--json", "lock", "compare", "before.lock", "after.lock"],
    );
    assert!(output.status.success(), "{comparison}");
    assert_eq!(comparison["data"]["summary"]["changed_selections"], 0);
    assert_eq!(comparison["data"]["summary"]["added_instances"], 0);
    assert_eq!(comparison["data"]["selected_graph_supported"], true);
    assert_eq!(
        fs::read(project.path().join("before.lock")).unwrap(),
        original.as_bytes()
    );
}

#[test]
fn graph_failures_preserve_pending_recovery_and_forbidden_store() {
    let project = TempProject::new("json-lock-read-only").unwrap();
    let home = TempHome::new("json-lock-read-only").unwrap();
    let mut files = vec![
        (
            "package.json",
            b"invalid manifest must not be loaded".to_vec(),
        ),
        (
            "tapid.toml",
            b"invalid configuration must not be loaded".to_vec(),
        ),
        (
            ".tapid-lifecycle-journal.json",
            b"pending recovery sentinel".to_vec(),
        ),
        ("store", b"poisoned store is a file".to_vec()),
        ("broken.lock", b"{bad".to_vec()),
        ("unsupported.lock", b"{\"lockfileVersion\":99}".to_vec()),
    ];
    let native = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    files.push(("valid.lock", native.to_json().unwrap().into_bytes()));
    for (path, bytes) in &files {
        project.write(path, bytes).unwrap();
    }
    for (file, code) in [
        ("broken.lock", "LOCK_GRAPH_INVALID_SNAPSHOT"),
        ("unsupported.lock", "LOCK_GRAPH_UNSUPPORTED_FORMAT"),
    ] {
        let output = command(&project, &home, &["--json", "lock", "inspect", file])
            .output()
            .unwrap();
        assert!(output.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], code);
    }
    let output = command(
        &project,
        &home,
        &["--json", "lock", "inspect", "valid.lock"],
    )
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    for (path, bytes) in files {
        assert_eq!(fs::read(project.path().join(path)).unwrap(), bytes);
    }
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn graph_read_limits_and_json_requirement_are_enforced() {
    let project = TempProject::new("json-lock-limits").unwrap();
    project.write("oversized.lock", b"").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(project.path().join("oversized.lock"))
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    let (output, result) = invoke(&project, &["--json", "lock", "inspect", "oversized.lock"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "LOCK_GRAPH_INPUT_LIMIT");
    let home = TempHome::new("json-lock-limits").unwrap();
    let output = command(&project, &home, &["lock", "inspect", "missing.lock"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("require --json"));
    assert!(output.stdout.is_empty());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("oversized.lock", project.path().join("link.lock")).unwrap();
        let (output, result) = invoke(&project, &["--json", "lock", "inspect", "link.lock"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], "LOCK_GRAPH_IO");
    }
}

#[test]
fn graph_json_separates_imported_receipts_and_rejects_cross_format_equivalence() {
    let project = TempProject::new("json-lock-imported").unwrap();
    let mut lock = tapid_lockfile::ImportedNpmLockfile::import(
        include_str!("../../../tapid-lockfile/tests/fixtures/npm-import-package-lock.json"),
        include_str!("../../../tapid-lockfile/tests/fixtures/npm-import-package.json"),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    project
        .write("before.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let graph = lock.graph().unwrap();
    let package = &graph.packages["node_modules/shared"];
    let digest = format!("sha256-{}", "b".repeat(64));
    lock.record_verified_trees([("node_modules/shared".into(), digest.clone())].into())
        .unwrap();
    lock.record_verified_artifacts(
        [(
            "node_modules/shared".into(),
            tapid_lockfile::ImportedNpmArtifactReceipt {
                tree_digest: digest,
                resolved: package.resolved.clone(),
                integrity: package.integrity.to_string(),
            },
        )]
        .into(),
    )
    .unwrap();
    project
        .write("after.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &["--json", "lock", "compare", "before.lock", "after.lock"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["summary"]["changed_selections"], 0);
    assert_eq!(result["data"]["summary"]["added_instances"], 0);
    assert_eq!(
        result["data"]["records"][0]["value"]["dimension"],
        "verification_receipts"
    );
    let native = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "c".repeat(64))).unwrap();
    project
        .write("native.lock", native.to_json().unwrap().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &["--json", "lock", "compare", "before.lock", "native.lock"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["data"]["selected_graph_supported"], false);
    assert!(result["data"]["summary"].is_null());
    assert_eq!(
        result["errors"][0]["code"],
        "LOCK_GRAPH_UNSUPPORTED_COMPARISON"
    );
}

#[test]
fn graph_display_shortening_preserves_references_and_bounds_the_whole_response() {
    let project = TempProject::new("json-lock-output-bound").unwrap();
    let mut lock = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    for index in 0..300 {
        let source = format!(
            "file:vendor/{}-{index}.tgz#sha256-{}",
            "a".repeat(5000),
            "b".repeat(64)
        );
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            &source,
            "actual",
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "c".repeat(64)),
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        lock.insert_package(package).unwrap();
    }
    lock.set_roots([lock.packages().keys().next().unwrap().clone()])
        .unwrap();
    project
        .write("selected.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "--json",
            "lock",
            "inspect",
            "selected.lock",
            "--limit",
            "1000",
        ],
    );
    assert!(output.status.success(), "{result}");
    assert!(output.stdout.len() <= 1024 * 1024);
    assert_eq!(result["data"]["summary"]["instances"], 300);
    assert_eq!(result["data"]["coverage"]["analysis_complete"], true);
    assert!(!result["truncated_fields"].as_array().unwrap().is_empty());
    assert!(result["data"]["continuation"].is_string());
    let records = result["data"]["records"].as_array().unwrap();
    let refs = records
        .iter()
        .map(|r| r["value"]["reference"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(refs.len(), records.len());
    assert_eq!(
        records[0]["value"]["identity"],
        records[1]["value"]["identity"]
    );
}

#[test]
fn graph_filter_metadata_is_redacted_and_scope_is_bound_to_cursor() {
    let project = TempProject::new("json-lock-scope").unwrap();
    let lock = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    project
        .write("selected.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "--json",
            "lock",
            "inspect",
            "selected.lock",
            "--package",
            "https://user:secret@example.test/p?token=hidden\u{1b}",
        ],
    );
    assert!(output.status.success());
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(!rendered.contains("secret"));
    assert!(!rendered.contains("hidden"));
    assert!(!rendered.contains("\\u001b"));
    assert_eq!(result["data"]["scope"]["kind"], "packages");
    assert_eq!(result["data"]["summary"]["instances"], 0);
    let (output, result) = invoke(
        &project,
        &[
            "--json",
            "lock",
            "inspect",
            "selected.lock",
            "--cursor",
            "v1.1.bad",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(result["errors"][0]["code"], "LOCK_GRAPH_INVALID_CURSOR");
}
