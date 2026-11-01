use std::{fs, process::Command};
use tapid_test_support::{TempHome, TempProject};

#[test]
fn pack_preview_and_creation_share_inventory_without_running_hooks() {
    let project = TempProject::new("cli-pack").unwrap();
    let home = TempHome::new("cli-pack").unwrap();
    project.write("package.json", br#"{"name":"example","version":"1.0.0","scripts":{"prepack":"touch marker","prepare":"touch marker","postpack":"touch marker"}}"#).unwrap();
    project.write("index.js", b"module.exports = 42;").unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .current_dir(project.path())
            .env("HOME", home.path())
            .output()
            .unwrap()
    };
    let preview = invoke(&["--json", "pack", "--dry-run", "--output", "artifact.bin"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["changes"]["state"], "unchanged");
    assert!(!project.path().join("artifact.bin").exists());
    let creation = invoke(&["pack", "--json", "--output", "artifact.bin"]);
    assert!(
        creation.status.success(),
        "{}",
        String::from_utf8_lossy(&creation.stderr)
    );
    let creation: serde_json::Value = serde_json::from_slice(&creation.stdout).unwrap();
    assert_eq!(creation["changes"]["state"], "committed");
    assert_eq!(creation["data"]["files"], preview["data"]["files"]);
    assert_eq!(creation["data"]["archive"], preview["data"]["archive"]);
    let original = fs::read(project.path().join("artifact.bin")).unwrap();
    assert_eq!(
        creation["data"]["archive"]["sha256"],
        tapid_publish::artifact_digest(&original).to_string()
    );
    let existing = invoke(&["--json", "pack", "--output", "artifact.bin"]);
    assert!(!existing.status.success());
    let existing: serde_json::Value = serde_json::from_slice(&existing.stdout).unwrap();
    assert!(existing["data"].is_null());
    assert_eq!(
        fs::read(project.path().join("artifact.bin")).unwrap(),
        original
    );
    assert!(!project.path().join("marker").exists());
}

#[test]
fn pack_missing_output_invalid_metadata_and_destination_parent_leave_sources_unchanged() {
    let project = TempProject::new("cli-pack-failures").unwrap();
    let home = TempHome::new("cli-pack-failures").unwrap();
    let manifest = br#"{"name":"example","version":"1.0.0","files":["[unsupported]"]}"#;
    project.write("package.json", manifest).unwrap();
    for args in [
        vec!["pack"],
        vec!["pack", "--output", "missing/out.tgz"],
        vec!["pack", "--output", "out.tgz"],
        vec!["pack", "--dry-run"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_tapid"))
            .arg("--json")
            .args(&args)
            .current_dir(project.path())
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(!result.status.success());
        let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(result["changes"]["state"], "unchanged");
        assert!(result["data"].is_null());
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn pack_refuses_dangling_destination_symlinks() {
    let project = TempProject::new("cli-pack-output-symlink").unwrap();
    let home = TempHome::new("cli-pack-output-symlink").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    std::os::unix::fs::symlink("absent", project.path().join("out")).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["pack", "--dry-run", "--output", "out"])
        .current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        fs::read_link(project.path().join("out")).unwrap(),
        std::path::Path::new("absent")
    );
}
