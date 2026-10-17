use super::*;

fn status(project: &Path, extra: &[&str]) -> (std::process::Output, serde_json::Value) {
    let mut args = vec!["status", "--json"];
    args.extend_from_slice(extra);
    let output = run(project, &args);
    assert!(
        output.stderr.is_empty(),
        "status JSON must leave stderr empty"
    );
    let value = serde_json::from_slice(&output.stdout).expect("status JSON result");
    (output, value)
}

#[test]
fn status_reports_missing_files_without_creating_anything() {
    let project = tapid_test_support::TempProject::new("status-missing").unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["operation"], "status");
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(value["data"]["manifest_lock_agreement"], "unknown");
    assert_eq!(value["data"]["installation"][0]["state"], "absent");
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 0);
}

#[test]
fn status_reports_pending_recovery_and_unmanaged_installation_without_recovering() {
    let project = tapid_test_support::TempProject::new("status-recovery").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    project
        .write(".tapid-lifecycle-journal.json", b"interrupted")
        .unwrap();
    project.write("node_modules/keep", b"keep").unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["recovery"], "pending");
    assert_eq!(value["data"]["manifest_lock_agreement"], "match");
    assert_eq!(
        value["data"]["installation"][0]["state"],
        "unmanaged_or_invalid"
    );
    assert_eq!(
        fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        b"interrupted"
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/keep")).unwrap(),
        b"keep"
    );
    assert!(
        value["data"]["checks_not_performed"]
            .as_array()
            .unwrap()
            .len()
            > 3
    );
}

#[test]
fn status_checks_all_workspace_manifests_and_never_reads_credentials() {
    let project = tapid_test_support::TempProject::new("status-workspace").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    project.write("packages/a/package.json", br#"{"name":"a","version":"1.0.0","dependencies":{"bad":"https://example.invalid/pkg.tgz"}}"#).unwrap();
    project
        .write(
            "packages/b/package.json",
            br#"{"name":"b","version":"1.0.0"}"#,
        )
        .unwrap();
    project.write("tapid.toml", b"[registries.default]\nurl = \"https://registry.example\"\ntoken-env = \"TAPID_STATUS_TEST_SECRET\"\n").unwrap();
    let (_, value) = status(project.path(), &["--workspace", "b"]);
    assert_eq!(value["data"]["workspace"]["selected"], "b");
    assert_eq!(value["data"]["workspace"]["linking"], "not_verified");
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "UNSUPPORTED_SPEC"
                && b["location"]
                    .as_str()
                    .unwrap()
                    .contains("packages/a/package.json#/dependencies/bad"))
    );
    assert_eq!(
        value["data"]["registries"]["credential_environment_names"][0],
        "TAPID_STATUS_TEST_SECRET"
    );
    assert_eq!(value["data"]["snapshot"]["consistency"], "not_atomic");
}

#[test]
fn status_reports_local_readiness_without_claiming_tree_or_runner_verification() {
    let project = tapid_test_support::TempProject::new("status-ready").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    project
        .write(".tapid-managed", b"tapid-managed-v1\n")
        .unwrap();
    project
        .write("node_modules/unverified", b"not verified")
        .unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(output.status.success());
    assert_eq!(value["data"]["assessment"], "no_local_blockers");
    assert_eq!(value["data"]["manifest_lock_agreement"], "match");
    assert_eq!(value["data"]["installation"][0]["state"], "managed");
    assert_eq!(value["data"]["capabilities"]["runner"], "not_assessed");
    assert_eq!(
        value["data"]["registries"]["default_origin"],
        "https://registry.npmjs.org"
    );
    assert_eq!(
        value["data"]["snapshot"]["files"][0]["changed_on_recheck"],
        false
    );
    let text = run(project.path(), &["status"]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("not atomic"));
}

#[cfg(unix)]
#[test]
fn status_rejects_symlink_metadata_without_following_it() {
    let project = tapid_test_support::TempProject::new("status-symlink").unwrap();
    project
        .write("private", b"secret data must not appear")
        .unwrap();
    std::os::unix::fs::symlink("private", project.path().join("package.json")).unwrap();
    std::os::unix::fs::symlink("private", project.path().join("tapid.lock")).unwrap();
    std::os::unix::fs::symlink(
        "private",
        project.path().join(".tapid-lifecycle-journal.json"),
    )
    .unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["recovery"], "unknown");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret data"));
    assert_eq!(
        fs::read(project.path().join("private")).unwrap(),
        b"secret data must not appear"
    );
}

#[test]
fn status_rejects_workspace_lock_edges_that_disagree_with_current_manifests() {
    let project = tapid_test_support::TempProject::new("status-workspace-edges").unwrap();
    let root = br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    let a = br#"{"name":"a","version":"1.0.0","dependencies":{"b":"1"}}"#;
    let b = br#"{"name":"b","version":"1.0.0"}"#;
    project.write("package.json", root).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(root))).unwrap();
    for (name, bytes) in [("a", a.as_slice()), ("b", b.as_slice())] {
        let path = format!("packages/{name}");
        project
            .write(format!("{path}/package.json"), bytes)
            .unwrap();
        let source = tapid_lockfile::LocalWorkspaceSource::new(&path, name, "1.0.0").unwrap();
        let package = tapid_lockfile::LockedWorkspacePackage::new(
            source,
            &format!("sha256-{:x}", Sha256::digest(bytes)),
        )
        .unwrap();
        lock.insert_workspace_package(package).unwrap();
    }
    lock.set_roots(
        lock.workspace_packages()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
    )
    .unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let (output, value) = status(project.path(), &["--workspace", "b"]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["manifest_lock_agreement"], "mismatch");
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "LOCK_WORKSPACE_MISMATCH")
    );
}

#[test]
fn status_human_output_sanitizes_untrusted_selection() {
    let project = tapid_test_support::TempProject::new("status-display").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let output = run(
        project.path(),
        &["status", "--workspace", "\u{1b}[31mforged\nline"],
    );
    assert!(!output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("forged\nline"));
}

#[test]
fn status_separates_matching_metadata_from_unsupported_lock_replay() {
    let project = tapid_test_support::TempProject::new("status-lock-replay").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    legacy["lockfileVersion"] = serde_json::json!(5);
    project
        .write("tapid.lock", legacy.to_string().as_bytes())
        .unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["manifest_lock_agreement"], "match");
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "LOCK_REPLAY_UNSUPPORTED")
    );
}

#[test]
fn status_bounds_blockers_and_marks_partial_inspection() {
    let project = tapid_test_support::TempProject::new("status-bounds").unwrap();
    let deps = (0..140)
        .map(|i| {
            (
                format!("package-{i}"),
                serde_json::json!("https://example.invalid/pkg.tgz"),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let manifest = serde_json::json!({"name": "app", "version": "1.0.0", "dependencies": deps});
    project
        .write("package.json", manifest.to_string().as_bytes())
        .unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["truncated"], true);
    assert_eq!(value["data"]["blockers"].as_array().unwrap().len(), 128);
    assert!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "INSPECTION_TRUNCATED")
    );
}
