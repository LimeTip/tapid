use super::*;

#[test]
fn dependency_edits_reject_duplicate_keys_before_pending_recovery() {
    let fixture = command_support::CliFixture::new("edit-duplicate-pending");
    let manifest = br#"{"name":"app","version":"1.0.0"}"#;
    fixture.project.write("package.json", manifest).unwrap();
    fixture
        .project
        .write(".tapid-lifecycle-journal.json", b"pending")
        .unwrap();
    fixture.project.write("request.json", br#"{"version":1,"edits":[{"op":"remove","section":"dependencies","name":"a","name":"b"}]}"#).unwrap();
    let output = fixture
        .command(&["edit-dependencies", "--file", "request.json", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["operation"], "edit-dependencies");
    assert_eq!(result["changes"]["state"], "unchanged");
    assert_eq!(
        fs::read(fixture.project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(fixture.project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        b"pending"
    );
}

#[test]
fn dependency_edits_install_mixed_sections_and_report_each_input_and_graph() {
    let fixture = command_support::CliFixture::new("edit-mixed-report");
    fixture
        .project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","peerDependencies":{"old":"^1"}}"#,
        )
        .unwrap();
    fixture
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/aliased-root-cycle.json"),
        )
        .unwrap();
    fixture.project.write("request.json", br#"{"version":1,"edits":[{"op":"add","section":"dependencies","spec":"a@1.0.0"},{"op":"add","section":"devDependencies","spec":"b@1.0.0"},{"op":"remove","section":"peerDependencies","name":"old"}]}"#).unwrap();
    let output = fixture
        .command(&[
            "edit-dependencies",
            "--file",
            "request.json",
            "--registry-fixture",
            "registry.json",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["changes"]["state"], "committed");
    for index in 0..3 {
        assert_eq!(result["data"]["edits"][index]["index"], index);
        assert_eq!(result["data"]["edits"][index]["disposition"], "committed");
    }
    assert_eq!(result["data"]["graph"]["complete"], true);
    assert_eq!(result["data"]["graph"]["omitted"], 0);
    assert!(
        !result["data"]["graph"]["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.project.path().join("package.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["dependencies"]["a"], "1.0.0");
    assert_eq!(manifest["devDependencies"]["b"], "1.0.0");
    assert!(manifest["peerDependencies"]["old"].is_null());
}

#[test]
fn dependency_edits_invalid_last_edit_has_index_and_no_successful_edits() {
    let fixture = command_support::CliFixture::new("edit-invalid-last");
    let manifest = br#"{"name":"app","version":"1.0.0"}"#;
    fixture.project.write("package.json", manifest).unwrap();
    fixture
        .project
        .write(".tapid-lifecycle-journal.json", b"pending")
        .unwrap();
    fixture.project.write("request.json", br#"{"version":1,"edits":[{"op":"add","section":"dependencies","spec":"a@1"},{"op":"remove","section":"dependencies","name":"missing"}]}"#).unwrap();
    let output = fixture
        .command(&["edit-dependencies", "--file", "request.json", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["errors"][0]["context"]["edit_indices"],
        serde_json::json!([1])
    );
    assert_eq!(result["data"]["edits"].as_array().unwrap().len(), 2);
    for edit in result["data"]["edits"].as_array().unwrap() {
        assert_eq!(edit["disposition"], "not_committed");
    }
    assert_eq!(
        fs::read(fixture.project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(fixture.project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        b"pending"
    );
}

#[test]
fn dependency_edits_malformed_last_edit_reports_its_input_index() {
    let fixture = command_support::CliFixture::new("edit-shape-index");
    fixture.project.write("request.json", br#"{"version":1,"edits":[{"op":"add","section":"dependencies","spec":"a@1"},{"op":"move","name":"a","from":"dependencies","to":"devDependencies","extra":true}]}"#).unwrap();
    let output = fixture
        .command(&["edit-dependencies", "--file", "request.json", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["errors"][0]["context"]["edit_indices"],
        serde_json::json!([1])
    );
    assert!(!fixture.project.path().join("package.json").exists());
}

#[test]
fn dependency_edits_file_and_edit_limits_are_inclusive_and_never_omit_edits() {
    let fixture = command_support::CliFixture::new("edit-limits");
    fixture
        .project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    fixture
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/aliased-root-cycle.json"),
        )
        .unwrap();
    let edits = (0..256).map(|index| serde_json::json!({"op":"add","section":"peerDependencies","spec":format!("peer{index}@^1")})).collect::<Vec<_>>();
    let mut bytes = serde_json::to_vec(&serde_json::json!({"version":1,"edits":edits})).unwrap();
    bytes.resize(1_048_576, b' ');
    fixture.project.write("request.json", &bytes).unwrap();
    let output = fixture
        .command(&[
            "edit-dependencies",
            "--file",
            "request.json",
            "--registry-fixture",
            "registry.json",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["edits"].as_array().unwrap().len(), 256);
    assert_eq!(result["data"]["edits"][255]["index"], 255);
    let before = fs::read(fixture.project.path().join("package.json")).unwrap();
    bytes.push(b' ');
    fixture.project.write("request.json", &bytes).unwrap();
    let oversized = fixture
        .command(&["edit-dependencies", "--file", "request.json", "--json"])
        .output()
        .unwrap();
    assert!(!oversized.status.success());
    let mut edits = edits;
    edits.push(serde_json::json!({"op":"add","section":"peerDependencies","spec":"last@1"}));
    fixture
        .project
        .write(
            "request.json",
            &serde_json::to_vec(&serde_json::json!({"version":1,"edits":edits})).unwrap(),
        )
        .unwrap();
    let excessive = fixture
        .command(&["edit-dependencies", "--file", "request.json", "--json"])
        .output()
        .unwrap();
    assert!(!excessive.status.success());
    assert_eq!(
        fs::read(fixture.project.path().join("package.json")).unwrap(),
        before
    );
}

#[test]
fn dependency_edits_strict_shapes_fail_before_any_project_or_journal_access() {
    let fixture = command_support::CliFixture::new("edit-invalid-shapes");
    fixture
        .project
        .write(".tapid-lifecycle-journal.json", b"pending")
        .unwrap();
    let invalid: &[&[u8]] = &[
        b"\xff", b"{", b"null", b"[]", b"{}",
        br#"{"version":2,"edits":[]}"#,
        br#"{"version":1,"edits":[]}"#,
        br#"{"version":1,"edits":[],"project":"elsewhere"}"#,
        br#"{"version":1,"edits":[{"op":"exec","command":"hello"}]}"#,
        br#"{"version":1,"edits":[{"op":"add","section":"runtime","spec":"a@1"}]}"#,
        br#"{"version":1,"edits":[{"op":"add","section":"dependencies","spec":null}]}"#,
        br#"{"version":1,"edits":[{"op":"remove","section":"dependencies","name":"a","spec":"b"}]}"#,
        br#"{"version":1,"edits":[{"op":"move","name":"a","from":"dependencies"}]}"#,
        br#"{"version":1,"edits":[{"op":"add","section":"dependencies","section":"devDependencies","spec":"a"}]}"#,
        br#"{"version":1,"edits":[{"op":"add","section":"dependencies","spec":"a"}],"edits":[]}"#,
        br#"{"version":1,"edits":[{"op":"add","section":"dependencies","spec":"a"}]} true"#,
    ];
    for bytes in invalid {
        fixture.project.write("request.json", bytes).unwrap();
        let output = fixture
            .command(&["edit-dependencies", "--file", "request.json", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{bytes:?}");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["errors"][0]["code"], "INVALID_REQUEST", "{bytes:?}");
        assert_eq!(result["changes"]["state"], "unchanged");
        assert!(!fixture.project.path().join("package.json").exists());
        assert_eq!(
            fs::read(fixture.project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
            b"pending"
        );
    }
    let directory = fixture
        .command(&["edit-dependencies", "--file", ".", "--json"])
        .output()
        .unwrap();
    assert!(!directory.status.success());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.project.path().join("request.json"),
            fixture.project.path().join("link.json"),
        )
        .unwrap();
        let symlink = fixture
            .command(&["edit-dependencies", "--file", "link.json", "--json"])
            .output()
            .unwrap();
        assert!(!symlink.status.success());
    }
}

#[test]
fn dependency_edits_workspace_move_preserves_alias_and_unselected_member() {
    let fixture = command_support::CliFixture::new("edit-workspace-alias");
    fixture.project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"},"workspaces":["packages/*"]}"#).unwrap();
    fixture
        .project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"1.0.0","dependencies":{"old":"npm:h3@1.0.0"}}"#,
        )
        .unwrap();
    let untouched = br#"{"name":"untouched","version":"1.0.0"}"#;
    fixture
        .project
        .write("packages/untouched/package.json", untouched)
        .unwrap();
    fixture
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/npm-aliases.json"),
        )
        .unwrap();
    let initial = fixture
        .command(&["install", "--registry-fixture", "registry.json", "--json"])
        .output()
        .unwrap();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stdout)
    );
    fixture.project.write("request.json", br#"{"version":1,"edits":[{"op":"move","name":"old","from":"dependencies","to":"devDependencies"},{"op":"add","section":"optionalDependencies","spec":"extra@npm:h3@2.0.0"},{"op":"add","section":"peerDependencies","spec":"h3@^1"}]}"#).unwrap();
    let output = fixture
        .command(&[
            "edit-dependencies",
            "--file",
            "request.json",
            "--workspace",
            "member",
            "--registry-fixture",
            "registry.json",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.project.path().join("packages/member/package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["devDependencies"]["old"], "npm:h3@1.0.0");
    assert!(manifest["dependencies"]["old"].is_null());
    assert_eq!(manifest["optionalDependencies"]["extra"], "npm:h3@2.0.0");
    assert_eq!(manifest["peerDependencies"]["h3"], "^1");
    assert_eq!(
        fs::read(
            fixture
                .project
                .path()
                .join("packages/untouched/package.json")
        )
        .unwrap(),
        untouched
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["data"]["graph"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["kind"] == "workspace" && record["action"] == "changed")
    );
    fixture
        .project
        .write(".tapid-lifecycle-journal.json", b"pending")
        .unwrap();
    for selector in ["missing", "../member", "workspace:member"] {
        let output = fixture
            .command(&[
                "edit-dependencies",
                "--file",
                "request.json",
                "--workspace",
                selector,
                "--json",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["changes"]["state"], "unchanged");
        assert_eq!(
            fs::read(fixture.project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
            b"pending"
        );
    }
    fixture
        .project
        .write(
            "packages/duplicate/package.json",
            br#"{"name":"member","version":"1.0.0"}"#,
        )
        .unwrap();
    let duplicate = fixture
        .command(&[
            "edit-dependencies",
            "--file",
            "request.json",
            "--workspace",
            "member",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert_eq!(
        fs::read(fixture.project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        b"pending"
    );
}
