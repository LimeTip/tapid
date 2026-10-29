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
fn status_reports_member_file_access_failures_at_the_member_path() {
    for directory in [false, true] {
        let project = tapid_test_support::TempProject::new("status-member-access").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"root","version":"1.0.0","workspaces":["member"]}"#,
            )
            .unwrap();
        let member = project.write("member/package.json", &[0xff]).unwrap();
        if directory {
            fs::remove_file(&member).unwrap();
            fs::create_dir(&member).unwrap();
        }
        let (output, value) = status(project.path(), &[]);
        assert!(!output.status.success(), "{value}");
        assert!(
            value["data"]["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| {
                    b["code"] == "WORKSPACE_INVALID"
                        && b["location"] == member.canonicalize().unwrap().display().to_string()
                }),
            "{value}"
        );
    }
}

#[test]
fn status_reports_the_malformed_workspace_member_path() {
    let project = tapid_test_support::TempProject::new("status-invalid-member").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    let member = project
        .write("packages/a/package.json", b"malformed JSON")
        .unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success(), "{value}");
    let blocker = value["data"]["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|blocker| blocker["code"] == "WORKSPACE_INVALID")
        .unwrap();
    assert_eq!(
        blocker["location"],
        member.canonicalize().unwrap().display().to_string(),
        "{value}"
    );
}

#[test]
fn status_ignores_member_overrides_but_checks_root_overrides_and_member_specs() {
    for (root_override, invalid_spec) in [(false, false), (true, false), (false, true)] {
        let project = tapid_test_support::TempProject::new("status-member-overrides").unwrap();
        let overrides = serde_json::json!({"jsr:@std/path":"1"});
        let mut root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["member"]});
        if root_override {
            root["overrides"] = overrides.clone();
        }
        let mut member =
            serde_json::json!({"name":"member","version":"1.0.0","overrides":overrides});
        if invalid_spec {
            member["dependencies"] = serde_json::json!({"bad":"https://example.invalid/pkg.tgz"});
        }
        let root = root.to_string();
        let member = member.to_string();
        project.write("package.json", root.as_bytes()).unwrap();
        project
            .write("member/package.json", member.as_bytes())
            .unwrap();
        let digest = |text: &str| format!("sha256-{:x}", Sha256::digest(text.as_bytes()));
        let mut lock = Lockfile::new(&digest(&root)).unwrap();
        lock.insert_workspace_package(
            tapid_lockfile::LockedWorkspacePackage::new(
                tapid_lockfile::LocalWorkspaceSource::new("member", "member", "1.0.0").unwrap(),
                &digest(&member),
            )
            .unwrap(),
        )
        .unwrap();
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
        let (output, value) = status(project.path(), &[]);
        let blockers = value["data"]["blockers"].as_array().unwrap();
        assert_eq!(
            output.status.success(),
            !root_override && !invalid_spec,
            "{value}"
        );
        assert_eq!(
            blockers.iter().any(|b| b["code"] == "UNSUPPORTED_OVERRIDE"),
            root_override,
            "{value}"
        );
        assert_eq!(
            blockers.iter().any(|b| b["code"] == "UNSUPPORTED_SPEC"),
            invalid_spec,
            "{value}"
        );
    }
}

#[test]
fn status_treats_root_peers_as_declarations_but_validates_member_peers() {
    for member in [false, true] {
        let project = tapid_test_support::TempProject::new("status-peer-declarations").unwrap();
        let peer = serde_json::json!({"name":"app","version":"1.0.0","peerDependencies":{"react":"workspace:*"}}).to_string();
        let root = if member {
            project
                .write("member/package.json", peer.as_bytes())
                .unwrap();
            r#"{"name":"root","version":"1.0.0","workspaces":["member"]}"#.to_owned()
        } else {
            peer
        };
        project.write("package.json", root.as_bytes()).unwrap();
        let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(root.as_bytes()))).unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        let (output, value) = status(project.path(), &[]);
        assert_eq!(output.status.success(), !member, "{value}");
        let unsupported = value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| {
                b["code"] == "UNSUPPORTED_SPEC"
                    && b["location"]
                        .as_str()
                        .unwrap()
                        .contains("#/peerDependencies/react")
            });
        assert_eq!(unsupported, member, "{value}");
    }
}

#[test]
fn status_points_recovery_blockers_at_backup_only_journals() {
    for invalid in [false, true] {
        let project = tapid_test_support::TempProject::new("status-backup-journal").unwrap();
        let backup = project
            .path()
            .join(".tapid-lifecycle-journal.lifecycle.bak");
        if invalid {
            fs::create_dir(&backup).unwrap();
        } else {
            fs::write(&backup, b"interrupted").unwrap();
        }
        let (_, value) = status(project.path(), &[]);
        let code = if invalid {
            "RECOVERY_STATE_INVALID"
        } else {
            "RECOVERY_REQUIRED"
        };
        let blocker = value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["code"] == code)
            .unwrap();
        assert_eq!(
            blocker["location"],
            backup.canonicalize().unwrap().display().to_string()
        );
        assert!(
            !project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists()
        );
        assert!(fs::symlink_metadata(&backup).is_ok());
    }
}

#[test]
fn status_checks_imported_npm_manifest_and_workspace_receipts() {
    for change in ["none", "root", "member", "missing_receipt", "invalid"] {
        let project = tapid_test_support::TempProject::new("status-imported-lock").unwrap();
        let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]})
            .to_string();
        let member = r#"{"name":"member","version":"1.0.0"}"#;
        project.write("package.json", root.as_bytes()).unwrap();
        project
            .write("packages/member/package.json", member.as_bytes())
            .unwrap();
        let npm = serde_json::json!({"lockfileVersion":3,"packages":{
            "":serde_json::from_str::<serde_json::Value>(&root).unwrap(),
            "packages/member":{"name":"member","version":"1.0.0"},
            "node_modules/member":{"resolved":"packages/member","link":true}
        }});
        let mut lock = tapid_lockfile::ImportedNpmLockfile::import(
            &npm.to_string(),
            &root,
            &format!("sha256-{:x}", Sha256::digest(root.as_bytes())),
        )
        .unwrap();
        if change != "missing_receipt" {
            lock.bind_workspace_manifests(&std::collections::BTreeMap::from([(
                "packages/member".into(),
                (
                    member.into(),
                    format!("sha256-{:x}", Sha256::digest(member.as_bytes())),
                ),
            )]))
            .unwrap();
        }
        let lock_bytes = if change == "invalid" {
            r#"{"lockfileVersion":8}"#.into()
        } else {
            lock.to_json().unwrap()
        };
        project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
        if change == "root" {
            project
                .write("package.json", root.replace("1.0.0", "1.0.1").as_bytes())
                .unwrap();
        }
        if change == "member" {
            project
                .write(
                    "packages/member/package.json",
                    member.replace("1.0.0", "1.0.1").as_bytes(),
                )
                .unwrap();
        }
        let (output, value) = status(project.path(), &[]);
        assert_eq!(
            output.status.success(),
            change == "none",
            "{change}: {value}"
        );
        assert_eq!(
            value["data"]["manifest_lock_agreement"],
            if change == "none" {
                "match"
            } else if change == "invalid" {
                "unknown"
            } else {
                "mismatch"
            },
            "{change}: {value}"
        );
        if change != "none" {
            let expected = match change {
                "invalid" => "LOCKFILE_INVALID",
                "root" => "LOCK_MANIFEST_MISMATCH",
                _ => "LOCK_WORKSPACE_MISMATCH",
            };
            assert!(
                value["data"]["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["code"] == expected),
                "{change}: {value}"
            );
        }
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes.as_bytes()
        );
        assert!(!project.path().join("node_modules").exists());
    }
}

#[test]
fn status_bounds_workspace_discovery_before_parsing_members() {
    let project = tapid_test_support::TempProject::new("status-member-limit").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    for i in 0..129 {
        project
            .write(
                format!("packages/member-{i}/package.json"),
                b"invalid: must not be parsed",
            )
            .unwrap();
    }
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["truncated"], true, "{value}");
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "WORKSPACE_DISCOVERY_LIMIT"),
        "{value}"
    );
    assert!(output.stdout.len() < 16 * 1024);
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
    let guidance = value["data"]["recovery_guidance"].as_str().unwrap();
    assert!(guidance.contains("tapid install"));
    assert!(guidance.contains("Preserve recovery files"));
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
    assert_eq!(
        value["project"],
        project.path().canonicalize().unwrap().display().to_string()
    );
    assert_eq!(
        value["data"]["workspace"]["manifest"],
        project
            .path()
            .join("packages/b/package.json")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    assert_eq!(value["data"]["workspace"]["linking"], "not_verified");
    let expected_location = format!(
        "{}#/dependencies/bad",
        Path::new("packages")
            .join("a")
            .join("package.json")
            .display()
    );
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "UNSUPPORTED_SPEC"
                && b["location"]
                    .as_str()
                    .unwrap()
                    .ends_with(&expected_location))
    );
    assert_eq!(
        value["data"]["registries"]["credential_environment_names"][0],
        "TAPID_STATUS_TEST_SECRET"
    );
    assert_eq!(value["data"]["snapshot"]["consistency"], "not_atomic");

    let (output, unknown) = status(project.path(), &["--workspace", "missing"]);
    assert!(!output.status.success());
    assert_eq!(unknown["data"]["workspace"]["selected"], "missing");
    assert!(unknown["data"]["workspace"]["manifest"].is_null());
    assert!(
        unknown["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| { b["code"] == "WORKSPACE_SELECTION_INVALID" })
    );
    assert_eq!(unknown["changes"]["state"], "unchanged");
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
    assert_eq!(
        value["data"]["verification_depth"],
        "local_metadata_and_ownership_only"
    );
    for check in [
        "atomic_snapshot_or_concurrent_writer_exclusion",
        "installed_tree_integrity_and_lock_correspondence",
    ] {
        assert!(
            value["data"]["checks_not_performed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == check)
        );
    }
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

#[test]
fn status_rejects_invalid_markers_even_without_node_modules() {
    for kind in ["empty", "invalid", "oversized", "directory", "valid"] {
        let project = tapid_test_support::TempProject::new("status-absent-tree-marker").unwrap();
        let manifest = br#"{"name":"app","version":"1.0.0"}"#;
        project.write("package.json", manifest).unwrap();
        let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
        project
            .write("tapid.lock", lock.to_json().unwrap().as_bytes())
            .unwrap();
        match kind {
            "empty" => {
                project.write(".tapid-managed", b"").unwrap();
            }
            "invalid" => {
                project.write(".tapid-managed", b"invalid").unwrap();
            }
            "oversized" => {
                project.write(".tapid-managed", &[b'x'; 129]).unwrap();
            }
            "directory" => {
                fs::create_dir(project.path().join(".tapid-managed")).unwrap();
            }
            "valid" => {
                project
                    .write(".tapid-managed", b"tapid-managed-v1\n")
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (output, value) = status(project.path(), &[]);
        assert_eq!(output.status.success(), kind == "valid", "{kind}: {value}");
        if kind == "valid" {
            assert_eq!(value["data"]["installation"][0]["state"], "absent");
        } else {
            assert!(
                value["data"]["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["code"] == "INSTALLATION_NOT_MANAGED"),
                "{kind}: {value}"
            );
        }
        assert!(!project.path().join("node_modules").exists());
        assert!(fs::symlink_metadata(project.path().join(".tapid-managed")).is_ok());
    }
}

#[test]
fn status_blocks_oversized_members_before_workspace_parsing() {
    use std::io::Write;
    let project = tapid_test_support::TempProject::new("status-oversized-member").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    let path = project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"1.0.0"}"#,
        )
        .unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    for _ in 0..1024 {
        file.write_all(&[b' '; 32 * 1024]).unwrap();
    }
    drop(file);
    let size = fs::metadata(&path).unwrap().len();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "WORKSPACE_INVALID"
                && b["location"] == path.canonicalize().unwrap().display().to_string()),
        "{value}"
    );
    assert!(output.stdout.len() < 16 * 1024);
    assert_eq!(fs::metadata(path).unwrap().len(), size);
}

#[cfg(unix)]
#[test]
fn status_rejects_symlinked_marker_without_following_or_replacing_it() {
    let project = tapid_test_support::TempProject::new("status-symlink-marker").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0"}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let target = project
        .write("private-marker", b"tapid-managed-v1\n")
        .unwrap();
    let marker = project.path().join(".tapid-managed");
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert!(
        value["data"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "INSTALLATION_NOT_MANAGED")
    );
    assert!(
        fs::symlink_metadata(marker)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target).unwrap(), b"tapid-managed-v1\n");
}

#[test]
fn status_reports_root_mismatch_despite_invalid_registry_or_workspace() {
    for invalid in ["registry", "workspace"] {
        let project =
            tapid_test_support::TempProject::new("status-independent-root-mismatch").unwrap();
        let manifest = if invalid == "workspace" {
            br#"{"name":"app","version":"1.0.1","workspaces":["packages/**"]}"#.as_slice()
        } else {
            br#"{"name":"app","version":"1.0.1"}"#.as_slice()
        };
        project.write("package.json", manifest).unwrap();
        if invalid == "registry" {
            project.write("tapid.toml", b"[registries\n").unwrap();
        }
        let previous = br#"{"name":"app","version":"1.0.0"}"#;
        let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(previous))).unwrap();
        let lock_bytes = lock.to_json().unwrap();
        project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
        let (output, value) = status(project.path(), &[]);
        assert!(!output.status.success());
        assert_eq!(
            value["data"]["manifest_lock_agreement"], "mismatch",
            "{invalid}: {value}"
        );
        for code in [
            "LOCK_MANIFEST_MISMATCH",
            if invalid == "registry" {
                "REGISTRY_CONFIGURATION_INVALID"
            } else {
                "WORKSPACE_INVALID"
            },
        ] {
            assert!(
                value["data"]["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["code"] == code),
                "missing {code}: {value}"
            );
        }
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes.as_bytes()
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
    }
}

#[test]
fn status_reports_replay_blocker_despite_invalid_registry_or_workspace() {
    for invalid in ["registry", "workspace"] {
        let project = tapid_test_support::TempProject::new("status-independent-replay").unwrap();
        let manifest = if invalid == "workspace" {
            br#"{"name":"app","version":"1.0.0","workspaces":["packages/**"]}"#.as_slice()
        } else {
            br#"{"name":"app","version":"1.0.0"}"#.as_slice()
        };
        project.write("package.json", manifest).unwrap();
        if invalid == "registry" {
            project.write("tapid.toml", b"[registries\n").unwrap();
        }
        let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
        legacy["lockfileVersion"] = serde_json::json!(5);
        let lock_bytes = legacy.to_string();
        project.write("tapid.lock", lock_bytes.as_bytes()).unwrap();
        let (output, value) = status(project.path(), &[]);
        assert!(!output.status.success());
        assert_eq!(value["data"]["manifest_lock_agreement"], "unknown");
        for code in [
            "LOCK_REPLAY_UNSUPPORTED",
            if invalid == "registry" {
                "REGISTRY_CONFIGURATION_INVALID"
            } else {
                "WORKSPACE_INVALID"
            },
        ] {
            assert!(
                value["data"]["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["code"] == code),
                "{invalid}, missing {code}: {value}"
            );
        }
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes.as_bytes()
        );
    }
}

#[test]
fn status_reports_root_bindings_without_requiring_workspace_discovery() {
    let project = tapid_test_support::TempProject::new("status-independent-root-bindings").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0","workspaces":["packages/**"],"dependencies":{"missing":"1"}}"#;
    project.write("package.json", manifest).unwrap();
    let lock = Lockfile::new(&format!("sha256-{:x}", Sha256::digest(manifest))).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    let (output, value) = status(project.path(), &[]);
    assert!(!output.status.success());
    assert_eq!(value["data"]["manifest_lock_agreement"], "mismatch");
    for code in ["LOCK_ROOT_BINDINGS_INVALID", "WORKSPACE_INVALID"] {
        assert!(
            value["data"]["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["code"] == code),
            "missing {code}: {value}"
        );
    }
}
