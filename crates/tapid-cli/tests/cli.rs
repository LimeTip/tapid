use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};
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
fn upgrade_is_not_exposed_without_an_authenticated_update_design() {
    let dir = temp_dir("upgrade-disabled");
    let output = run(&dir, &["upgrade"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand 'upgrade'"));
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

#[cfg(windows)]
#[test]
fn run_preserves_windows_child_exit_code() {
    let dir = temp_dir("run-exit-code");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit /b 256"}}"#,
    )
    .unwrap();
    let output = run(&dir, &["run", "dev"]);
    assert_eq!(output.status.code(), Some(256));
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
fn install_warns_about_unverified_artifacts_before_later_failure() {
    let dir = temp_dir("unverified-warning-error");
    let missing_project = dir.join("missing-project");
    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--project-dir",
            missing_project.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not authenticated against a registry-declared digest"));
    assert!(stderr.contains("cannot access project directory"));
    cleanup(dir);
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
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let package = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(
        package["registryIntegrityDeclared"],
        serde_json::Value::Bool(false)
    );

    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    for mode in ["offline", "frozen"] {
        let replay = run(&dir, &["install", &format!("--{mode}")]);
        assert_eq!(replay.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&replay.stderr)
                .contains("lacks registry-declared artifact integrity")
        );
        assert!(!dir.join("node_modules").exists());
    }
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
fn offline_replay_uses_exact_roots_when_names_have_transitive_versions() {
    let dir = temp_dir("exact-roots");
    let fixture = dir.join("registry.json");
    let debug_four = "base64:H4sIAAAAAAAC/+3TsQrCMBRA0cx+hWTWNLGxg38TNRQV09K0Ioj/bqwFobMU1HuWF97yhnBrtzu50mf1a6pjrIL4MJ0U1vYzGU+tbf5+P/fGFKtczLWYQBdb16Tz4j/dZHBnLzdy77ddKRfy4pt4qELaWKWVlveZwO8aus+Gb1fttRWT92/suP91Yeh/Cn32yz51QgcAAAAAAAAAAAAAAPhCD3aP37sAKAAA";
    let debug_three = "base64:H4sIAAAAAAAC/+3TvQrCMBRA4cw+hWTWNDGxg28TNRQV29IfEcR3N8aC0FkK6vmWG+5yh3Bqvzv5ImT1a6pjW5Xiw3SUO5dmNJ5aO/t+P/fG5Csr5lpMoG8738Tz4j/dZOnPQW7kPmz7Qi7kJTTtoSrjxiqttLzPBH7X0H02fLvqrp2YvH/jxv2vc0P/U0jZL1PqhA4AAAAAAAAAAAAAAPCFHl2bEsoAKAAA";
    let parent = "base64:H4sIAAAAAAAC/+3TsQrCMBRA0cx+hWTWNCltB/8mSBAV05BGKYj/bmwLQmcpqPcsD16GDI8b7P5sD64I41SnrvXiw3TWVNUws/nU+v027o1pykqstVjAtUs25u/Ff7pLby9O7mSw0fkkN/LmYndsfV4ZpZWWj5XAz5q6L6arq9QnsXj/pp73Xzcl/S9z/1f22yF1QgcAAAAAAAAAAAAAAPg+TxkNmJgAKAAA";
    let integrity = |artifact: &str| {
        let bytes = STANDARD
            .decode(artifact.strip_prefix("base64:").unwrap())
            .unwrap();
        format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))
    };
    let debug_four_integrity = integrity(debug_four);
    let debug_three_integrity = integrity(debug_three);
    let parent_integrity = integrity(parent);
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"debug":"^4.0.0","parent":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[
                {{"registry":"https://registry.npmjs.org","name":"debug","version":"4.0.0","integrity":"{debug_four_integrity}","artifact":"{debug_four}"}},
                {{"registry":"https://registry.npmjs.org","name":"debug","version":"3.0.0","integrity":"{debug_three_integrity}","artifact":"{debug_three}"}},
                {{"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","integrity":"{parent_integrity}","artifact":"{parent}","dependencies":{{"debug":"^3.0.0"}}}}
            ]}}"#
        ),
    )
    .unwrap();

    let online = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        online.status.success(),
        "{}",
        String::from_utf8_lossy(&online.stderr)
    );
    fs::remove_dir_all(dir.join("node_modules")).unwrap();

    let replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(dir.join("node_modules/debug").is_dir());
    assert!(dir.join("node_modules/parent/node_modules/debug").is_dir());
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/debug/version.txt")).unwrap(),
        "debug-4.0.0\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/parent/node_modules/debug/version.txt")).unwrap(),
        "debug-3.0.0\n"
    );

    let lock_path = dir.join("tapid.lock");
    let canonical: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&lock_path).unwrap()).unwrap();
    let package_keys = canonical["packages"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let debug_three = package_keys
        .iter()
        .find(|key| key.contains("|debug@3.0.0|"))
        .unwrap()
        .clone();
    let parent = package_keys
        .iter()
        .find(|key| key.contains("|parent@1.0.0|"))
        .unwrap()
        .clone();

    let mut transitive_root = canonical.clone();
    let mut invalid_roots = vec![debug_three, parent.clone()];
    invalid_roots.sort();
    transitive_root["roots"] = serde_json::json!(invalid_roots);
    fs::write(
        &lock_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&transitive_root).unwrap()
        ),
    )
    .unwrap();
    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let transitive_replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(!transitive_replay.status.success());
    assert!(
        String::from_utf8_lossy(&transitive_replay.stderr)
            .contains("does not satisfy a direct manifest dependency")
    );
    assert!(!dir.join("node_modules").exists());

    let mut missing_root = canonical.clone();
    missing_root["roots"] = serde_json::json!([parent]);
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string_pretty(&missing_root).unwrap()),
    )
    .unwrap();
    let missing_replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(!missing_replay.status.success());
    assert!(
        String::from_utf8_lossy(&missing_replay.stderr)
            .contains("must contain exactly one root for direct dependency")
    );
    assert!(!dir.join("node_modules").exists());

    let mut legacy = canonical;
    legacy["lockfileVersion"] = 4.into();
    legacy.as_object_mut().unwrap().remove("roots");
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string_pretty(&legacy).unwrap()),
    )
    .unwrap();

    let legacy_replay = run(&dir, &["install", "--offline", "--frozen"]);
    assert!(
        legacy_replay.status.success(),
        "{}",
        String::from_utf8_lossy(&legacy_replay.stderr)
    );
    assert!(dir.join("node_modules/debug").is_dir());
    assert!(dir.join("node_modules/parent/node_modules/debug").is_dir());
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/debug/version.txt")).unwrap(),
        "debug-4.0.0\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/parent/node_modules/debug/version.txt")).unwrap(),
        "debug-3.0.0\n"
    );
    cleanup(dir);
}

#[test]
fn padded_and_unpadded_fixture_integrity_produce_canonical_lockfile_values() {
    let dir = temp_dir("canonical-integrity");
    let fixture = dir.join("registry.json");
    let encoded_artifact = "H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let artifact_bytes = STANDARD.decode(encoded_artifact).unwrap();
    let padded = format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(&artifact_bytes))
    );
    let unpadded = padded.trim_end_matches('=').to_owned();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"padded":"1.0.0","unpadded":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[
                {{"registry":"https://registry.npmjs.org","name":"padded","version":"1.0.0","integrity":"{padded}","artifact":"base64:{encoded_artifact}"}},
                {{"registry":"https://registry.npmjs.org","name":"unpadded","version":"1.0.0","integrity":"{unpadded}","artifact":"base64:{encoded_artifact}"}}
            ]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    for package in lock["packages"].as_object().unwrap().values() {
        assert_eq!(package["artifactIntegrity"], padded);
    }
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unmarked node_modules"), "{stderr}");
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
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1
"
    );
    assert!(!fs::read_dir(&dir).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tapid-managed-old-")
    }));
    cleanup(dir);
}
#[test]
fn registry_fixture_relative_artifact_is_loaded_from_fixture_directory() {
    let root = temp_dir("relative-fixture");
    let fixture_dir = root.join("fixture");
    let project = root.join("project");
    let unrelated = root.join("unrelated");
    let store = root.join("store");
    fs::create_dir_all(fixture_dir.join("fixture-files")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    let encoded = "H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    fs::write(
        fixture_dir.join("fixture-files/artifact.tgz"),
        STANDARD.decode(encoded).unwrap(),
    )
    .unwrap();
    fs::write(
        project.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"foo":"1.0.0"}}"#,
    )
    .unwrap();
    let fixture = fixture_dir.join("registry.json");
    fs::write(&fixture, r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"foo","version":"1.0.0","artifact":"fixture-files/artifact.tgz"}]}"#).unwrap();
    let output = run(
        &unrelated,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--project-dir",
            project.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(project.join("node_modules/foo").is_dir());
    cleanup(root);
}
