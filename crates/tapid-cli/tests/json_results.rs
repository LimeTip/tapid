use std::{fs, process::Command};
use tapid_test_support::TempProject;

fn invoke(project: &TempProject, args: &[&str]) -> (std::process::Output, serde_json::Value) {
    let home = tapid_test_support::TempHome::new("json-command").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("LOCALAPPDATA", home.path().join("cache"))
        .args(args)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).expect("one JSON result on stdout");
    assert!(output.stderr.is_empty(), "{:?}", output);
    (output, value)
}

#[test]
fn json_peer_conflict_is_actionable_without_human_output() {
    for provider in [None, Some("2.0.0")] {
        let project = TempProject::new("json-peer-context").unwrap();
        let mut dependencies = serde_json::json!({"plugin":"1.0.0"});
        if let Some(version) = provider {
            dependencies["@local/host"] = serde_json::json!(format!("npm:h3@{version}"));
        }
        project
            .write(
                "package.json",
                serde_json::json!({"name":"app","version":"1.0.0","dependencies":dependencies,"peerDependencies":{"plugin":"1.0.0"}})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let mut fixture: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
        let mut plugin = fixture["packages"][0].clone();
        plugin["name"] = "plugin".into();
        plugin["peerDependencies"] = serde_json::json!({"@local/host":"npm:h3@^1"});
        fixture["packages"].as_array_mut().unwrap().push(plugin);
        project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let (output, result) = invoke(
            &project,
            &[
                "install",
                "--json",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                "store",
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], "PEER_DEPENDENCY_UNSATISFIED");
        assert_eq!(result["retry"], "after_correction");
        assert_eq!(result["changes"]["state"], "unchanged");
        assert_eq!(result["changes"]["files"], serde_json::json!([]));
        let context = &result["errors"][0]["context"];
        assert_eq!(context["package"]["name"], "plugin");
        assert_eq!(context["package"]["version"], "1.0.0");
        assert_eq!(context["package"]["source"], "https://registry.npmjs.org");
        assert_eq!(context["peer"]["name"], "@local/host");
        assert_eq!(context["peer"]["requirement"], "npm:h3@^1");
        assert_eq!(
            context["peer"]["status"],
            if provider.is_some() {
                "incompatible"
            } else {
                "missing"
            }
        );
        assert_eq!(context["declaration"]["origin"], "metadata");
        assert_eq!(context["requirements"].as_array().unwrap().len(), 1);
        assert_eq!(
            context["declaration"]["pointer"],
            "/peerDependencies/@local~1host"
        );
        assert_eq!(
            context["requirements"][0]["declaration"]["pointer"],
            "/dependencies/plugin"
        );
        if let Some(version) = provider {
            assert_eq!(context["peer"]["provider"]["name"], "h3");
            assert_eq!(context["peer"]["provider"]["version"], version);
            assert_eq!(
                context["peer"]["declaration"]["pointer"],
                "/dependencies/@local~1host"
            );
            assert_eq!(
                context["peer"]["declaration"]["manifest_path"]["value"],
                std::fs::canonicalize(project.path())
                    .unwrap()
                    .join("package.json")
                    .to_str()
                    .unwrap()
            );
        } else {
            assert!(context["peer"]["provider"].is_null());
        }
    }
}

#[test]
fn json_manifest_failure_identifies_field_without_echoing_contents() {
    let project = TempProject::new("json-manifest-context").unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"@scope/pkg":{"token":"fixture-secret"}}}"#).unwrap();
    let (output, result) = invoke(&project, &["install", "--json", "--store-dir", "store"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "MANIFEST_INVALID");
    let context = &result["errors"][0]["context"];
    assert_eq!(
        context["declaration"]["pointer"],
        "/dependencies/@scope~1pkg"
    );
    assert_eq!(context["comparison"]["expected"], "string");
    assert_eq!(context["comparison"]["observed"], "object");
    assert!(!result.to_string().contains("fixture-secret"));
}

#[test]
fn json_invalid_workspace_manifest_keeps_its_location() {
    let project = TempProject::new("json-member-manifest-context").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"1.0.0","dependencies":{"@scope/pkg":false}}"#,
        )
        .unwrap();
    let (output, result) = invoke(&project, &["install", "--json", "--store-dir", "store"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "INVALID_REQUEST");
    let context = &result["errors"][0]["context"];
    assert_eq!(
        context["declaration"]["pointer"],
        "/dependencies/@scope~1pkg"
    );
    assert_eq!(
        context["declaration"]["manifest_path"]["value"],
        fs::canonicalize(project.path())
            .unwrap()
            .join("packages/member/package.json")
            .to_str()
            .unwrap()
    );
    assert_eq!(context["comparison"]["observed"], "boolean");
}

#[test]
fn json_resolution_conflict_lists_requirements_and_candidates() {
    let project = TempProject::new("json-resolution-context").unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"first":"npm:h3@^1","second":"npm:h3@^3"}}"#).unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "RESOLUTION_FAILED");
    let context = &result["errors"][0]["context"];
    assert_eq!(context["package"]["name"], "h3");
    assert!(
        context["available"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("1.0.0"))
    );
    assert!(
        context["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|requirement| requirement["requirement"] == "npm:h3@^3"
                && requirement["declaration"]["pointer"] == "/dependencies/second")
    );
}

#[test]
fn json_integrity_failure_reports_expected_and_observed() {
    let project = TempProject::new("json-integrity-context").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    let mut fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
    let expected = fixture["packages"][1]["integrity"].clone();
    let observed = fixture["packages"][0]["integrity"].clone();
    fixture["packages"][0]["integrity"] = expected.clone();
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let error = &result["errors"][0];
    assert_eq!(error["code"], "INTEGRITY_MISMATCH");
    assert_eq!(error["context"]["package"]["name"], "h3");
    assert_eq!(error["context"]["comparison"]["kind"], "artifact_integrity");
    assert_eq!(error["context"]["comparison"]["algorithm"], "sha512");
    assert_eq!(error["context"]["comparison"]["expected"], expected);
    assert_eq!(error["context"]["comparison"]["observed"], observed);
}

#[test]
fn json_frozen_integrity_failure_has_the_same_context() {
    let project = TempProject::new("json-frozen-integrity-context").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    let store = project.path().join("store");
    let mut fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let (output, installed) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{installed}");
    let lock_path = project.path().join("tapid.lock");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = "https://registry.npmjs.org/h3/-/h3-1.0.0.tgz".into();
    }
    fs::write(&lock_path, lock.to_string()).unwrap();
    fs::remove_dir_all(project.path().join("store")).unwrap();
    fixture["packages"][0]["artifact"] = fixture["packages"][1]["artifact"].clone();
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--frozen",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "INTEGRITY_MISMATCH");
    let context = &result["errors"][0]["context"];
    assert_eq!(context["package"]["name"], "h3");
    assert_eq!(context["comparison"]["kind"], "artifact_integrity");
    assert_eq!(
        context["comparison"]["expected"],
        fixture["packages"][0]["integrity"]
    );
    assert_eq!(
        context["comparison"]["observed"],
        fixture["packages"][1]["integrity"]
    );
}

#[test]
fn json_lock_mismatch_retains_comparison_in_install_and_why() {
    let project = TempProject::new("json-lock-context").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let (output, _) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert!(output.status.success());
    project
        .write("package.json", br#"{"name":"app","version":"1.0.1"}"#)
        .unwrap();
    for args in [
        vec!["install", "--offline", "--json", "--store-dir", "store"],
        vec!["why", "h3", "--json"],
    ] {
        let (output, result) = invoke(&project, &args);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], "LOCK_MANIFEST_MISMATCH");
        let comparison = &result["errors"][0]["context"]["comparison"];
        assert_eq!(comparison["kind"], "root_manifest_digest");
        assert_eq!(comparison["algorithm"], "sha256");
        assert_eq!(
            comparison["expected"]
                .as_str()
                .unwrap()
                .strip_prefix("sha256-")
                .unwrap()
                .len(),
            64
        );
        assert_ne!(comparison["expected"], comparison["observed"]);
    }
}

#[test]
fn json_workspace_conflict_locates_each_declaring_member() {
    let project = TempProject::new("json-workspace-conflict").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    for (member, requirement) in [("a", "^1"), ("b", "^2")] {
        project.write(format!("packages/{member}/package.json"), serde_json::json!({"name":member,"version":"1.0.0","dependencies":{"h3":requirement},"peerDependencies":{"h3":requirement}}).to_string().as_bytes()).unwrap();
    }
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let requirements = result["errors"][0]["context"]["requirements"]
        .as_array()
        .unwrap();
    assert_eq!(requirements.len(), 2);
    for (member, requirement) in [("a", "^1"), ("b", "^2")] {
        let declaration = &requirements
            .iter()
            .find(|entry| entry["requirement"] == requirement)
            .unwrap()["declaration"];
        assert_eq!(declaration["origin"], "manifest");
        assert_eq!(declaration["workspace"], member);
        assert_eq!(declaration["pointer"], "/dependencies/h3");
        assert_eq!(
            declaration["manifest_path"]["value"],
            fs::canonicalize(project.path())
                .unwrap()
                .join(format!("packages/{member}/package.json"))
                .to_str()
                .unwrap()
        );
    }
}

#[test]
fn json_transitive_failure_keeps_metadata_provenance() {
    let project = TempProject::new("json-transitive-context").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"alias-parent":"1.0.0"}}"#,
        )
        .unwrap();
    let mut fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
    fixture["packages"][4]["dependencies"] = serde_json::json!({"h3-v2":"npm:h3@^3"});
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let requirement = &result["errors"][0]["context"]["requirements"][0];
    assert_eq!(requirement["package"]["name"], "alias-parent");
    assert_eq!(requirement["local_name"], "h3-v2");
    assert_eq!(requirement["declaration"]["origin"], "metadata");
    assert_eq!(requirement["declaration"]["pointer"], "/dependencies/h3-v2");
    assert!(requirement["declaration"]["manifest_path"].is_null());
}

#[test]
fn json_install_success_and_handled_failure() {
    let project = TempProject::new("json-install").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project.write("registry.json", br#"{"packages":[{"registry":"https://jsr.io","name":"@scope/unused","version":"1.0.0","artifact":"https://jsr.io/@scope/unused/1.0.0.tgz"}]}"#).unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            "store",
        ],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "install");
    assert_eq!(result["outcome"], "success");
    assert_eq!(result["data"]["package_count"], 0);
    assert_eq!(result["changes"]["state"], "committed");
    assert_eq!(result["project_path"]["encoding"], "utf8");
    assert_eq!(
        result["project_path"]["value"],
        std::fs::canonicalize(project.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(result["truncated_fields"], serde_json::json!([]));
    let native_files = result["changes"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| {
            assert_eq!(path["encoding"], "utf8");
            path["value"].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        native_files,
        *result["changes"]["files"].as_array().unwrap()
    );
    std::fs::remove_file(project.path().join("tapid.lock")).unwrap();
    let (output, result) = invoke(
        &project,
        &["--json", "install", "--offline", "--store-dir", "store"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["outcome"], "failure");
    assert_eq!(result["errors"][0]["code"], "LOCKFILE_MISSING");
    assert_eq!(result["retry"], "after_correction");
}

#[test]
fn json_parse_errors_and_unsupported_commands_do_not_echo_input() {
    let project = TempProject::new("json-parse").unwrap();
    let (output, result) = invoke(&project, &["--json", "--fixture-secret"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
    let (output, result) = invoke(&project, &["--json", "init"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
    assert!(!project.path().join("package.json").exists());
    let (output, result) = invoke(
        &project,
        &["--json", "import-package-lock", "package-lock.json"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["operation"], "import-package-lock");
    assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
    assert!(!project.path().join("tapid.lock").exists());
}

#[test]
fn json_install_replays_imported_npm_selections() {
    let project = TempProject::new("json-npm-import").unwrap();
    for (path, bytes) in [
        (
            "package.json",
            include_bytes!("fixtures/npm-import/package.json").as_slice(),
        ),
        (
            "package-lock.json",
            include_bytes!("fixtures/npm-import/package-lock.json").as_slice(),
        ),
        (
            "registry.json",
            include_bytes!("fixtures/npm-import/registry.json").as_slice(),
        ),
    ] {
        project.write(path, bytes).unwrap();
    }
    let import = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["import-package-lock", "package-lock.json"])
        .output()
        .unwrap();
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let store = project.path().join("store");
    for offline in [false, true] {
        let mut args = vec![
            "install",
            "--json",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ];
        if offline {
            args.push("--offline");
        } else {
            args.extend(["--registry-fixture", "registry.json"]);
        }
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], "install");
        assert_eq!(result["data"]["package_count"], 4);
        assert_eq!(result["outcome"], "success");
    }
}

#[test]
fn outdated_schema_6_uses_direct_root_instead_of_newer_transitive_version() {
    let project = TempProject::new("outdated-legacy-root").unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"^1.0.0","alias-parent":"1.0.0"}}"#).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let store = project.path().join("store");
    let (output, installed) = invoke(
        &project,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{installed}");
    let lock_path = project.path().join("tapid.lock");
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    lock["lockfileVersion"] = 6.into();
    lock.as_object_mut().unwrap().remove("rootBindings");
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        if let Some(aliases) = package.as_object_mut().unwrap().remove("dependencyAliases") {
            for (local, actual) in aliases.as_object().unwrap() {
                let dependencies = package["dependencies"].as_object_mut().unwrap();
                let target = dependencies.remove(local).unwrap();
                dependencies.insert(actual.as_str().unwrap().to_owned(), target);
            }
        }
    }
    fs::write(&lock_path, lock.to_string()).unwrap();
    let before = fs::read(&lock_path).unwrap();
    let (output, report) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{report}");
    let entry = report["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["identity"] == "h3")
        .unwrap();
    assert_eq!(entry["locked"], "1.0.0");
    assert_eq!(
        entry["compatible_impact"],
        serde_json::json!({"version_change":"unchanged","lockfile":"unchanged","manifest":"unchanged"})
    );
    assert_eq!(
        entry["available_impact"],
        serde_json::json!({"version_change":"major","lockfile":"changed","manifest":"changed"})
    );
    assert_eq!(fs::read(&lock_path).unwrap(), before);
}

#[test]
fn outdated_peer_declarations_do_not_claim_direct_lockfile_changes() {
    let project = TempProject::new("outdated-peer-only").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let store = project.path().join("store");
    let (output, added) = invoke(
        &project,
        &[
            "--json",
            "add",
            "h3@^1.0.0",
            "--peer",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{added}");
    assert_eq!(added["data"]["package_count"], 0);
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let (output, report) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{report}");
    let entry = &report["data"]["entries"][0];
    assert_eq!(entry["kind"], "peerDependencies");
    assert!(entry["locked"].is_null());
    assert_eq!(
        entry["compatible_impact"],
        serde_json::json!({"version_change":"unlocked", "lockfile":"unchanged", "manifest":"unchanged"})
    );
    assert_eq!(
        entry["available_impact"],
        serde_json::json!({"version_change":"unlocked", "lockfile":"unchanged", "manifest":"changed"})
    );
    let human = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["outdated", "--registry-fixture", "registry.json"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("compatible-change=unlocked compatible-lockfile=unchanged"));
    assert!(stdout.contains(
        "available-change=unlocked available-lockfile=unchanged available-manifest=changed"
    ));
    let (output, offline) = invoke(&project, &["--json", "outdated", "--offline"]);
    assert!(output.status.success(), "{offline}");
    assert_eq!(offline["outcome"], "partial");
    assert_eq!(
        offline["data"]["entries"][0]["available_impact"],
        serde_json::json!({"version_change":"unknown", "lockfile":"unchanged", "manifest":"unknown"})
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
    // A provider installed by another section still is not selected by its peer range.
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"^1.0.0"},"peerDependencies":{"h3":"^1.0.0"}}"#).unwrap();
    let (output, added) = invoke(
        &project,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{added}");
    let (output, overlapping) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{overlapping}");
    let entries = overlapping["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert_eq!(entry["locked"], "1.0.0");
        assert_eq!(entry["available_impact"]["version_change"], "major");
        assert_eq!(entry["available_impact"]["manifest"], "changed");
        assert_eq!(
            entry["available_impact"]["lockfile"],
            if entry["kind"] == "peerDependencies" {
                "unchanged"
            } else {
                "changed"
            }
        );
    }
}

#[test]
fn json_outdated_failures_do_not_label_locked_versions_as_selected() {
    let project = TempProject::new("json-outdated-failure-version").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
        )
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    project.write("empty.json", br#"{"packages":[]}"#).unwrap();
    project.write("invalid.json", b"not json").unwrap();
    let store = project.path().join("store");
    let (output, installed) = invoke(
        &project,
        &[
            "--json",
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{installed}");
    for (requirement, options, code) in [
        ("1.0.0", vec!["--offline"], "REGISTRY_METADATA_UNAVAILABLE"),
        (
            "1.0.0",
            vec!["--registry-fixture", "empty.json"],
            "REGISTRY_METADATA_INVALID",
        ),
        (
            "1.0.0",
            vec!["--registry-fixture", "invalid.json"],
            "REGISTRY_METADATA_INVALID",
        ),
        (
            "invalid requirement ??",
            vec!["--registry-fixture", "registry.json"],
            "INVALID_REQUEST",
        ),
    ] {
        project.write("package.json", serde_json::json!({"name":"app","version":"1.0.0","dependencies":{"h3":requirement}}).to_string().as_bytes()).unwrap();
        let mut args = vec!["--json", "outdated"];
        args.extend(options);
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["outcome"], "partial");
        let entry = &result["data"]["entries"][0];
        assert_eq!(entry["error"]["code"], code);
        assert_eq!(entry["locked"], "1.0.0");
        assert_eq!(entry["error"]["context"]["package"]["name"], "h3");
        assert!(
            entry["error"]["context"]["package"]["version"].is_null(),
            "{entry}"
        );
    }
}

#[test]
fn json_lifecycle_results_and_partial_outdated_metadata() {
    let project = TempProject::new("json-lifecycle").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    project
        .write("registry.json", include_bytes!("fixtures/npm-aliases.json"))
        .unwrap();
    let store = project.path().join("store");
    let store = store.to_str().unwrap();
    for (operation, packages) in [("add", vec!["foo@npm:h3@^1.0.0"]), ("update", vec![])] {
        let mut args = vec![
            "--json",
            operation,
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store,
        ];
        args.extend(packages);
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], operation);
        assert_eq!(result["outcome"], "success");
    }
    let (output, result) = invoke(
        &project,
        &["--json", "outdated", "--registry-fixture", "registry.json"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["outcome"], "success");
    assert_eq!(result["data"]["truncated"], false);
    assert_eq!(
        result["data"]["entries"][0]["compatible_impact"],
        serde_json::json!({
            "version_change": "unchanged", "lockfile": "unchanged", "manifest": "unchanged"
        })
    );
    assert_eq!(
        result["data"]["entries"][0]["available_impact"],
        serde_json::json!({
            "version_change": "major", "lockfile": "changed", "manifest": "changed"
        })
    );
    assert_eq!(
        result["data"]["entries"][0]["newest_available"],
        "2.0.1-rc.20"
    );
    project
        .write("missing.json", br#"{"packages":[]}"#)
        .unwrap();
    let manifest_before = fs::read(project.path().join("package.json")).unwrap();
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let (output, offline) = invoke(&project, &["--json", "outdated", "--offline"]);
    assert!(output.status.success(), "{offline}");
    assert_eq!(offline["outcome"], "partial");
    assert_eq!(
        offline["data"]["entries"][0]["error"]["code"],
        "REGISTRY_METADATA_UNAVAILABLE"
    );
    assert_eq!(
        offline["data"]["entries"][0]["error"]["context"]["package"]["name"],
        "h3"
    );
    assert_eq!(
        offline["data"]["entries"][0]["error"]["context"]["declaration"]["pointer"],
        "/dependencies/foo"
    );
    assert!(offline["data"]["entries"][0]["newest_available"].is_null());
    assert_eq!(
        offline["data"]["entries"][0]["available_impact"]["lockfile"],
        "unknown"
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
    let (output, fixture_offline) = invoke(
        &project,
        &[
            "--json",
            "outdated",
            "--offline",
            "--registry-fixture",
            "registry.json",
        ],
    );
    assert!(output.status.success());
    assert_eq!(fixture_offline, result);
    let (output, result) = invoke(
        &project,
        &["outdated", "--json", "--registry-fixture", "missing.json"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["outcome"], "partial");
    assert_eq!(result["changes"]["state"], "unchanged");
    assert!(result["data"]["entries"][0]["error"]["code"].is_string());
    assert!(result["data"]["entries"][0]["newest_available"].is_null());
    assert_eq!(
        result["data"]["entries"][0]["available_impact"],
        serde_json::json!({
            "version_change": "unknown", "lockfile": "unknown", "manifest": "unknown"
        })
    );
    let (output, result) = invoke(&project, &["prune", "--json", "--store-dir", store]);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["operation"], "prune");
    let (output, result) = invoke(
        &project,
        &[
            "remove",
            "foo",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store,
        ],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["operation"], "remove");
}

#[test]
fn json_early_failures_are_single_objects() {
    let project = TempProject::new("json-early").unwrap();
    for args in [
        vec!["add"],
        vec!["remove"],
        vec!["install", "--workspace", "missing"],
        vec!["update"],
        vec!["outdated"],
        vec!["prune"],
    ] {
        let mut args = args;
        args.push("--json");
        let (output, result) = invoke(&project, &args);
        assert_eq!(output.status.code(), Some(1), "{result}");
        assert_eq!(result["outcome"], "failure");
        assert!(result["errors"][0]["code"].is_string());
    }
    // A forwarded token belongs to the child and does not select JSON parsing.
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["run", "--", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn json_help_and_version_are_successful_without_project_access() {
    let project = TempProject::new("json-information").unwrap();
    project.write("package.json", b"invalid manifest").unwrap();
    project
        .write("tapid.toml", b"invalid configuration")
        .unwrap();
    for (args, expected) in [
        (vec!["--json", "--help"], "Install dependencies"),
        (vec!["--help", "--json"], "Install dependencies"),
        (vec!["--json", "install", "--help"], "--registry-fixture"),
        (vec!["i", "--json", "-h"], "--registry-fixture"),
        (vec!["--json", "help", "run"], "--node-runtime"),
        (vec!["manifest", "validate", "--json", "--help"], "Usage:"),
    ] {
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["operation"], "help");
        assert_eq!(result["outcome"], "success");
        assert_eq!(result["errors"], serde_json::json!([]));
        assert!(result["project"].is_null());
        let help = result["data"]["text"].as_str().unwrap();
        assert!(help.contains(expected), "{help}");
        assert!(!help.contains('\u{1b}'));
    }
    for args in [["--json", "--version"], ["-V", "--json"]] {
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], "version");
        assert_eq!(result["outcome"], "success");
        assert_eq!(result["errors"], serde_json::json!([]));
        assert_eq!(result["data"]["name"], "tapid");
        assert_eq!(result["data"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(result["project"].is_null());
    }
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 2);
}

#[test]
fn json_workspace_selection_errors_have_consistent_codes() {
    let project = TempProject::new("json-workspace-codes").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    for operation in ["install", "update", "outdated", "prune"] {
        let (output, result) = invoke(&project, &["--json", operation, "--workspace", "missing"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], "INVALID_REQUEST", "{result}");
    }
}

#[test]
fn json_outdated_limit_can_retrieve_entries_beyond_the_default() {
    use sha2::{Digest, Sha256};
    let project = TempProject::new("json-outdated-limit").unwrap();
    let dependencies = (0..105)
        .map(|index| (format!("package-{index:03}"), serde_json::json!("^1.0.0")))
        .collect::<serde_json::Map<_, _>>();
    let manifest =
        serde_json::json!({"name":"app", "version":"1.0.0", "dependencies": dependencies})
            .to_string();
    project.write("package.json", manifest.as_bytes()).unwrap();
    let digest = format!(
        "sha256-{}",
        hex::encode(Sha256::digest(manifest.as_bytes()))
    );
    let lock = tapid_lockfile::Lockfile::new(&digest).unwrap();
    project
        .write("tapid.lock", lock.to_json().unwrap().as_bytes())
        .unwrap();
    project
        .write("registry.json", br#"{"packages":[]}"#)
        .unwrap();
    for (limit, expected, truncated) in [
        (None, 100, true),
        (Some("103"), 103, true),
        (Some("0"), 105, false),
    ] {
        let mut args = vec!["--json", "outdated", "--registry-fixture", "registry.json"];
        if let Some(limit) = limit {
            args.extend(["--json-limit", limit]);
        }
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(
            result["data"]["entries"].as_array().unwrap().len(),
            expected
        );
        assert_eq!(result["data"]["truncated"], truncated);
        assert_eq!(result["data"]["total_entries"], 105);
        assert_eq!(result["outcome"], "partial");
    }
    let (output, result) = invoke(
        &project,
        &[
            "outdated",
            "--json-limit",
            "0",
            "--json",
            "--registry-fixture",
            "registry.json",
        ],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["entries"].as_array().unwrap().len(), 105);
    let (output, result) = invoke(&project, &["--json", "outdated", "--json-limit", "-1"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(project.path())
        .args(["outdated", "--json-limit", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn json_unsupported_results_preserve_canonical_command_names() {
    let project = TempProject::new("json-unsupported-operation").unwrap();
    for (args, operation) in [
        (vec!["--json", "init"], "init"),
        (vec!["--json", "ci"], "ci"),
        (vec!["--json", "run", "test"], "run"),
        (vec!["--json", "manifest", "validate"], "manifest"),
        (vec!["--json", "lock", "verify"], "lock"),
        (vec!["--json", "license"], "license"),
        (vec!["--json", "upgrade"], "upgrade"),
        (vec!["--json"], "none"),
    ] {
        let (output, result) = invoke(&project, &args);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["operation"], operation);
        assert_eq!(result["errors"][0]["code"], "JSON_UNSUPPORTED_COMMAND");
    }
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 0);
}
