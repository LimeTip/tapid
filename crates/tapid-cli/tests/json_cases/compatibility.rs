use super::{command, invoke};
use std::fs;
use tapid_test_support::{TempHome, TempProject};

#[cfg(unix)]
#[test]
fn explicit_node_runtime_is_bound_to_observed_executable() {
    use std::os::unix::fs::PermissionsExt;
    let project = TempProject::new("compat-runtime-project").unwrap();
    let runtime = TempProject::new("compat-runtime-external").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","engines":{"node":">=22"}}"#,
        )
        .unwrap();
    runtime.write("node", b"#!/bin/sh\n[ -z \"${NODE_OPTIONS+x}${NODE_PATH+x}${HOME+x}\" ] || exit 42\n[ \"$1\" = --version ] || exit 43\nprintf 'v22.3.0\\n'\n").unwrap();
    let executable = runtime.path().join("node");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let home = TempHome::new("compat-runtime-home").unwrap();
    let output = command(
        &project,
        &home,
        &[
            "install",
            "--json",
            "--node-runtime",
            executable.to_str().unwrap(),
        ],
    )
    .env("NODE_OPTIONS", "--require=untrusted")
    .env("NODE_PATH", project.path())
    .output()
    .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON result");
    assert!(output.status.success(), "{result}");
    let report = &result["data"]["compatibility"];
    assert_eq!(report["runtime"]["state"], "identified");
    assert_eq!(
        report["runtime"]["executable"],
        fs::canonicalize(executable).unwrap().to_str().unwrap()
    );
    assert_eq!(report["runtime"]["version"], "22.3.0");
    assert_eq!(report["facts"][0]["status"], "satisfied");
    assert_eq!(report["strict_satisfied"], true);
}

#[cfg(unix)]
#[test]
fn explicit_node_runtime_rejects_project_and_symlink_paths_without_execution() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let project = TempProject::new("compat-runtime-untrusted").unwrap();
    let external = TempProject::new("compat-runtime-links").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","engines":{"node":"*"}}"#,
        )
        .unwrap();
    let marker = project.path().join("executed");
    project
        .write(
            "bin/node",
            format!(
                "#!/bin/sh\n: > '{}'\nprintf 'v22.3.0\\n'\n",
                marker.display()
            )
            .as_bytes(),
        )
        .unwrap();
    fs::set_permissions(
        project.path().join("bin/node"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink(
        project.path().join("bin/node"),
        external.path().join("node"),
    )
    .unwrap();
    let external_node = external.path().join("external/node");
    external
        .write("external/node", b"#!/bin/sh\nprintf 'v22.3.0\\n'\n")
        .unwrap();
    fs::set_permissions(&external_node, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&external_node, project.path().join("node")).unwrap();
    for path in [
        project.path().join("bin/node"),
        external.path().join("node"),
        project.path().join("node"),
        std::path::PathBuf::from("node"),
    ] {
        let (output, result) = invoke(
            &project,
            &[
                "install",
                "--json",
                "--node-runtime",
                path.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{result}");
        let report = &result["data"]["compatibility"];
        assert_eq!(report["runtime"]["state"], "failed", "{result}");
        assert_eq!(report["facts"][0]["status"], "unchecked_runtime");
        assert!(!marker.exists());
    }
}

#[cfg(unix)]
#[test]
fn explicit_node_runtime_reports_bounded_probe_failures() {
    use std::os::unix::fs::PermissionsExt;
    for (body, reason) in [
        ("exit 7", "probe_unsuccessful"),
        ("printf 'v22.3.0\\nv20.0.0\\n'", "probe_malformed_version"),
        (
            "printf 'v22.3.0\\n'; printf warning >&2",
            "probe_malformed_version",
        ),
        ("large_output", "probe_output_limit"),
        ("while :; do :; done", "probe_timed_out"),
    ] {
        let project = TempProject::new("compat-probe-project").unwrap();
        let runtime = TempProject::new("compat-probe-executable").unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"app","version":"1.0.0","engines":{"node":"*"}}"#,
            )
            .unwrap();
        let body = if body == "large_output" {
            format!("printf '{}'", "x".repeat(5000))
        } else {
            body.to_owned()
        };
        runtime
            .write("node", format!("#!/bin/sh\n{body}\n").as_bytes())
            .unwrap();
        let executable = runtime.path().join("node");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let (output, result) = invoke(
            &project,
            &[
                "install",
                "--json",
                "--node-runtime",
                executable.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{result}");
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        let report = &result["data"]["compatibility"];
        assert_eq!(report["runtime"]["state"], "failed", "{result}");
        assert_eq!(report["runtime"]["reason"], reason, "{result}");
        assert_eq!(report["facts"][0]["reason"], reason);
        assert_eq!(report["strict_satisfied"], false);
    }
}

#[cfg(unix)]
#[test]
fn identify_node_skips_project_paths_and_selects_first_external_runtime() {
    use std::os::unix::fs::PermissionsExt;
    let project = TempProject::new("compat-path-project").unwrap();
    let external = TempProject::new("compat-path-external").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","engines":{"node":">=22"}}"#,
        )
        .unwrap();
    project.write("bin/node", b"#!/bin/sh\nexit 88\n").unwrap();
    external
        .write("first/node", b"#!/bin/sh\nprintf 'v20.0.0\\n'\n")
        .unwrap();
    external
        .write("second/node", b"#!/bin/sh\nprintf 'v22.0.0\\n'\n")
        .unwrap();
    for path in [
        project.path().join("bin/node"),
        external.path().join("first/node"),
        external.path().join("second/node"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let home = TempHome::new("compat-path-home").unwrap();
    let path = std::env::join_paths([
        project.path().join("bin"),
        external.path().join("first"),
        external.path().join("second"),
    ])
    .unwrap();
    let output = command(&project, &home, &["install", "--json", "--identify-node"])
        .env("PATH", path)
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{result}");
    assert_eq!(
        result["data"]["compatibility"]["runtime"]["version"],
        "20.0.0"
    );
    assert_eq!(
        result["data"]["compatibility"]["facts"][0]["status"],
        "incompatible"
    );
    let output = command(&project, &home, &["install", "--json", "--identify-node"])
        .env("PATH", "")
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{result}");
    assert_eq!(
        result["data"]["compatibility"]["runtime"]["state"],
        "not_found"
    );
    assert_eq!(
        result["data"]["compatibility"]["facts"][0]["reason"],
        "no_eligible_runtime"
    );
}

#[cfg(unix)]
#[test]
fn explicit_runtime_is_evaluated_on_ci_and_lifecycle_paths() {
    use std::os::unix::fs::PermissionsExt;
    let project = TempProject::new("compat-runtime-lifecycle").unwrap();
    let runtime = TempProject::new("compat-runtime-tool").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","engines":{"node":">=22"}}"#,
        )
        .unwrap();
    runtime
        .write("node", b"#!/bin/sh\nprintf 'v22.3.0\\n'\n")
        .unwrap();
    let node = runtime.path().join("node");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let (output, result) = invoke(&project, &["install", "--json"]);
    assert!(output.status.success(), "{result}");
    for args in [
        vec!["ci", "--offline"],
        vec!["add", "unused@1.0.0", "--peer"],
        vec!["remove", "unused"],
        vec!["update"],
    ] {
        let mut args = args;
        args.extend(["--json", "--node-runtime", node.to_str().unwrap()]);
        let (output, result) = invoke(&project, &args);
        assert!(output.status.success(), "{result}");
        assert_eq!(
            result["data"]["compatibility"]["runtime"]["version"],
            "22.3.0"
        );
        assert_eq!(
            result["data"]["compatibility"]["facts"][0]["status"],
            "satisfied"
        );
    }
}

#[test]
fn strict_compatibility_rejects_unchecked_requirements_without_activation() {
    let project = TempProject::new("compat-strict-project").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0","engines":{"node":">=22"}}"#;
    project.write("package.json", manifest).unwrap();
    let (output, result) = invoke(&project, &["install", "--json", "--strict-compat"]);
    assert!(!output.status.success());
    assert_eq!(result["errors"][0]["code"], "COMPATIBILITY_REJECTED");
    assert_eq!(
        result["data"]["compatibility"]["facts"][0]["status"],
        "unchecked_runtime"
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
}

#[cfg(unix)]
#[test]
fn explain_evaluates_only_inspected_package_against_explicit_runtime() {
    use std::os::unix::fs::PermissionsExt;
    let project = TempProject::new("compat-explain-project").unwrap();
    let runtime = TempProject::new("compat-explain-tool").unwrap();
    project.write("metadata.json", br#"{"name":"dep","versions":{"1.0.0":{"name":"dep","version":"1.0.0","engines":{"node":">=22"},"dist":{"tarball":"https://registry.npmjs.org/dep/-/dep-1.0.0.tgz"}}}}"#).unwrap();
    runtime
        .write("node", b"#!/bin/sh\nprintf 'v20.0.0\\n'\n")
        .unwrap();
    let node = runtime.path().join("node");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let (output, result) = invoke(
        &project,
        &[
            "explain",
            "dep@1.0.0",
            "--registry-metadata",
            "metadata.json",
            "--node-runtime",
            node.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{result}");
    let report = &result["data"]["compatibility"];
    assert_eq!(report["runtime"]["version"], "20.0.0");
    assert_eq!(report["facts"].as_array().unwrap().len(), 1);
    assert_eq!(report["facts"][0]["status"], "incompatible");
    assert_eq!(report["facts"][0]["provenance"], "registry_metadata");
    assert!(!project.path().join("package.json").exists());
}

#[test]
fn status_reports_local_compatibility_without_probing_or_recovery() {
    let project = TempProject::new("compat-status-project").unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","engines":{"node":">=22"},"packageManager":"npm@10.0.0"}"#).unwrap();
    let (output, result) = invoke(&project, &["install", "--json"]);
    assert!(output.status.success(), "{result}");
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    let (output, result) = invoke(&project, &["status", "--json"]);
    assert!(output.status.success(), "{result}");
    let report = &result["data"]["compatibility"];
    assert_eq!(report["runtime"]["state"], "not_requested");
    assert_eq!(report["facts"][0]["status"], "unchecked_runtime");
    assert_eq!(report["facts"][1]["status"], "unsupported");
    assert_eq!(report["strict_satisfied"], false);
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
}

#[cfg(unix)]
#[test]
fn strict_selected_requirements_reject_all_activation_paths_without_mutation() {
    use std::os::unix::fs::PermissionsExt;
    let project = TempProject::new("compat-strict-paths").unwrap();
    let runtime = TempProject::new("compat-strict-node").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0","dependencies":{"h3":"1.0.0"},"peerDependencies":{"unused":"1.0.0"}}"#;
    project.write("package.json", manifest).unwrap();
    let mut fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/npm-aliases.json")).unwrap();
    fixture["packages"][0]["engines"] = serde_json::json!({"node":">=99"});
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    runtime
        .write("node", b"#!/bin/sh\nprintf 'v22.3.0\\n'\n")
        .unwrap();
    let node = runtime.path().join("node");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let store = project.path().join("store");
    let common = [
        "--json",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        store.to_str().unwrap(),
    ];
    let mut fresh = vec![
        "install",
        "--strict-compat",
        "--node-runtime",
        node.to_str().unwrap(),
    ];
    fresh.extend(common);
    let (output, result) = invoke(&project, &fresh);
    assert!(!output.status.success(), "{result}");
    assert_eq!(result["errors"][0]["code"], "COMPATIBILITY_REJECTED");
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    let mut seed = vec!["install"];
    seed.extend(common);
    let (output, result) = invoke(&project, &seed);
    assert!(output.status.success(), "{result}");
    project.write("node_modules/keep", b"preserved").unwrap();
    let locked = fs::read(project.path().join("tapid.lock")).unwrap();
    for mut args in [
        vec!["install"],
        vec!["install", "--offline"],
        vec!["install", "--frozen"],
        vec!["ci"],
        vec!["ci", "--offline"],
        vec!["add", "other@1.0.0", "--peer"],
        vec!["remove", "unused"],
        vec!["update"],
        vec!["prune"],
    ] {
        args.extend(common);
        args.extend(["--strict-compat", "--node-runtime", node.to_str().unwrap()]);
        let (output, result) = invoke(&project, &args);
        assert!(!output.status.success(), "{args:?}: {result}");
        assert_eq!(
            result["errors"][0]["code"], "COMPATIBILITY_REJECTED",
            "{args:?}: {result}"
        );
        assert!(
            result["data"]["compatibility"]["facts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["status"] == "incompatible" && f["requirement"] == ">=99")
        );
        assert_eq!(result["lifecycle"]["hooks_executed"], 0);
        assert_eq!(result["lifecycle"]["inspected_packages"], 0);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest,
            "{args:?}"
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            locked,
            "{args:?}"
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/keep")).unwrap(),
            b"preserved"
        );
        assert!(
            !project
                .path()
                .join(".tapid-lifecycle-journal.json")
                .exists(),
            "{args:?}"
        );
    }
}

#[test]
fn strict_compatibility_passes_known_absence_without_node_and_flags_conflict() {
    let project = TempProject::new("compat-strict-absence").unwrap();
    project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let home = TempHome::new("compat-strict-absence-home").unwrap();
    let output = command(&project, &home, &["install", "--strict-compat", "--json"])
        .env("PATH", "")
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["compatibility"]["strict_satisfied"], true);
    assert_eq!(
        result["data"]["compatibility"]["runtime"]["state"],
        "not_requested"
    );
    for args in [
        vec!["install", "--identify-node", "--node-runtime", "node"],
        vec!["install", "--dry-run", "--identify-node"],
        vec!["install", "--dry-run", "--strict-compat"],
        vec!["ci", "--identify-node", "--node-runtime", "node"],
        vec![
            "explain",
            "dep@1.0.0",
            "--identify-node",
            "--node-runtime",
            "node",
        ],
        vec!["add", "dep", "--dry-run", "--node-runtime", "node"],
    ] {
        let mut args = args;
        args.push("--json");
        let (output, result) = invoke(&project, &args);
        assert!(!output.status.success(), "{args:?}: {result}");
        assert_eq!(result["errors"][0]["code"], "ARGUMENT_INVALID");
    }
}

#[cfg(unix)]
#[test]
fn runtime_evidence_preserves_native_paths_and_human_json_reasons() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    let project = TempProject::new("compat-native-runtime-project").unwrap();
    let runtime = TempProject::new("compat-native-runtime-tool").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","engines":{"node":">=99"}}"#,
        )
        .unwrap();
    #[cfg(target_os = "linux")]
    let relative = std::path::PathBuf::from(std::ffi::OsString::from_vec(
        b"node-\xff\n\x1b[31m".to_vec(),
    ))
    .join("node");
    // macOS filesystems reject invalid UTF-8 filenames. Exercise native Unicode
    // and control characters there, and the byte-preserving fallback on Linux.
    #[cfg(not(target_os = "linux"))]
    let relative = std::path::PathBuf::from("node-界\n\x1b[31m").join("node");
    runtime
        .write(&relative, b"#!/bin/sh\nprintf 'v22.3.0\\n'\n")
        .unwrap();
    let node = runtime.path().join(&relative);
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let home = TempHome::new("compat-native-runtime-home").unwrap();
    let output = command(&project, &home, &["install", "--json", "--node-runtime"])
        .arg(&node)
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{result}");
    let report = &result["data"]["compatibility"];
    let path = &report["runtime"]["executable_path"];
    let bytes = if path["encoding"] == "unix_bytes_base64" {
        STANDARD.decode(path["value"].as_str().unwrap()).unwrap()
    } else {
        assert_eq!(path["encoding"], "utf8");
        path["value"].as_str().unwrap().as_bytes().to_vec()
    };
    assert_eq!(
        bytes,
        fs::canonicalize(&node).unwrap().as_os_str().as_bytes()
    );
    assert_eq!(report["facts"][0]["reason"], "node_version_mismatch");
    assert_eq!(report["facts"][0]["runtime_reference"], "/runtime");
    let output = command(&project, &home, &["install", "--node-runtime"])
        .arg(&node)
        .output()
        .unwrap();
    assert!(output.status.success());
    let human = String::from_utf8(output.stderr).unwrap();
    assert!(human.contains("node_version_mismatch"), "{human}");
    assert!(human.contains("22.3.0"), "{human}");
    assert!(!human.contains('\u{1b}'));
}

#[test]
fn strict_compatibility_evaluates_workspace_facts_beyond_display_limit() {
    let project = TempProject::new("compat-strict-bounds").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
        )
        .unwrap();
    for index in 0..341 {
        let mut manifest =
            serde_json::json!({"name":format!("member-{index:03}"),"version":"1.0.0"});
        if index == 340 {
            manifest["devEngines"] = serde_json::Value::Null;
        }
        project
            .write(
                format!("packages/member-{index:03}/package.json"),
                manifest.to_string().as_bytes(),
            )
            .unwrap();
    }
    let (output, result) = invoke(&project, &["install", "--strict-compat", "--json"]);
    assert!(!output.status.success(), "{result}");
    assert_eq!(result["errors"][0]["code"], "COMPATIBILITY_REJECTED");
    let report = &result["data"]["compatibility"];
    assert_eq!(report["facts"].as_array().unwrap().len(), 1024);
    assert_eq!(report["omitted_facts"], 2);
    assert_eq!(report["strict_satisfied"], false);
    assert_eq!(report["details_complete"], false);
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    assert!(
        !project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
}

#[cfg(debug_assertions)]
#[test]
fn strict_rejection_recovers_prior_interrupted_activation_before_rejecting_new_work() {
    let project = TempProject::new("compat-strict-recovery").unwrap();
    let home = TempHome::new("compat-strict-recovery-home").unwrap();
    let manifest = br#"{"name":"app","version":"1.0.0","engines":{"node":">=22"}}"#;
    project.write("package.json", manifest).unwrap();
    let store = project.path().join("store");
    let (output, result) = invoke(
        &project,
        &["install", "--json", "--store-dir", store.to_str().unwrap()],
    );
    assert!(output.status.success(), "{result}");
    project
        .write("node_modules/keep", b"prior installed tree")
        .unwrap();
    let locked = fs::read(project.path().join("tapid.lock")).unwrap();
    let crashed = command(
        &project,
        &home,
        &[
            "add",
            "other@1.0.0",
            "--peer",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    )
    .env("TAPID_TEST_CRASH_POINT", "activation_complete")
    .output()
    .unwrap();
    assert!(!crashed.status.success());
    assert!(
        project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
    let (output, result) = invoke(
        &project,
        &[
            "install",
            "--offline",
            "--strict-compat",
            "--json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success(), "{result}");
    assert_eq!(result["errors"][0]["code"], "COMPATIBILITY_REJECTED");
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), locked);
    assert_eq!(
        fs::read(project.path().join("node_modules/keep")).unwrap(),
        b"prior installed tree"
    );
    assert!(
        !project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
}
