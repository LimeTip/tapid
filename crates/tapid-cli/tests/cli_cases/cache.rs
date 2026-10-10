use std::{fs, process::Command};
use tapid_test_support::TempHome;

#[test]
fn cache_inspection_and_clean_preview_preserve_bytes_until_yes() {
    let home = TempHome::new("cache-cli").unwrap();
    let store = home.path().join("store");
    fs::create_dir_all(store.join("artifacts")).unwrap();
    let artifact = store
        .join("artifacts")
        .join(format!("sha256-{}", "a".repeat(64)));
    fs::write(&artifact, b"cache bytes").unwrap();
    fs::write(store.join(".store.lock"), b"").unwrap();

    fs::write(store.join("keep-me"), b"user file").unwrap();
    for args in [vec!["cache"], vec!["clean"], vec!["clean", "--dry-run"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .arg("--store-dir")
            .arg(&store)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("11 bytes"), "{text}");
        assert!(artifact.exists());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["clean", "--yes", "--store-dir"])
        .arg(&store)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!artifact.exists());
    assert_eq!(fs::read(store.join("keep-me")).unwrap(), b"user file");
}

#[test]
fn cache_json_uses_versioned_results_for_info_preview_and_clear() {
    let home = TempHome::new("cache-json").unwrap();
    let store = home.path().join("store");
    let digest = format!("sha256-{}", "a".repeat(64));
    fs::create_dir_all(store.join("artifacts")).unwrap();
    fs::write(store.join(".store.lock"), b"").unwrap();
    fs::write(store.join("artifacts").join(digest), b"123").unwrap();
    for (args, state, action) in [
        (vec!["--json", "cache", "info"], "unchanged", "info"),
        (vec!["cache", "clean", "--json"], "unchanged", "preview"),
        (
            vec!["--json", "cache", "clean", "--yes"],
            "committed",
            "clean",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .arg("--store-dir")
            .arg(&store)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["operation"], "cache");
        assert_eq!(value["changes"]["state"], state);
        assert_eq!(value["data"]["action"], action);
        assert_eq!(value["data"]["summary"]["artifacts"]["bytes"], 3);
    }
}

#[test]
fn cache_json_failure_does_not_claim_changes_before_deletion() {
    let home = TempHome::new("bad-cache-json").unwrap();
    let store = home.path().join("not-a-directory");
    fs::write(&store, b"preserve me").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["--json", "clean", "--yes", "--store-dir"])
        .arg(&store)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(fs::read(store).unwrap(), b"preserve me");
}

#[test]
fn cache_clean_refuses_a_store_used_by_another_process() {
    let home = TempHome::new("cache-busy-cli").unwrap();
    let store = tapid_store::Store::new(home.path().join("store"));
    let digest = format!("sha256-{}", "a".repeat(64)).parse().unwrap();
    let guard = store.read_guard().unwrap();
    fs::create_dir_all(store.root().join("artifacts")).unwrap();
    fs::write(store.artifact_path(&digest), b"cache bytes").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["clean", "--yes", "--json", "--store-dir"])
        .arg(store.root())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "CACHE_BUSY");
    assert_eq!(value["errors"][0]["phase"], "operation");
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(value["retry"], "after_contention");
    assert!(store.artifact_path(&digest).exists());
    drop(guard);
}

#[test]
fn cache_defaults_match_install_location_without_creating_files_or_loading_project() {
    let home = TempHome::new("cache-default-cli").unwrap();
    let project = tapid_test_support::TempProject::new("cache-no-project").unwrap();
    project.write("package.json", b"invalid").unwrap();
    project.write("tapid.toml", b"invalid").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .args(["cache", "info", "--json"])
        .env_clear()
        .env("HOME", home.path())
        .env("LOCALAPPDATA", home.path())
        .current_dir(project.path());
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let platform = if cfg!(target_os = "macos") {
        "Library/Caches"
    } else if cfg!(windows) {
        ""
    } else {
        ".cache"
    };
    assert_eq!(
        value["data"]["store_path"]["value"],
        home.path()
            .join(platform)
            .join("tapid/store")
            .to_str()
            .unwrap()
    );
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 2);
}

#[test]
fn cache_clean_rejects_conflicting_confirmation_and_preview_flags() {
    let home = TempHome::new("cache-options").unwrap();
    for args in [vec!["clean"], vec!["cache", "clean"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .args(["--yes", "--dry-run", "--store-dir"])
            .arg(home.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    }
}

#[test]
fn cache_json_operational_errors_have_a_phase_and_invalid_roots_have_path_codes() {
    let home = TempHome::new("cache-path-errors").unwrap();
    let file = home.path().join("file");
    fs::write(&file, b"preserve").unwrap();
    for root in [file.as_path()] {
        for args in [vec!["cache", "info"], vec!["clean", "--yes"]] {
            let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
                .args(args)
                .args(["--json", "--store-dir"])
                .arg(root)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["errors"][0]["code"], "CACHE_PATH_INVALID");
            assert_eq!(value["errors"][0]["phase"], "operation");
            assert_eq!(value["changes"]["state"], "unchanged");
        }
    }
    assert_eq!(fs::read(file).unwrap(), b"preserve");
}

#[test]
fn cache_empty_explicit_root_remains_an_argument_error() {
    let home = TempHome::new("cache-empty-root").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["cache", "info", "--json", "--store-dir", ""])
        .current_dir(home.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "ARGUMENT_INVALID");
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn cache_invalid_default_location_is_an_operational_path_error() {
    let home = TempHome::new("cache-bad-home").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
    command
        .args(["cache", "info", "--json"])
        .env_clear()
        .env("HOME", "relative")
        .env("LOCALAPPDATA", "relative")
        .current_dir(home.path());
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "CACHE_PATH_INVALID");
    assert_eq!(value["errors"][0]["phase"], "operation");
    assert_eq!(value["changes"]["state"], "unchanged");
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn cache_json_pending_recovery_has_operation_phase() {
    let home = TempHome::new("cache-error-phase").unwrap();
    let store = home.path().join("store");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join(".store.lock"), b"").unwrap();
    fs::write(store.join(".tapid-transaction.json"), b"pending").unwrap();
    for args in [vec!["cache", "info"], vec!["clean", "--yes"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .args(["--json", "--store-dir"])
            .arg(&store)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["errors"][0]["code"], "CACHE_MAINTENANCE_FAILED");
        assert_eq!(value["errors"][0]["phase"], "operation");
        assert_eq!(value["changes"]["state"], "unchanged");
    }
    assert_eq!(
        fs::read(store.join(".tapid-transaction.json")).unwrap(),
        b"pending"
    );
}
