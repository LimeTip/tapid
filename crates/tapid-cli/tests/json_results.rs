#[path = "json_cases/outdated.rs"]
mod outdated;

use std::{fs, process::Command};
use tapid_test_support::{TempHome, TempProject};

/// Run an isolated CLI command and require a single JSON result with empty stderr.
fn invoke(project: &TempProject, args: &[&str]) -> (std::process::Output, serde_json::Value) {
    let home = TempHome::new("json-result").unwrap();
    let output = command(project, &home, args).output().unwrap();
    let value = serde_json::from_slice(&output.stdout).expect("one JSON result on stdout");
    assert!(output.stderr.is_empty(), "{:?}", output);
    (output, value)
}

/// Build isolated invocations, with explicit test hooks applied only by their scenarios.
fn command(project: &TempProject, home: &TempHome, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CACHE_HOME", home.path())
        .env("LOCALAPPDATA", home.path())
        .env_remove("TAPID_NPM_TOKEN")
        .env_remove("TAPID_TEST_LOCKFILE_RENAME_FAILURE")
        .current_dir(project.path())
        .args(args);
    command
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
fn json_effective_overrides_locate_root_manifest_fields() {
    for (workspace, local, previous, selector, expected) in [
        (false, "h3", "^1", "npm:h3", "^3"),
        (false, "h3-v2", "npm:h3@^1", "h3-v2", "npm:h3@^3"),
        (
            false,
            "@local/direct",
            "npm:@actual/scoped@^1",
            "npm:@local/direct",
            "npm:@actual/scoped@^3",
        ),
        (true, "h3", "^1", "npm:h3", "^3"),
    ] {
        let project = TempProject::new("json-override-context").unwrap();
        let mut manifest =
            serde_json::json!({"name":"app","version":"1.0.0","overrides":{selector:"^3"}});
        let mut fixture: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/npm-aliases.json")).unwrap();
        if workspace {
            manifest["workspaces"] = serde_json::json!(["packages/*"]);
            project.write("packages/member/package.json", serde_json::json!({"name":"member","version":"1.0.0","dependencies":{local:previous},"peerDependencies":{local:"^3"},"overrides":{local:"^4"}}).to_string().as_bytes()).unwrap();
        } else {
            manifest["dependencies"] = serde_json::json!({"alias-parent":"1.0.0"});
            fixture["packages"][4]["dependencies"] = serde_json::json!({local:previous});
        }
        project
            .write("package.json", manifest.to_string().as_bytes())
            .unwrap();
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
        let requirements = result["errors"][0]["context"]["requirements"]
            .as_array()
            .unwrap();
        assert_eq!(requirements.len(), 1, "{result}");
        let requirement = &requirements[0];
        assert_eq!(requirement["local_name"], local);
        assert_eq!(requirement["requirement"], expected);
        assert_eq!(requirement["declaration"]["origin"], "manifest");
        assert_eq!(
            requirement["declaration"]["pointer"],
            format!(
                "/overrides/{}",
                selector.replace('~', "~0").replace('/', "~1")
            )
        );
        assert_eq!(
            requirement["declaration"]["manifest"],
            project
                .path()
                .canonicalize()
                .unwrap()
                .join("package.json")
                .to_str()
                .unwrap()
        );
        assert!(requirement["declaration"]["workspace"].is_null());
        if !workspace {
            assert_eq!(requirement["package"]["name"], "alias-parent");
        }
    }
}

#[test]
fn json_transitive_failure_keeps_metadata_provenance() {
    let project = TempProject::new("json-transitive-context").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"alias-parent":"1.0.0"},"overrides":{"h3":"^3"}}"#,
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

/// Reject malformed or unsupported requests without echoing values or writing project outputs.
#[test]
fn json_parse_errors_and_unsupported_commands_do_not_echo_input() {
    let project = TempProject::new("json-parse").unwrap();
    let (output, result) = invoke(&project, &["--json", "--fixture-secret"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
    let (output, result) = invoke(&project, &["--json", "init"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "init");
    assert_eq!(result["outcome"], "success");
    assert_eq!(result["changes"]["state"], "committed");
    assert!(
        result["changes"]["files"][0]
            .as_str()
            .unwrap()
            .ends_with("/package.json")
    );

    let (output, result) = invoke(&project, &["--json", "init"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(result["outcome"], "failure");
    assert_eq!(result["errors"][0]["code"], "MANIFEST_ALREADY_EXISTS");
    let (output, result) = invoke(
        &project,
        &["--json", "import-package-lock", "package-lock.json"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["operation"], "import-package-lock");
    assert_eq!(result["errors"][0]["code"], "LOCKFILE_INVALID");
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

/// Keep canonical operation names even when JSON execution is unsupported.
#[test]
fn json_unsupported_results_preserve_canonical_command_names() {
    let project = TempProject::new("json-unsupported-operation").unwrap();
    for (args, operation) in [
        (vec!["--json", "run", "test"], "run"),
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

/// Exercise validation, import, and native CI without changing pinned selections on replay.
#[test]
fn json_automation_commands_validate_import_and_ci() {
    let project = TempProject::new("json-automation").unwrap();
    project
        .write(
            "package.json",
            include_bytes!("fixtures/npm-import/package.json"),
        )
        .unwrap();
    project
        .write(
            "package-lock.json",
            include_bytes!("fixtures/npm-import/package-lock.json"),
        )
        .unwrap();
    project
        .write(
            "registry.json",
            include_bytes!("fixtures/npm-import/registry.json"),
        )
        .unwrap();
    let (output, result) = invoke(&project, &["manifest", "validate", "--json"]);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["operation"], "manifest");
    assert_eq!(result["changes"]["state"], "unchanged");
    let (output, result) = invoke(
        &project,
        &["--json", "import-package-lock", "package-lock.json"],
    );
    assert!(output.status.success(), "{result}");
    assert_eq!(result["changes"]["state"], "committed");
    assert_eq!(result["changes"]["files"].as_array().unwrap().len(), 1);
    let (output, result) = invoke(
        &project,
        &["--json", "ci", "--offline", "--store-dir", "store"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["errors"][0]["code"], "LOCKFILE_INVALID");
    let store = project.path().join("store");
    let (output, result) = invoke(
        &project,
        &[
            "--json",
            "update",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{result}");
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    let (output, result) = invoke(&project, &["lock", "verify", "--json"]);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["changes"]["state"], "unchanged");
    for offline in [false, true] {
        let mut args = vec![
            "--json",
            "ci",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            "registry.json",
        ];
        if offline {
            args.push("--offline");
        }
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(result["operation"], "ci");
        assert_eq!(result["data"]["package_count"], 5);
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
    }
}

/// Keep untrusted diagnostics out of handled failures and preserve existing outputs.
#[test]
fn json_automation_failures_are_typed_and_preserve_outputs() {
    let project = TempProject::new("json-automation-failure").unwrap();
    project
        .write("package.json", b"secret invalid manifest")
        .unwrap();
    project.write("tapid.lock", b"secret invalid lock").unwrap();
    project
        .write("package-lock.json", b"secret invalid npm lock")
        .unwrap();
    for (args, code) in [
        (vec!["--json", "manifest", "validate"], "MANIFEST_INVALID"),
        (vec!["--json", "lock", "verify"], "LOCKFILE_INVALID"),
        (
            vec!["--json", "import-package-lock", "package-lock.json"],
            "MANIFEST_INVALID",
        ),
        (
            vec!["--json", "ci", "--offline", "--store-dir", "store"],
            "MANIFEST_INVALID",
        ),
    ] {
        let (output, result) = invoke(&project, &args);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(result["errors"][0]["code"], code, "{result}");
        assert_eq!(result["changes"]["state"], "unchanged");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            b"secret invalid lock"
        );
        assert!(!project.path().join("node_modules").exists());
    }
}

/// Refuse pending recovery and unsafe lock destinations without altering their contents.
#[test]
fn json_import_refuses_pending_recovery_and_nonregular_destination() {
    for pending in [false, true] {
        let project = TempProject::new("json-import-refusal").unwrap();
        project
            .write(
                "package.json",
                include_bytes!("fixtures/npm-import/package.json"),
            )
            .unwrap();
        project
            .write(
                "package-lock.json",
                include_bytes!("fixtures/npm-import/package-lock.json"),
            )
            .unwrap();
        if pending {
            project
                .write(".tapid-install-stage-test/KEEP", b"pending")
                .unwrap();
        } else {
            project
                .write("tapid.lock/KEEP", b"existing directory")
                .unwrap();
        }
        let (output, result) = invoke(
            &project,
            &["--json", "import-package-lock", "package-lock.json"],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            result["errors"][0]["code"],
            if pending {
                "RECOVERY_FAILED"
            } else {
                "LOCKFILE_INVALID"
            }
        );
        assert_eq!(
            result["retry"],
            if pending {
                "recover_first"
            } else {
                "after_correction"
            }
        );
        assert_eq!(
            result["changes"]["state"],
            if pending {
                "recovery_required"
            } else {
                "unchanged"
            }
        );
        assert!(!project.path().join("node_modules").exists());
        assert_eq!(
            fs::read(project.path().join(if pending {
                ".tapid-install-stage-test/KEEP"
            } else {
                "tapid.lock/KEEP"
            }))
            .unwrap(),
            if pending {
                b"pending".as_slice()
            } else {
                b"existing directory".as_slice()
            }
        );
    }
}

/// A failed replacement must keep the old lock even when restoring a moved backup would fail.
#[test]
fn json_import_failed_replacement_cannot_strand_the_old_lock() {
    for failure in ["replace-and-rollback", "replace"] {
        let project = TempProject::new("json-import-rename-failure").unwrap();
        let home = TempHome::new("json-import-rename-failure").unwrap();
        project
            .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
            .unwrap();
        project
            .write(
                "package-lock.json",
                br#"{"lockfileVersion":3,"packages":{"":{"name":"app","version":"1.0.0"}}}"#,
            )
            .unwrap();
        project.write("tapid.lock", b"previous lock bytes").unwrap();
        project
            .write("node_modules/KEEP", b"previous installation")
            .unwrap();
        let output = command(
            &project,
            &home,
            &["--json", "import-package-lock", "package-lock.json"],
        )
        .env("TAPID_TEST_LOCKFILE_RENAME_FAILURE", failure)
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty(), "{output:?}");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).ok(),
            Some(b"previous lock bytes".to_vec()),
            "{failure}: {result}"
        );
        assert_eq!(result["errors"][0]["code"], "TRANSACTION_FAILED");
        assert_eq!(result["changes"]["state"], "unchanged");
        assert_eq!(result["changes"]["paths"], serde_json::json!([]));
        assert_eq!(result["retry"], "after_correction");
        assert_eq!(
            fs::read(project.path().join("node_modules/KEEP")).unwrap(),
            b"previous installation"
        );
        assert!(!fs::read_dir(project.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tapid-lock-")
        }));
        let (output, result) = invoke(
            &project,
            &["--json", "import-package-lock", "package-lock.json"],
        );
        assert!(output.status.success(), "{result}");
        assert_eq!(result["changes"]["state"], "committed");
        let imported = fs::read_to_string(project.path().join("tapid.lock")).unwrap();
        assert!(tapid_lockfile::ImportedNpmLockfile::from_json(&imported).is_ok());
        assert_eq!(
            fs::read(project.path().join("node_modules/KEEP")).unwrap(),
            b"previous installation"
        );
    }
}
