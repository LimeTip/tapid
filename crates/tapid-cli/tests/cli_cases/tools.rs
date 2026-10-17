use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tapid_test_support::{TempHome, TempProject};

// Offline npm archive: hello-tool@1.0.0 has one JS bin and a postinstall
// sentinel. Its CLI prints its arguments as JSON and exits with status 7.
const HELLO_ARCHIVE: &str = "H4sIAAAAAAAACu2TW2vCQBCFffZXpNOHJBDXjbdCpA9FFFrEQn0qCCXGMV273U13VlsR/3tJekF6oS9SS8n3MsvZAzO7w8ni5C5OsZ69VLYgrSp7hnPOO61WUTnnHytvtDrv50IPw06TVxy+70G+Ykk2NhX+K73+IBtQ8T1CBLcopa5ZrSUEsEJDQiuIIGSccQhgKhREm11XlGv1QmALgm0AlBiRWcp9mSYrFNlY5kalZ+jU0JmAwYelMOi5c3J99miExYGQOF6rxHOH54N+77o37N9cnY3cwJ3GM9efAGy3h/6l/8tr7uu7u9x3j5/yHzY/5b/dDsv8/wbHR/UlmWL9qFZOntRqohVpiUzq1LsYX44YWSNUKuZrLzM6QSIWm3TFSIoEvYbv+93q2wU+CdvL437qnHSrh35dSUlJScl3PAOJTaPbAAwAAA==";

fn isolated_process(project: &TempProject, home: &TempHome, program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .current_dir(project.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LOCALAPPDATA", home.path())
        .env("XDG_DATA_HOME", home.path())
        .env("XDG_CACHE_HOME", home.path());
    for key in ["PATH", "SystemRoot", "LLVM_PROFILE_FILE"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

fn command(project: &TempProject, home: &TempHome, args: &[&str]) -> Command {
    let mut command = isolated_process(project, home, env!("CARGO_BIN_EXE_tapid"));
    command
        .args(args)
        .arg("--tools-dir")
        .arg(home.path().join("tools with spaces"));
    command
}

fn fixture(project: &TempProject) -> std::path::PathBuf {
    let archive = STANDARD.decode(HELLO_ARCHIVE).unwrap();
    let fixture = json!({"packages": [{
        "registry": "https://registry.npmjs.org",
        "name": "hello-tool",
        "version": "1.0.0",
        "integrity": format!("sha512-{}", STANDARD.encode(Sha512::digest(&archive))),
        "artifact": format!("base64:{HELLO_ARCHIVE}")
    }]});
    project
        .write("registry.json", &serde_json::to_vec(&fixture).unwrap())
        .unwrap()
}

fn install(project: &TempProject, home: &TempHome) -> Output {
    command(project, home, &["tool", "install", "hello-tool@1.0.0"])
        .arg("--registry-fixture")
        .arg(fixture(project))
        .arg("--store-dir")
        .arg(home.path().join("store"))
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn list(project: &TempProject, home: &TempHome) -> Value {
    let output = command(project, home, &["--json", "tool", "list"])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(output.stderr.is_empty(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], 1);
    result
}

fn multi_fixture(project: &TempProject) -> std::path::PathBuf {
    project
        .write("multi.json", include_bytes!("tools-registry.json"))
        .unwrap()
}

fn install_multi(project: &TempProject, home: &TempHome, spec: &str) -> Output {
    command(project, home, &["tool", "install", spec])
        .arg("--registry-fixture")
        .arg(multi_fixture(project))
        .arg("--store-dir")
        .arg(home.path().join("store"))
        .output()
        .unwrap()
}

#[test]
fn tool_update_and_remove_preserve_other_tools_and_unowned_files() {
    let project = TempProject::new("tool-update-remove").unwrap();
    let home = TempHome::new("tool-update-remove").unwrap();
    // Install against the original one-version registry, then refresh against
    // a registry containing a newer compatible version with a renamed command.
    let first = command(&project, &home, &["tool", "install", "hello-tool@^1.0.0"])
        .arg("--registry-fixture")
        .arg(fixture(&project))
        .arg("--store-dir")
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert_success(&first);
    assert_success(&install_multi(&project, &home, "other-tool@1.0.0"));
    let before = list(&project, &home);
    let other = before["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "other-tool")
        .unwrap()
        .clone();
    let other_lock =
        fs::read(Path::new(other["environment"].as_str().unwrap()).join("tapid.lock")).unwrap();
    let bin = home.path().join("tools with spaces/bin");
    fs::write(bin.join("keep-user-file"), b"user owned").unwrap();
    let update = command(&project, &home, &["--json", "tool", "update", "hello-tool"])
        .arg("--registry-fixture")
        .arg(multi_fixture(&project))
        .arg("--store-dir")
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert_success(&update);
    let updated: Value = serde_json::from_slice(&update.stdout).unwrap();
    assert_eq!(updated["changes"]["state"], "committed");
    assert_eq!(updated["data"]["tools"][0]["version"], "1.1.0");
    assert!(
        !bin.join(if cfg!(windows) {
            "hello-tool.cmd"
        } else {
            "hello-tool"
        })
        .exists()
    );
    assert!(
        bin.join(if cfg!(windows) {
            "hello-new.cmd"
        } else {
            "hello-new"
        })
        .is_file()
    );
    assert!(
        !bin.join(if cfg!(windows) {
            "shared-internal.cmd"
        } else {
            "shared-internal"
        })
        .exists()
    );
    assert_eq!(
        fs::read(Path::new(other["environment"].as_str().unwrap()).join("tapid.lock")).unwrap(),
        other_lock
    );
    let removed = command(&project, &home, &["tool", "remove", "hello-tool"])
        .output()
        .unwrap();
    assert_success(&removed);
    let after = list(&project, &home);
    assert_eq!(after["data"]["tools"], json!([other]));
    assert_eq!(fs::read(bin.join("keep-user-file")).unwrap(), b"user owned");
    assert!(
        !bin.join(if cfg!(windows) {
            "hello-new.cmd"
        } else {
            "hello-new"
        })
        .exists()
    );
}

#[test]
fn tool_collisions_and_no_bin_fail_without_changing_existing_installation() {
    let project = TempProject::new("tool-collision").unwrap();
    let home = TempHome::new("tool-collision").unwrap();
    assert_success(&install(&project, &home));
    let before = list(&project, &home);
    for spec in ["collision-tool", "reserved-tool", "no-bin"] {
        let output = install_multi(&project, &home, spec);
        assert!(!output.status.success(), "{spec}: {output:?}");
        assert_eq!(list(&project, &home), before);
        assert_eq!(
            fs::read_dir(home.path().join("tools with spaces/environments"))
                .unwrap()
                .count(),
            1,
            "failed install must retire its owned staging environment"
        );
    }
}

#[test]
fn tool_reinstall_changes_requirement_and_keeps_one_active_environment() {
    let project = TempProject::new("tool-reinstall").unwrap();
    let home = TempHome::new("tool-reinstall").unwrap();
    assert_success(&install(&project, &home));
    assert_success(&install_multi(&project, &home, "hello-tool@^1.0.0"));
    let listed = list(&project, &home);
    assert_eq!(listed["data"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(listed["data"]["tools"][0]["version"], "1.1.0");
    assert_eq!(listed["data"]["tools"][0]["requirement"], "^1.0.0");
    assert_eq!(
        fs::read_dir(home.path().join("tools with spaces/environments"))
            .unwrap()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn tool_can_remove_a_legacy_node_launcher() {
    let project = TempProject::new("tool-legacy-node").unwrap();
    let home = TempHome::new("tool-legacy-node").unwrap();
    assert_success(&install(&project, &home));
    let root = home.path().join("tools with spaces");
    let path = root.join("catalog.json");
    let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let bin = catalog["tools"]["hello-tool"]["commands"]["hello-tool"].take();
    catalog["tools"]["hello-tool"]["commands"] = json!({"node":bin});
    let shim = catalog["shims"]["hello-tool"]
        .as_str()
        .unwrap()
        .replace("hello-tool", "node");
    catalog["shims"] = json!({"node":shim});
    fs::rename(root.join("bin/hello-tool"), root.join("bin/node")).unwrap();
    fs::write(root.join("bin/node"), shim).unwrap();
    fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    // Reject legacy runtime dispatch before PATH discovery can recurse.
    let rejected = command(&project, &home, &["__tool-exec", "--command", "node"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("tool command collides with the Node runtime")
    );
    assert_success(
        &command(&project, &home, &["tool", "remove", "hello-tool"])
            .output()
            .unwrap(),
    );
    assert!(!root.join("bin/node").exists());
    assert_eq!(list(&project, &home)["data"]["tools"], json!([]));
}

#[test]
fn tool_rejects_invalid_specs_before_creating_state() {
    let project = TempProject::new("tool-invalid").unwrap();
    let home = TempHome::new("tool-invalid").unwrap();
    for spec in [
        "../escape",
        "hello@npm:other",
        "jsr:@scope/tool",
        "hello@latest",
    ] {
        let output = install_multi(&project, &home, spec);
        assert!(!output.status.success(), "{spec}: {output:?}");
        assert!(
            !home.path().join("tools with spaces").exists(),
            "{spec}: invalid requests must not create tool state"
        );
    }
}

#[test]
fn tool_preserves_replaced_launcher_and_unknown_environment() {
    let project = TempProject::new("tool-user-replacement").unwrap();
    let home = TempHome::new("tool-user-replacement").unwrap();
    assert_success(&install(&project, &home));
    let root = home.path().join("tools with spaces");
    let shim = root.join("bin").join(if cfg!(windows) {
        "hello-tool.cmd"
    } else {
        "hello-tool"
    });
    fs::write(&shim, b"user replacement").unwrap();
    fs::create_dir(root.join("environments/user-project")).unwrap();
    fs::write(root.join("environments/user-project/keep"), b"user data").unwrap();
    let removed = command(&project, &home, &["tool", "remove", "hello-tool"])
        .output()
        .unwrap();
    assert!(!removed.status.success());
    assert_eq!(fs::read(shim).unwrap(), b"user replacement");
    assert_eq!(
        fs::read(root.join("environments/user-project/keep")).unwrap(),
        b"user data"
    );
}

#[test]
fn tool_catalog_recovery_retires_interrupted_replacement_files() {
    for point in ["lockfile_writing", "lockfile_staged", "lockfile_published"] {
        let project = TempProject::new(point).unwrap();
        let home = TempHome::new(point).unwrap();
        assert_success(&install(&project, &home));
        let root = home.path().join("tools with spaces");
        // Neither a lookalike filename nor an old unbound nonce proves ownership.
        for name in [
            ".tapid-lock-user.tmp",
            ".tapid-lock-123-456.bak",
            ".tapid-tool-catalog-user.tmp",
        ] {
            fs::write(root.join(name), b"unowned").unwrap();
        }
        let before: std::collections::BTreeSet<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let crashed = command(&project, &home, &["tool", "remove", "hello-tool"])
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(!crashed.status.success(), "{point}");
        assert!(root.join("pending.json").is_file(), "{point}");
        let interrupted_recovery = command(&project, &home, &["tool", "list"])
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(!interrupted_recovery.status.success(), "{point}");
        assert_eq!(list(&project, &home)["data"]["tools"], json!([]), "{point}");
        let after: std::collections::BTreeSet<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(after, before, "abandoned replacement file at {point}");
        for name in [
            ".tapid-lock-user.tmp",
            ".tapid-lock-123-456.bak",
            ".tapid-tool-catalog-user.tmp",
        ] {
            assert_eq!(fs::read(root.join(name)).unwrap(), b"unowned");
        }
    }
}

#[test]
fn tool_recovers_interrupted_activation_before_and_after_commit() {
    for (point, version) in [
        ("tool_launchers", "1.0.0"),
        ("tool_commit", "1.1.0"),
        ("tool_settled", "1.1.0"),
    ] {
        let project = TempProject::new(point).unwrap();
        let home = TempHome::new(point).unwrap();
        assert_success(&install(&project, &home));
        let crashed = command(&project, &home, &["tool", "install", "hello-tool@^1.0.0"])
            .arg("--registry-fixture")
            .arg(multi_fixture(&project))
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(!crashed.status.success());
        let recovered = list(&project, &home);
        assert_eq!(recovered["data"]["tools"][0]["version"], version, "{point}");
        assert!(!home.path().join("tools with spaces/pending.json").exists());
        assert_eq!(
            fs::read_dir(home.path().join("tools with spaces/environments"))
                .unwrap()
                .count(),
            1
        );
    }
}

#[test]
fn tool_cleanup_defers_an_environment_with_a_live_invocation_lease() {
    use fs4::FileExt;
    let project = TempProject::new("tool-live-lease").unwrap();
    let home = TempHome::new("tool-live-lease").unwrap();
    assert_success(&install(&project, &home));
    let listed = list(&project, &home);
    let environment = Path::new(listed["data"]["tools"][0]["environment"].as_str().unwrap());
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(environment.join(".in-use"))
        .unwrap();
    FileExt::try_lock_shared(&lease).unwrap();
    let removed = command(&project, &home, &["--json", "tool", "remove", "hello-tool"])
        .output()
        .unwrap();
    assert_success(&removed);
    let result: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(result["changes"]["state"], "committed_cleanup_pending");
    assert!(environment.exists());
    assert_eq!(
        list(&project, &home)["changes"]["state"],
        "committed_cleanup_pending"
    );
    drop(lease);
    let cleaned = list(&project, &home);
    assert_eq!(cleaned["data"]["tools"], json!([]));
    assert_eq!(
        cleaned["changes"]["state"], "committed",
        "listing must report the cleanup it performed"
    );
    assert!(!environment.exists());
}

#[test]
fn tool_scoped_package_exports_its_declared_command() {
    let project = TempProject::new("tool-scoped").unwrap();
    let home = TempHome::new("tool-scoped").unwrap();
    assert_success(&install_multi(&project, &home, "@scope/tool@1.0.0"));
    let listed = list(&project, &home);
    assert_eq!(listed["data"]["tools"][0]["name"], "@scope/tool");
    assert_eq!(
        listed["data"]["tools"][0]["commands"][0]["name"],
        "scoped-command"
    );
}

#[test]
fn tool_mutation_rejects_drifted_bins_without_changing_published_state() {
    for drift in ["added", "removed", "retargeted", "missing", "interpreter"] {
        let project = TempProject::new("tool-bin-drift").unwrap();
        let home = TempHome::new("tool-bin-drift").unwrap();
        assert_success(&install(&project, &home));
        let root = home.path().join("tools with spaces");
        let before = list(&project, &home);
        let environment = Path::new(before["data"]["tools"][0]["environment"].as_str().unwrap());
        let package = environment.join("node_modules/hello-tool");
        let manifest_path = package.join("package.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        match drift {
            "added" => {
                manifest["bin"] =
                    json!({"hello-tool":"bin/hello.js", "shadow-command":"bin/hello.js"})
            }
            "removed" => {
                manifest.as_object_mut().unwrap().remove("bin");
            }
            "retargeted" => {
                fs::copy(package.join("bin/hello.js"), package.join("bin/other.js")).unwrap();
                manifest["bin"] = json!({"hello-tool":"bin/other.js"});
            }
            "missing" => fs::remove_file(package.join("bin/hello.js")).unwrap(),
            "interpreter" => {
                fs::write(package.join("bin/hello.js"), b"MZchanged interpreter").unwrap()
            }
            _ => unreachable!(),
        }
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let catalog = fs::read(root.join("catalog.json")).unwrap();
        let shims: std::collections::BTreeMap<_, _> = fs::read_dir(root.join("bin"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect();
        let output = install_multi(&project, &home, "other-tool@1.0.0");
        assert!(
            !output.status.success(),
            "accepted {drift} bin drift: {output:?}"
        );
        assert_eq!(
            fs::read(root.join("catalog.json")).unwrap(),
            catalog,
            "{drift}"
        );
        let after_shims: std::collections::BTreeMap<_, _> = fs::read_dir(root.join("bin"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect();
        assert_eq!(after_shims, shims, "{drift}");
        assert_eq!(
            list(&project, &home)["data"]["tools"],
            before["data"]["tools"]
        );
        // Removing the affected tool remains a way to recover from local drift.
        assert_success(
            &command(&project, &home, &["tool", "remove", "hello-tool"])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn tool_corrupt_catalog_and_unowned_command_are_preserved() {
    let project = TempProject::new("tool-invalid-state").unwrap();
    let home = TempHome::new("tool-invalid-state").unwrap();
    assert_success(&install(&project, &home));
    let root = home.path().join("tools with spaces");
    let catalog = fs::read(root.join("catalog.json")).unwrap();
    fs::write(root.join("bin/other-command.cmd"), b"unowned cmd").unwrap();
    fs::write(root.join("bin/other-command"), b"unowned unix").unwrap();
    let output = install_multi(&project, &home, "other-tool");
    assert!(!output.status.success());
    assert_eq!(fs::read(root.join("catalog.json")).unwrap(), catalog);
    assert_eq!(
        fs::read(root.join("bin/other-command.cmd")).unwrap(),
        b"unowned cmd"
    );
    fs::write(root.join("catalog.json"), b"malformed state").unwrap();
    let output = command(&project, &home, &["--json", "tool", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(root.join("catalog.json")).unwrap(),
        b"malformed state"
    );
}

#[cfg(windows)]
#[test]
fn tool_windows_case_and_extension_collisions_are_rejected() {
    let project = TempProject::new("tool-windows-collision").unwrap();
    let home = TempHome::new("tool-windows-collision").unwrap();
    assert_success(&install(&project, &home));
    let before = list(&project, &home);
    for spec in ["case-tool", "extension-tool"] {
        assert!(!install_multi(&project, &home, spec).status.success());
        assert_eq!(list(&project, &home), before);
    }
}

#[cfg(windows)]
#[test]
fn tool_windows_cmd_shim_runs_node_with_spaces_and_preserves_exit_status() {
    use std::os::windows::process::CommandExt;
    let project = TempProject::new("tool-cmd").unwrap();
    let home = TempHome::new("tool-cmd").unwrap();
    assert_success(&install(&project, &home));
    let shim = home.path().join("tools with spaces/bin/hello-tool.cmd");
    let output = isolated_process(&project, &home, "cmd.exe")
        .args(["/d", "/s", "/c"])
        .raw_arg(format!("\"\"{}\" \"two words\" --flag\"", shim.display()))
        .current_dir(project.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!(["two words", "--flag"])
    );
}

#[cfg(windows)]
#[test]
fn tool_powershell_shim_preserves_empty_arguments() {
    let project = TempProject::new("tool-powershell-empty").unwrap();
    let home = TempHome::new("tool-powershell-empty").unwrap();
    assert_success(&install(&project, &home));
    let shim = home.path().join("tools with spaces/bin/hello-tool.ps1");
    let script = format!(
        "& '{}' 'two words' '' '--flag'; exit $LASTEXITCODE",
        shim.display().to_string().replace('\'', "''")
    );
    let output = isolated_process(&project, &home, "powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(script)
        .current_dir(project.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!(["two words", "", "--flag"])
    );
}

#[test]
fn tool_failed_install_remains_retryable_with_earlier_cleanup_pending() {
    use fs4::FileExt;
    let project = TempProject::new("tool-failed-with-cleanup").unwrap();
    let home = TempHome::new("tool-failed-with-cleanup").unwrap();
    assert_success(&install(&project, &home));
    let listed = list(&project, &home);
    let environment = Path::new(listed["data"]["tools"][0]["environment"].as_str().unwrap());
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(environment.join(".in-use"))
        .unwrap();
    FileExt::try_lock_shared(&lease).unwrap();
    assert_success(
        &command(&project, &home, &["tool", "remove", "hello-tool"])
            .output()
            .unwrap(),
    );
    let output = command(
        &project,
        &home,
        &["--json", "tool", "install", "hello-tool@9.0.0"],
    )
    .arg("--registry-fixture")
    .arg(fixture(&project))
    .arg("--store-dir")
    .arg(home.path().join("store"))
    .output()
    .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["changes"]["state"], "unchanged");
    assert_eq!(result["retry"], "after_correction");
    assert!(environment.exists());
}

#[test]
fn tool_json_truncation_pointers_are_sorted() {
    let project = TempProject::new("tool-json-truncation").unwrap();
    let home = TempHome::new("tool-json-truncation").unwrap();
    assert_success(&install(&project, &home));
    let path = home.path().join("tools with spaces/catalog.json");
    let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let long_version = format!("1.0.0-{}", "x".repeat(5000));
    catalog["tools"]["hello-tool"]["version"] = json!(long_version);
    catalog["tools"]["hello-tool"]["requirement"] = json!(format!(">={long_version}"));
    fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let result = list(&project, &home);
    assert_eq!(
        result["truncated_fields"],
        json!(["/data/tools/0/requirement", "/data/tools/0/version"])
    );
}

#[test]
fn tool_invocation_runs_while_a_mutation_holds_the_staging_lock() {
    use fs4::FileExt;
    let project = TempProject::new("tool-exec-during-stage").unwrap();
    let home = TempHome::new("tool-exec-during-stage").unwrap();
    assert_success(&install(&project, &home));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.path().join("tools with spaces/.tapid-activation.lock"))
        .unwrap();
    FileExt::try_lock(&lock).unwrap();
    let output = command(&project, &home, &["__tool-exec", "--command", "hello-tool"])
        .args(["--", "during-stage"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!(["during-stage"])
    );
}

#[test]
fn tool_invocation_waits_for_catalog_publication() {
    use fs4::FileExt;
    use std::{process::Stdio, time::Duration};
    let project = TempProject::new("tool-exec-waits").unwrap();
    let home = TempHome::new("tool-exec-waits").unwrap();
    assert_success(&install(&project, &home));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            home.path()
                .join("tools with spaces/.tapid-tool-catalog.lock"),
        )
        .unwrap();
    FileExt::try_lock(&lock).unwrap();
    let mut child = command(&project, &home, &["__tool-exec", "--command", "hello-tool"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let early_exit = child.try_wait().unwrap();
    drop(lock);
    let output = child.wait_with_output().unwrap();
    assert!(
        early_exit.is_none(),
        "invocation must wait for publication: {output:?}"
    );
    assert_eq!(output.status.code(), Some(7), "{output:?}");
}

#[test]
fn tool_mutations_fail_when_another_process_holds_the_staging_lock() {
    use fs4::FileExt;
    let project = TempProject::new("tool-lock").unwrap();
    let home = TempHome::new("tool-lock").unwrap();
    assert_success(&install(&project, &home));
    let path = home.path().join("tools with spaces/.tapid-activation.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    FileExt::try_lock(&lock).unwrap();
    let output = command(&project, &home, &["--json", "tool", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["code"], "PROJECT_BUSY");
    assert_eq!(result["changes"]["state"], "unchanged");
}

#[test]
fn tool_rejects_catalog_launcher_paths_that_are_not_single_normal_components() {
    let project = TempProject::new("tool-catalog-escape").unwrap();
    let home = TempHome::new("tool-catalog-escape").unwrap();
    assert_success(&install(&project, &home));
    let path = home.path().join("tools with spaces/catalog.json");
    let before = fs::read(&path).unwrap();
    for name in ["/", "..", "../outside"] {
        let mut state: Value = serde_json::from_slice(&before).unwrap();
        state["shims"] = json!({name:"untrusted"});
        let encoded = serde_json::to_vec(&state).unwrap();
        fs::write(&path, &encoded).unwrap();
        let output = command(&project, &home, &["tool", "list"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{name}: {output:?}");
        assert_eq!(fs::read(&path).unwrap(), encoded);
    }
}

#[cfg(windows)]
#[test]
fn tool_reparse_root_is_rejected_and_cleanup_never_traverses_a_junction() {
    let project = TempProject::new("tool-junction").unwrap();
    let home = TempHome::new("tool-junction").unwrap();
    let outside = TempProject::new("tool-junction-outside").unwrap();
    outside.write("keep", b"external data").unwrap();
    assert_success(&install(&project, &home));
    let listed = list(&project, &home);
    let environment = Path::new(listed["data"]["tools"][0]["environment"].as_str().unwrap());
    let junction = environment.join("outside-link");
    let created = isolated_process(&project, &home, "cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(outside.path())
        .output()
        .unwrap();
    assert_success(&created);
    assert_success(
        &command(&project, &home, &["tool", "remove", "hello-tool"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(outside.path().join("keep")).unwrap(),
        b"external data"
    );
    let root = home.path().join("tools with spaces");
    fs::rename(&root, home.path().join("original-tools")).unwrap();
    let created = isolated_process(&project, &home, "cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&root)
        .arg(outside.path())
        .output()
        .unwrap();
    assert_success(&created);
    assert!(
        !command(&project, &home, &["tool", "list"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(outside.path().join("keep")).unwrap(),
        b"external data"
    );
    fs::remove_dir(root).unwrap();
}

#[cfg(unix)]
#[test]
fn tool_symlink_root_is_rejected_and_cleanup_never_traverses_a_symlink() {
    let project = TempProject::new("tool-symlink").unwrap();
    let home = TempHome::new("tool-symlink").unwrap();
    let outside = TempProject::new("tool-symlink-outside").unwrap();
    outside.write("keep", b"external data").unwrap();
    assert_success(&install(&project, &home));
    let listed = list(&project, &home);
    let environment = Path::new(listed["data"]["tools"][0]["environment"].as_str().unwrap());
    std::os::unix::fs::symlink(outside.path(), environment.join("outside-link")).unwrap();
    assert_success(
        &command(&project, &home, &["tool", "remove", "hello-tool"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(outside.path().join("keep")).unwrap(),
        b"external data"
    );
    let root = home.path().join("tools with spaces");
    fs::rename(&root, home.path().join("original-tools")).unwrap();
    std::os::unix::fs::symlink(outside.path(), &root).unwrap();
    assert!(
        !command(&project, &home, &["tool", "list"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(outside.path().join("keep")).unwrap(),
        b"external data"
    );
}

#[test]
fn tool_requires_an_absolute_tools_directory_without_creating_it() {
    let project = TempProject::new("tool-relative-root").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .env_clear()
        .current_dir(project.path())
        .args(["tool", "list", "--tools-dir", "relative-root"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!project.path().join("relative-root").exists());
}

#[test]
fn tool_catalog_rejects_invalid_versions_and_requirements() {
    let project = TempProject::new("tool-catalog-identity").unwrap();
    let home = TempHome::new("tool-catalog-identity").unwrap();
    assert_success(&install(&project, &home));
    let path = home.path().join("tools with spaces/catalog.json");
    let before = fs::read(&path).unwrap();
    for (field, value) in [
        ("version", "not-semver"),
        ("requirement", "npm:other@1"),
        ("requirement", "latest"),
    ] {
        let mut catalog: Value = serde_json::from_slice(&before).unwrap();
        catalog["tools"]["hello-tool"][field] = json!(value);
        fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let output = command(&project, &home, &["tool", "list"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{field}: {output:?}");
    }
}

#[test]
fn tool_rejects_integrity_valid_archives_with_a_different_locked_identity() {
    for case in ["version", "name"] {
        let project = TempProject::new("tool-archive-identity").unwrap();
        let home = TempHome::new("tool-archive-identity").unwrap();
        assert_success(&install(&project, &home));
        let root = home.path().join("tools with spaces");
        let before = list(&project, &home);
        let catalog = fs::read(root.join("catalog.json")).unwrap();
        let mut registry: Value =
            serde_json::from_slice(&fs::read(fixture(&project)).unwrap()).unwrap();
        // The archive and its SHA-512 remain valid hello-tool@1.0.0 bytes.
        let spec = if case == "version" {
            registry["packages"][0]["version"] = json!("1.1.0");
            "hello-tool@1.1.0"
        } else {
            registry["packages"][0]["name"] = json!("wrong-name");
            "wrong-name@1.0.0"
        };
        let registry = project
            .write(
                "mismatched-identity.json",
                &serde_json::to_vec(&registry).unwrap(),
            )
            .unwrap();
        let output = command(&project, &home, &["tool", "install", spec])
            .arg("--registry-fixture")
            .arg(registry)
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "accepted {case} mismatch: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("identity"),
            "{case}: {output:?}"
        );
        assert_eq!(fs::read(root.join("catalog.json")).unwrap(), catalog);
        assert_eq!(
            list(&project, &home)["data"]["tools"],
            before["data"]["tools"]
        );
        let invoked = command(&project, &home, &["__tool-exec", "--command", "hello-tool"])
            .output()
            .unwrap();
        assert_eq!(invoked.status.code(), Some(7), "{case}: {invoked:?}");
    }
}

#[test]
fn tool_failed_integrity_verification_preserves_the_previous_installation() {
    let project = TempProject::new("tool-integrity").unwrap();
    let home = TempHome::new("tool-integrity").unwrap();
    assert_success(&install(&project, &home));
    let root = home.path().join("tools with spaces");
    let catalog = fs::read(root.join("catalog.json")).unwrap();
    let mut fixture: Value = serde_json::from_slice(include_bytes!("tools-registry.json")).unwrap();
    for package in fixture["packages"].as_array_mut().unwrap() {
        if package["name"] == "hello-tool" && package["version"] == "1.1.0" {
            package["integrity"] = json!(format!("sha512-{}", STANDARD.encode([0u8; 64])));
        }
    }
    let fixture = project
        .write("bad-integrity.json", &serde_json::to_vec(&fixture).unwrap())
        .unwrap();
    let output = command(
        &project,
        &home,
        &["--json", "tool", "install", "hello-tool@^1"],
    )
    .arg("--registry-fixture")
    .arg(fixture)
    .arg("--store-dir")
    .arg(home.path().join("store"))
    .output()
    .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["code"], "INTEGRITY_MISMATCH");
    assert_eq!(fs::read(root.join("catalog.json")).unwrap(), catalog);
    assert_eq!(fs::read_dir(root.join("environments")).unwrap().count(), 1);
}

#[test]
fn tool_human_list_exposes_its_requirement_and_inspectable_lock_path() {
    let project = TempProject::new("tool-human-list").unwrap();
    let home = TempHome::new("tool-human-list").unwrap();
    assert_success(&install(&project, &home));
    let output = command(&project, &home, &["tool", "list"])
        .output()
        .unwrap();
    assert_success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("requirement=1.0.0"), "{text}");
    assert!(text.contains("tapid.lock"), "{text}");
}

#[test]
fn tool_list_without_installations_does_not_create_state() {
    let project = TempProject::new("tool-empty-list").unwrap();
    let home = TempHome::new("tool-empty-list").unwrap();
    project
        .write("package.json", b"invalid caller manifest")
        .unwrap();
    project
        .write("tapid.toml", b"invalid caller policy")
        .unwrap();

    let result = list(&project, &home);

    assert_eq!(result["data"]["tools"], json!([]));
    assert_eq!(result["changes"]["state"], "unchanged");
    assert!(!home.path().join("tools with spaces").exists());
    assert!(!project.path().join("tapid.lock").exists());
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        b"invalid caller manifest"
    );
}

#[test]
fn tool_install_creates_locked_environment_without_touching_the_caller() {
    let project = TempProject::new("tool-install").unwrap();
    let home = TempHome::new("tool-install").unwrap();
    project
        .write("tapid.toml", b"invalid caller policy must not be loaded")
        .unwrap();

    let output = install(&project, &home);
    assert_success(&output);

    assert!(!project.path().join("package.json").exists());
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join("node_modules").exists());
    let result = list(&project, &home);
    let tools = result["data"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "hello-tool");
    assert_eq!(tools[0]["version"], "1.0.0");
    assert_eq!(tools[0]["requirement"], "1.0.0");
    let environment = Path::new(tools[0]["environment"].as_str().unwrap());
    assert!(
        fs::canonicalize(environment)
            .unwrap()
            .starts_with(fs::canonicalize(home.path()).unwrap())
    );
    assert!(environment.join("package.json").is_file());
    assert!(environment.join("tapid.lock").is_file());
    assert!(
        environment
            .join("node_modules/hello-tool/bin/hello.js")
            .is_file()
    );
    assert!(!environment.join("LIFECYCLE_RAN").exists());
    assert!(
        !environment
            .join("node_modules/hello-tool/LIFECYCLE_RAN")
            .exists()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("skipped dependency lifecycle hook postinstall")
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PATH"));
}

#[test]
fn tool_platform_migration_can_replace_multiple_legacy_tools_one_at_a_time() {
    let project = TempProject::new("tool-platform-migration").unwrap();
    let home = TempHome::new("tool-platform-migration").unwrap();
    assert_success(&install(&project, &home));
    assert_success(&install_multi(&project, &home, "other-tool@1.0.0"));
    let path = home.path().join("tools with spaces/catalog.json");
    let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for tool in catalog["tools"].as_object_mut().unwrap().values_mut() {
        tool.as_object_mut()
            .unwrap()
            .remove("installation_platform");
    }
    fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    assert_success(&install(&project, &home));
    assert_success(
        &command(&project, &home, &["tool", "remove", "other-tool"])
            .output()
            .unwrap(),
    );
}

#[test]
fn tool_execution_rejects_foreign_or_unknown_installation_platforms() {
    for dimension in ["missing", "os", "cpu", "libc"] {
        let project = TempProject::new(dimension).unwrap();
        let home = TempHome::new(dimension).unwrap();
        assert_success(&install(&project, &home));
        let path = home.path().join("tools with spaces/catalog.json");
        let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let tool = &mut catalog["tools"]["hello-tool"];
        if dimension == "missing" {
            tool.as_object_mut()
                .unwrap()
                .remove("installation_platform");
        } else {
            let mut parts: Vec<_> = tool["installation_platform"]
                .as_str()
                .unwrap_or("legacy-host-")
                .split('-')
                .map(str::to_owned)
                .collect();
            parts[match dimension {
                "os" => 0,
                "cpu" => 1,
                _ => 2,
            }] = "foreign".to_owned();
            tool["installation_platform"] = json!(parts.join("-"));
        }
        fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let output = command(&project, &home, &["__tool-exec", "--command", "hello-tool"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{dimension}: {output:?}");
        assert!(output.stdout.is_empty(), "{dimension}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("update or reinstall"),
            "{dimension}: {output:?}"
        );
        // Legacy/foreign records remain inspectable and reinstallable.
        assert_eq!(
            list(&project, &home)["data"]["tools"][0]["name"],
            "hello-tool"
        );
        assert_success(&install(&project, &home));
        let output = command(&project, &home, &["__tool-exec", "--command", "hello-tool"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "{dimension}: {output:?}");
    }
}

#[test]
fn tool_command_forwards_arguments_and_exit_status() {
    let project = TempProject::new("tool-execution").unwrap();
    let home = TempHome::new("tool-execution").unwrap();
    assert_success(&install(&project, &home));
    let bin = home.path().join("tools with spaces/bin");

    #[cfg(windows)]
    let output = isolated_process(&project, &home, "powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(bin.join("hello-tool.ps1"))
        .args(["two words", "--tool-argument"])
        .current_dir(project.path())
        .output()
        .unwrap();

    #[cfg(not(windows))]
    let output = Command::new(bin.join("hello-tool"))
        .args(["two words", "", "--tool-argument"])
        .current_dir(project.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let arguments: Value = serde_json::from_slice(&output.stdout).unwrap();
    #[cfg(windows)]
    assert_eq!(arguments, json!(["two words", "--tool-argument"]));
    #[cfg(not(windows))]
    assert_eq!(arguments, json!(["two words", "", "--tool-argument"]));
}

#[test]
fn tool_root_can_resolve_a_platform_alias_in_its_parent_directory() {
    let project = TempProject::new("tool-parent-alias").unwrap();
    let home = TempHome::new("tool-parent-alias").unwrap();
    let destination = TempProject::new("tool-parent-destination").unwrap();
    let alias = home.path().join("parent-alias");
    #[cfg(windows)]
    {
        let result = isolated_process(&project, &home, "cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&alias)
            .arg(destination.path())
            .output()
            .unwrap();
        assert_success(&result);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(destination.path(), &alias).unwrap();
    let output = isolated_process(&project, &home, env!("CARGO_BIN_EXE_tapid"))
        .args(["--json", "tool", "list", "--tools-dir"])
        .arg(alias.join("tools"))
        .output()
        .unwrap();
    assert_success(&output);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["tools"], json!([]));
    assert!(!destination.path().join("tools").exists());
    #[cfg(windows)]
    fs::remove_dir(alias).unwrap();
}

fn writing_crash_case(point: &str) {
    let project = TempProject::new(point).unwrap();
    let home = TempHome::new(point).unwrap();
    assert_success(&install(&project, &home));
    let root = home.path().join("tools with spaces");
    fs::write(root.join(".tapid-tool-write-deadbeef"), b"unowned").unwrap();
    fs::write(root.join("bin/.tapid-tool-write-deadbeef"), b"unowned").unwrap();
    let before: Vec<std::collections::BTreeSet<_>> = [&root, &root.join("bin")]
        .into_iter()
        .map(|path| {
            fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect()
        })
        .collect();
    let crashed = command(&project, &home, &["tool", "install", "hello-tool@^1"])
        .arg("--registry-fixture")
        .arg(multi_fixture(&project))
        .arg("--store-dir")
        .arg(home.path().join("store"))
        .env("TAPID_TEST_CRASH_POINT", point)
        .output()
        .unwrap();
    assert!(!crashed.status.success());
    let recovered = list(&project, &home);
    assert_eq!(recovered["data"]["tools"][0]["version"], "1.0.0");
    assert!(!home.path().join("tools with spaces/pending.json").exists());
    let after: Vec<std::collections::BTreeSet<_>> = [&root, &root.join("bin")]
        .into_iter()
        .map(|path| {
            fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect()
        })
        .collect();
    assert_eq!(after, before, "abandoned generic write at {point}");
    assert_eq!(
        fs::read(root.join(".tapid-tool-write-deadbeef")).unwrap(),
        b"unowned"
    );
    assert_eq!(
        fs::read(root.join("bin/.tapid-tool-write-deadbeef")).unwrap(),
        b"unowned"
    );
}

#[test]
fn tool_reclaims_interrupted_stages_without_claiming_unknown_generations() {
    for point in [
        "tool_stage_intent",
        "tool_environment_created",
        "tool_writing_environment_owner",
        "tool_environment_owner_renamed",
        "tool_environment_owned",
    ] {
        let project = TempProject::new(point).unwrap();
        let home = TempHome::new(point).unwrap();
        assert_success(&install(&project, &home));
        let root = home.path().join("tools with spaces");
        let unknown = root.join("environments/deadbeef");
        fs::create_dir(&unknown).unwrap();
        fs::write(unknown.join("user-file"), b"user file").unwrap();
        let before = list(&project, &home)["data"]["tools"].clone();
        let crashed = command(&project, &home, &["tool", "update", "hello-tool"])
            .arg("--registry-fixture")
            .arg(fixture(&project))
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(!crashed.status.success());
        assert_eq!(list(&project, &home)["data"]["tools"], before);
        assert_eq!(
            fs::read_dir(root.join("environments")).unwrap().count(),
            2,
            "{point}"
        );
        assert_eq!(fs::read(unknown.join("user-file")).unwrap(), b"user file");
    }
}

#[test]
fn tool_stage_recovery_never_follows_an_ownership_sidecar_link() {
    let project = TempProject::new("tool-stage-link").unwrap();
    let home = TempHome::new("tool-stage-link").unwrap();
    let outside = TempProject::new("tool-stage-link-outside").unwrap();
    assert_success(&install(&project, &home));
    let sentinel = outside.write("user-file", b"user file").unwrap();
    let root = home.path().join("tools with spaces");
    let marker = root.join("environments/.tapid-tool-staging-v1-deadbeef");
    #[cfg(windows)]
    assert_success(
        &isolated_process(&project, &home, "cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&marker)
            .arg(outside.path())
            .output()
            .unwrap(),
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &marker).unwrap();
    let output = command(&project, &home, &["tool", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(sentinel).unwrap(), b"user file");
    assert!(!root.join("environments/deadbeef").exists());
    #[cfg(windows)]
    fs::remove_dir(marker).unwrap();
}

#[cfg(unix)]
#[test]
fn tool_direct_unix_bin_from_a_non_executable_archive_runs() {
    use std::os::unix::fs::PermissionsExt;
    // hello-tool's shell bin has mode 0644 in this verified tar archive.
    const ARCHIVE: &str = "H4sIAAAAAAAACu2TTUvDQBCGc86viOMhLaTJRGMKuUlpQSge9CQUZJtM7da4G3e3flDy3yWpH0UEL6EV2ecyy7svzDDDW7H8nt1RVG1ruNJSOB2DiJgmSVsR8XvFkyT9fLd6HKen6HjY9SA/sdaGKQf30usPsgHBHggyWFJZyoGRsoQAnkhpLgVkEIcYIgQw5wKyza4ra7SoFcKVhjoAnSteGd34KqkNF9qwsjEKWZA3IG8Gih7XXFHPX2i/Hz4rbmjCS7p+FXnPn15MxqOb0XR8e3V+6Qf+nBV+fwZQ14fe0v/lPffR7i277vFb/vHr7yP/Z8nQ5n8fHB+1p9dLt1JcmIVXcEW5aSPu0gs33tA99IwWi8Vi6Z43SATa4QAMAAA=";
    let project = TempProject::new("tool-direct-shell").unwrap();
    let home = TempHome::new("tool-direct-shell").unwrap();
    let archive = STANDARD.decode(ARCHIVE).unwrap();
    let registry = json!({"packages":[{
        "registry":"https://registry.npmjs.org", "name":"hello-tool", "version":"1.0.0",
        "integrity":format!("sha512-{}", STANDARD.encode(Sha512::digest(&archive))),
        "artifact":format!("base64:{ARCHIVE}")
    }]});
    let registry = project
        .write(
            "shell-registry.json",
            &serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();
    let installed = command(&project, &home, &["tool", "install", "hello-tool@1.0.0"])
        .arg("--registry-fixture")
        .arg(registry)
        .arg("--store-dir")
        .arg(home.path().join("store"))
        .output()
        .unwrap();
    assert_success(&installed);
    let listed = list(&project, &home);
    let environment = Path::new(listed["data"]["tools"][0]["environment"].as_str().unwrap());
    assert_ne!(
        fs::metadata(environment.join("node_modules/hello-tool/bin/hello.js"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    let output = command(&project, &home, &["__tool-exec", "--command", "hello-tool"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(output.stdout, b"direct-tool");
}

#[test]
fn tool_retries_interrupted_ownership_publication_without_claiming_unowned_files() {
    for (point, published) in [
        ("tool_initializing_owner", false),
        ("tool_writing_owner", false),
        ("tool_owner_synced", false),
        ("tool_owner_published", true),
    ] {
        let project = TempProject::new(point).unwrap();
        let home = TempHome::new(point).unwrap();
        let crashed = command(&project, &home, &["tool", "install", "hello-tool@1.0.0"])
            .arg("--registry-fixture")
            .arg(fixture(&project))
            .arg("--store-dir")
            .arg(home.path().join("store"))
            .env("TAPID_TEST_CRASH_POINT", point)
            .output()
            .unwrap();
        assert!(!crashed.status.success());
        assert_eq!(
            home.path().join("tools with spaces/.tapid-tools").exists(),
            published
        );
        let recovered = list(&project, &home);
        assert_eq!(recovered["data"]["tools"], json!([]));
        assert!(home.path().join("tools with spaces/bin").is_dir());
        assert!(home.path().join("tools with spaces/environments").is_dir());
        assert_success(&install(&project, &home));
        assert_eq!(
            list(&project, &home)["data"]["tools"][0]["name"],
            "hello-tool"
        );
        assert!(
            !home
                .path()
                .join("tools with spaces/.tapid-tools-initializing-v1")
                .exists()
        );
    }
    let project = TempProject::new("tool-unowned-initialization").unwrap();
    let home = TempHome::new("tool-unowned-initialization").unwrap();
    let root = home.path().join("tools with spaces");
    fs::create_dir(&root).unwrap();
    let unowned = root.join(".tapid-tool-write-abc");
    fs::write(&unowned, b"tapid-tools-v1\n").unwrap();
    let output = install(&project, &home);
    assert!(!output.status.success());
    assert_eq!(fs::read(&unowned).unwrap(), b"tapid-tools-v1\n");
    assert!(!root.join(".tapid-tools").exists());
}

#[test]
fn tool_list_preserves_an_empty_unowned_root() {
    let project = TempProject::new("tool-list-unowned").unwrap();
    let home = TempHome::new("tool-list-unowned").unwrap();
    let root = home.path().join("tools with spaces");
    fs::create_dir(&root).unwrap();
    let output = command(&project, &home, &["tool", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn tool_initialization_rejects_invalid_ownership_and_preserves_unknown_entries() {
    for case in [
        "file",
        "nonempty",
        "unknown",
        "invalid-temp",
        "directory-temp",
    ] {
        let project = TempProject::new(case).unwrap();
        let home = TempHome::new(case).unwrap();
        let root = home.path().join("tools with spaces");
        let marker = root.join(".tapid-tools-initializing-v1");
        fs::create_dir(&root).unwrap();
        if case == "file" {
            fs::write(&marker, b"user file").unwrap();
        } else {
            fs::create_dir(&marker).unwrap();
        }
        let preserved = match case {
            "nonempty" => Some(marker.join("user-file")),
            "unknown" => Some(root.join("user-file")),
            "invalid-temp" => Some(root.join(".tapid-tool-write-not-hex")),
            _ => None,
        };
        if let Some(path) = &preserved {
            fs::write(path, b"user file").unwrap();
        }
        if case == "directory-temp" {
            fs::create_dir(root.join(".tapid-tool-write-abc")).unwrap();
        }
        let listed = command(&project, &home, &["tool", "list"])
            .output()
            .unwrap();
        assert!(!listed.status.success(), "{case}");
        assert!(!install(&project, &home).status.success(), "{case}");
        assert!(!root.join(".tapid-tools").exists());
        assert!(marker.exists());
        if let Some(path) = preserved {
            assert_eq!(fs::read(path).unwrap(), b"user file");
        }
    }
    let project = TempProject::new("tool-init-link").unwrap();
    let home = TempHome::new("tool-init-link").unwrap();
    let outside = TempProject::new("tool-init-outside").unwrap();
    let sentinel = outside.write("user-file", b"user file").unwrap();
    let root = home.path().join("tools with spaces");
    fs::create_dir(&root).unwrap();
    let marker = root.join(".tapid-tools-initializing-v1");
    #[cfg(windows)]
    assert_success(
        &isolated_process(&project, &home, "cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&marker)
            .arg(outside.path())
            .output()
            .unwrap(),
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &marker).unwrap();
    assert!(!install(&project, &home).status.success());
    assert_eq!(fs::read(sentinel).unwrap(), b"user file");
    assert!(!root.join(".tapid-tools").exists());
    #[cfg(windows)]
    fs::remove_dir(marker).unwrap();
}

#[test]
fn tool_write_recovery_preserves_unknown_contents_and_links() {
    for case in ["unknown", "directory", "file", "link"] {
        let project = TempProject::new(case).unwrap();
        let home = TempHome::new(case).unwrap();
        let outside = TempProject::new("tool-write-outside").unwrap();
        let sentinel = outside.write("sentinel", b"user data").unwrap();
        assert_success(&install(&project, &home));
        let root = home.path().join("tools with spaces");
        let stage = root.join(".tapid-tool-write-v1-deadbeef");
        match case {
            "file" => fs::write(&stage, b"user data").unwrap(),
            "link" => {
                #[cfg(windows)]
                assert_success(
                    &isolated_process(&project, &home, "cmd.exe")
                        .args(["/d", "/c", "mklink", "/J"])
                        .arg(&stage)
                        .arg(outside.path())
                        .output()
                        .unwrap(),
                );
                #[cfg(unix)]
                std::os::unix::fs::symlink(outside.path(), &stage).unwrap();
            }
            _ => {
                fs::create_dir(&stage).unwrap();
                if case == "unknown" {
                    fs::write(stage.join("user-file"), b"user data").unwrap();
                } else {
                    fs::create_dir(stage.join("payload")).unwrap();
                }
            }
        }
        let catalog = fs::read(root.join("catalog.json")).unwrap();
        let result = command(&project, &home, &["tool", "list"])
            .output()
            .unwrap();
        assert!(!result.status.success(), "{case}: {result:?}");
        assert!(fs::symlink_metadata(&stage).is_ok());
        assert_eq!(fs::read(root.join("catalog.json")).unwrap(), catalog);
        assert_eq!(fs::read(&sentinel).unwrap(), b"user data");
        if case == "unknown" {
            assert_eq!(fs::read(stage.join("user-file")).unwrap(), b"user data");
        }
        if case == "file" {
            assert_eq!(fs::read(&stage).unwrap(), b"user data");
        }
        #[cfg(windows)]
        if case == "link" {
            fs::remove_dir(stage).unwrap();
        }
    }
}

#[test]
fn tool_recovers_a_stop_inside_journal_publication() {
    writing_crash_case("tool_writing_journal");
}
#[test]
fn tool_recovers_a_stop_inside_commit_decision_publication() {
    writing_crash_case("tool_writing_decision");
}
#[test]
fn tool_recovers_a_stop_inside_launcher_publication() {
    writing_crash_case("tool_writing_launcher");
}

#[cfg(windows)]
#[test]
fn tool_powershell_shim_uses_the_current_filesystem_location() {
    let project = TempProject::new("tool-powershell-location").unwrap();
    let home = TempHome::new("tool-powershell-location").unwrap();
    assert_success(&install_multi(&project, &home, "other-tool@1.0.0"));
    let other = TempProject::new("tool-new-location").unwrap();
    let shim = home.path().join("tools with spaces/bin/other-command.ps1");
    let script = format!(
        "Set-Location -LiteralPath '{}'; & '{}'; exit $LASTEXITCODE",
        other.path().display().to_string().replace('\'', "''"),
        shim.display().to_string().replace('\'', "''")
    );
    let output = isolated_process(&project, &home, "powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(script)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::canonicalize(Path::new(result["cwd"].as_str().unwrap())).unwrap(),
        std::fs::canonicalize(other.path()).unwrap()
    );
}

#[cfg(windows)]
#[test]
fn tool_partial_cleanup_keeps_ownership_until_open_files_are_released() {
    use std::os::windows::fs::OpenOptionsExt;
    let project = TempProject::new("tool-partial-cleanup").unwrap();
    let home = TempHome::new("tool-partial-cleanup").unwrap();
    assert_success(&install(&project, &home));
    let listed = list(&project, &home);
    let environment = Path::new(listed["data"]["tools"][0]["environment"].as_str().unwrap());
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(environment.join("node_modules/hello-tool/package.json"))
        .unwrap();
    let removed = command(&project, &home, &["--json", "tool", "remove", "hello-tool"])
        .output()
        .unwrap();
    assert_success(&removed);
    let result: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(result["changes"]["state"], "committed_cleanup_pending");
    assert!(
        environment.join(".tapid-tool").is_file(),
        "partial deletion must preserve its ownership evidence"
    );
    drop(held);
    let result = list(&project, &home);
    assert_eq!(result["changes"]["state"], "committed");
    assert!(!environment.exists());
}

#[test]
fn tool_recovery_failure_reports_tool_paths_instead_of_project_outputs() {
    let project = TempProject::new("tool-recovery-scope").unwrap();
    let home = TempHome::new("tool-recovery-scope").unwrap();
    assert_success(&install(&project, &home));
    let root = home.path().join("tools with spaces");
    fs::write(root.join("pending.json"), b"unrecognized journal").unwrap();
    let output = command(&project, &home, &["--json", "tool", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["changes"]["state"], "recovery_required");
    let files = result["changes"]["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|path| path.as_str().unwrap().ends_with("catalog.json"))
    );
    assert!(!files.iter().any(|path| {
        ["package.json", "tapid.lock", "node_modules"]
            .iter()
            .any(|name| path.as_str().unwrap().ends_with(name))
    }));
    assert_eq!(
        fs::read(root.join("pending.json")).unwrap(),
        b"unrecognized journal"
    );
}
