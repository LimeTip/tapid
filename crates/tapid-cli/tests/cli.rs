use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_lockfile::Lockfile;

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("tapid-cli-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}
fn run(cwd: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}
fn run_with_env(cwd: &PathBuf, args: &[&str], key: &str, value: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(args)
        .current_dir(cwd)
        .env(key, value)
        .output()
        .unwrap()
}
fn cleanup(path: PathBuf) {
    let _ = fs::remove_dir_all(path);
}
#[test]
fn upgrade_fails_closed_without_explicit_trust_root() {
    let dir = temp_dir("upgrade-no-keyring");
    let output = run(
        &dir,
        &[
            "upgrade",
            "--endpoint",
            "https://example.invalid/stable.json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("trusted release keyring is required")
    );
    cleanup(dir);
}

#[test]
fn upgrade_rejects_unmanaged_destination_before_network() {
    let dir = temp_dir("upgrade-unmanaged");
    let destination = dir.join("tapid");
    fs::write(&destination, b"old").unwrap();
    let keyring = dir.join("keys.json");
    fs::write(
        &keyring,
        br#"{"version":"tapid-release-keyring-v1","keys":[]}"#,
    )
    .unwrap();
    let output = run(
        &dir,
        &[
            "upgrade",
            "--endpoint",
            "https://example.invalid/stable.json",
            "--keyring",
            keyring.to_str().unwrap(),
            "--destination",
            destination.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Tapid-managed"));
    assert_eq!(fs::read(&destination).unwrap(), b"old");
    cleanup(dir);
}
fn lock_for_manifest(raw: &str) -> Lockfile {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = format!("sha256-{}", hex::encode(hasher.finalize()));
    Lockfile::new(&digest).unwrap()
}

#[test]
fn init_creates_manifest_and_reports_stdout() {
    let dir = temp_dir("init");
    let output = run(&dir, &["init"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Created "));
    assert!(
        fs::read_to_string(dir.join("package.json"))
            .unwrap()
            .contains("\"private\": true")
    );
    cleanup(dir);
}
#[test]
fn validate_reports_success_and_malformed_input() {
    let dir = temp_dir("validate");
    fs::write(
        dir.join("good.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    )
    .unwrap();
    let good = run(&dir, &["manifest", "validate", "good.json"]);
    assert!(good.status.success());
    assert_eq!(
        String::from_utf8_lossy(&good.stdout),
        "Valid manifest: demo@1.0.0\n"
    );
    assert!(good.stderr.is_empty());
    fs::write(dir.join("bad.json"), "not json").unwrap();
    let bad = run(&dir, &["manifest", "validate", "bad.json"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid package.json"));
    assert!(bad.stdout.is_empty());
    cleanup(dir);
}
#[test]
fn lock_verify_reports_valid_and_missing_files() {
    let dir = temp_dir("lock");
    let lock =
        Lockfile::new("sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let valid = run(&dir, &["lock", "verify"]);
    assert!(valid.status.success());
    assert!(String::from_utf8_lossy(&valid.stdout).contains("Valid lockfile"));
    fs::remove_file(dir.join("tapid.lock")).unwrap();
    let missing = run(&dir, &["lock", "verify"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot verify"));
    cleanup(dir);
}
#[test]
fn clap_rejects_unknown_commands_with_usage_error() {
    let dir = temp_dir("unknown");
    let output = run(&dir, &["wat"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    assert!(output.stdout.is_empty());
    cleanup(dir);
}

#[test]
fn run_executes_root_script_and_forwards_arguments() {
    let dir = temp_dir("run");
    #[cfg(unix)]
    let manifest = r#"{"name":"demo","version":"1.0.0","scripts":{"init":"printf '%s' \"$1\" > forwarded","dev":"exit 37"}}"#;
    #[cfg(windows)]
    let manifest = r#"{"name":"demo","version":"1.0.0","scripts":{"init":"echo > forwarded","dev":"exit /b 37"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let output = run(&dir, &["run", "init", "--", "hello world"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("forwarded"))
            .unwrap()
            .trim()
            .trim_matches('"'),
        "hello world"
    );
    let child = run(&dir, &["run", "dev"]);
    assert_eq!(child.status.code(), Some(37));
    cleanup(dir);
}

#[test]
fn run_resolves_installed_bin_and_rejects_missing_script_stably() {
    let dir = temp_dir("run-bin");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"init":"helper > bin-output"}}"#,
    )
    .unwrap();
    #[cfg(unix)]
    let helper = dir.join("node_modules/.bin/helper");
    #[cfg(windows)]
    let helper = dir.join("node_modules/.bin/helper.cmd");
    #[cfg(unix)]
    fs::write(&helper, "#!/bin/sh\nprintf bin-ok\n").unwrap();
    #[cfg(windows)]
    fs::write(&helper, "@echo off\r\necho bin-ok\r\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ok = run(&dir, &["run", "init"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("bin-output"))
            .unwrap()
            .trim_end(),
        "bin-ok"
    );
    let missing = run(&dir, &["run", "missing"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr),
        "error: root package script is missing: missing\n"
    );
    cleanup(dir);
}
#[test]
fn install_rejects_malformed_lockfile_before_creating_output() {
    let dir = temp_dir("bad-install");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), "not json").unwrap();
    let output = run(&dir, &["install", "--offline"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid lockfile"));
    assert!(!dir.join("node_modules").exists());
    cleanup(dir);
}

#[test]
fn install_rejects_unverified_artifacts_with_offline_or_frozen() {
    for mode in ["offline", "frozen"] {
        let dir = temp_dir(&format!("unverified-{mode}"));
        fs::write(
            dir.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        let output = run(
            &dir,
            &[
                "install",
                "--allow-unverified-registry-artifacts",
                &format!("--{mode}"),
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("cannot be used with --offline or --frozen")
        );
        cleanup(dir);
    }
}

#[test]
fn install_allows_missing_integrity_only_with_explicit_warning() {
    let dir = temp_dir("unverified-online");
    let fixture = dir.join("registry.json");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"foo":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"foo","version":"1.0.0","artifact":"base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA"}]}"#,
    )
    .unwrap();

    let rejected = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("missing dist.integrity"));
    assert!(!dir.join("node_modules").exists());

    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("not authenticated against a registry-declared digest")
    );
    assert!(dir.join("node_modules").is_dir());
    cleanup(dir);
}

#[test]
fn install_requires_lockfile_in_offline_and_frozen_modes() {
    for mode in ["offline", "frozen"] {
        let dir = temp_dir(mode);
        fs::write(
            dir.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        let output = run(&dir, &["install", &format!("--{mode}")]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires tapid.lock"));
        assert!(!dir.join("node_modules").exists());
        cleanup(dir);
    }
}

#[test]
fn install_supports_an_explicit_dynamic_project_directory() {
    let parent = temp_dir("parent");
    let project = parent.join("project");
    fs::create_dir(&project).unwrap();
    let raw_manifest = r#"{"name":"dynamic-app","version":"1.0.0"}"#;
    fs::write(project.join("package.json"), raw_manifest).unwrap();
    let lock = lock_for_manifest(raw_manifest);
    fs::write(project.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let output = run(
        &parent,
        &[
            "install",
            "--project-dir",
            project.to_str().unwrap(),
            "--frozen",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(project.join("node_modules").is_dir());
    cleanup(parent);
}

#[test]
fn install_replays_valid_lockfile_without_running_scripts() {
    let dir = temp_dir("install");
    let raw_manifest =
        r#"{"name":"demo","version":"1.0.0","scripts":{"preinstall":"touch SHOULD_NOT_EXIST"}}"#;
    fs::write(dir.join("package.json"), raw_manifest).unwrap();
    let lock = lock_for_manifest(raw_manifest);
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    let verified = run(&dir, &["lock", "verify"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let output = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules").is_dir());
    assert!(!dir.join("SHOULD_NOT_EXIST").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Replayed lockfile"));
    cleanup(dir);
}

#[test]
fn install_refuses_to_replace_unmarked_node_modules() {
    let dir = temp_dir("ownership");
    let raw = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), raw).unwrap();
    fs::write(
        dir.join("tapid.lock"),
        lock_for_manifest(raw).to_json().unwrap(),
    )
    .unwrap();
    fs::create_dir(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules").join("KEEP"), "user data").unwrap();
    let output = run(&dir, &["install", "--offline"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unmarked node_modules"));
    assert_eq!(
        fs::read_to_string(dir.join("node_modules").join("KEEP")).unwrap(),
        "user data"
    );
    cleanup(dir);
}

#[test]
fn injected_activation_failure_restores_marked_node_modules() {
    let dir = temp_dir("activation-failure");
    let raw = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), raw).unwrap();
    fs::write(
        dir.join("tapid.lock"),
        lock_for_manifest(raw).to_json().unwrap(),
    )
    .unwrap();
    fs::create_dir(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules").join("KEEP"), "user data").unwrap();
    fs::write(dir.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
    let output = run_with_env(
        &dir,
        &["install", "--offline"],
        "TAPID_TEST_FAIL_ACTIVATION",
        "1",
    );
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(dir.join("node_modules").join("KEEP")).unwrap(),
        "user data"
    );
    cleanup(dir);
}
