use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_lockfile::Lockfile;

fn temp_dir(label: &str) -> PathBuf {
    static NEXT_TEMP_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT_TEMP_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "tapid-cli-{label}-{}-{nonce}-{sequence}",
        std::process::id()
    ));
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

fn run_with_isolated_path(cwd: &PathBuf, args: &[&str], path: &OsStr) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", path)
        .output()
        .unwrap()
}

fn cleanup(path: PathBuf) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn missing_private_registry_credentials_fail_before_committing_manifest_changes() {
    let dir = temp_dir("missing-registry-credential");
    let original = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), original).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_MISSING'\n",
    )
    .unwrap();
    let store = dir.join("store");
    let previous_lock = b"previous lock bytes";
    let previous_node_modules = b"previous installed tree marker";
    let previous_store = b"pre-existing verified store marker";
    fs::write(dir.join("tapid.lock"), previous_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/previous.txt"), previous_node_modules).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("previous.txt"), previous_store).unwrap();
    let output = run_with_isolated_path(
        &dir,
        &[
            "add",
            "@acme/private@1.0.0",
            "--store-dir",
            store.to_str().unwrap(),
        ],
        OsStr::new(""),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("TAPID_ISSUE164_MISSING"), "{stderr}");
    assert!(!stderr.contains("secret"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        original.as_bytes()
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), previous_lock);
    assert_eq!(
        fs::read(dir.join("node_modules/previous.txt")).unwrap(),
        previous_node_modules
    );
    assert_eq!(
        fs::read(store.join("previous.txt")).unwrap(),
        previous_store
    );
    cleanup(dir);
}

#[test]
fn invalid_private_registry_credentials_are_redacted_and_fail_transactionally() {
    let dir = temp_dir("invalid-registry-credential");
    let original = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), original).unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_INVALID'\n",
    )
    .unwrap();
    let store = dir.join("store");
    let invalid_secret = "synthetic-invalid-secret\nwith-newline";
    let output = run_with_env(
        &dir,
        &[
            "add",
            "@acme/private@1.0.0",
            "--store-dir",
            store.to_str().unwrap(),
        ],
        "TAPID_ISSUE164_INVALID",
        invalid_secret,
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid registry credential"), "{stderr}");
    assert!(!stderr.contains("synthetic-invalid-secret"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        original.as_bytes()
    );
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn scoped_registry_routing_selects_private_and_default_origins_and_replays_offline() {
    let dir = temp_dir("scoped-registry-routing");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","dependencies":{"@acme/private":"1.0.0","left-pad":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.default]\nurl='https://mirror.example'\n[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_OFFLINE_ONLY'\n",
    )
    .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://packages.example","name":"@acme/private","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}},{{"registry":"https://mirror.example","name":"left-pad","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");
    let installed = run(
        &dir,
        &[
            "install",
            "--registry-fixture",
            fixture.to_str().unwrap(),
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let lock_json = fs::read_to_string(dir.join("tapid.lock")).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&lock_json).unwrap();
    let registries = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .map(|package| package["registry"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        registries,
        ["https://mirror.example", "https://packages.example"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let replay = run_with_isolated_path(
        &dir,
        &[
            "install",
            "--offline",
            "--store-dir",
            store.to_str().unwrap(),
        ],
        OsStr::new(""),
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    cleanup(dir);
}

#[test]
fn registry_credential_environment_variables_are_denied_to_root_scripts() {
    let dir = temp_dir("registry-credential-child-env");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"test":"printf child-started"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[registries.'@acme']\nurl='https://packages.example'\ntoken-env='TAPID_ISSUE164_CHILD_SECRET'\n[run.scripts.test]\nenvironment=['TAPID_ISSUE164_CHILD_SECRET']\n",
    )
    .unwrap();
    let secret = "synthetic-secret-value";
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(["run", "test", "--project-dir"])
        .arg(&dir)
        .env("TAPID_ISSUE164_CHILD_SECRET", secret)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("registry credential"), "{stderr}");
    assert!(!stderr.contains(secret), "{stderr}");
    cleanup(dir);
}

#[test]
fn lifecycle_commands_are_exposed_as_cli_commands() {
    let dir = temp_dir("lifecycle-help");
    let output = run(&dir, &["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in ["add", "remove", "update", "outdated", "prune"] {
        assert!(
            stdout.contains(command),
            "missing {command} in help: {stdout}"
        );
    }
    let add_help = run(&dir, &["add", "--help"]);
    assert!(add_help.status.success());
    let add_help = String::from_utf8_lossy(&add_help.stdout);
    assert!(
        add_help.contains("Select workspace member by name"),
        "missing workspace selector guidance: {add_help}"
    );
    cleanup(dir);
}

#[test]
fn read_only_lifecycle_commands_fail_closed_without_writing() {
    let dir = temp_dir("lifecycle-read-only");
    let manifest = "{\"name\":\"demo\",\"version\":\"1.0.0\"}\n";
    fs::write(dir.join("package.json"), manifest).unwrap();
    let outdated = run(&dir, &["outdated"]);
    assert_eq!(outdated.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&outdated.stderr).contains("cannot read lockfile"));
    let prune = run(&dir, &["prune"]);
    assert_eq!(prune.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&prune.stderr).contains("requires tapid.lock"));
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert!(!dir.join("tapid.lock").exists());
    cleanup(dir);
}

#[test]
fn outdated_reports_versions_without_mutating_project_state() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};

    let dir = temp_dir("outdated-read-only");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"is-char":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let mut lock = lock_for_manifest(manifest);
    let package = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "is-char",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let key = package.key();
    lock.insert_package(package).unwrap();
    lock.set_roots([key]).unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    fs::create_dir_all(dir.join("node_modules/keep")).unwrap();
    fs::write(dir.join("node_modules/keep/sentinel"), "untouched").unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.1.0"},{"registry":"https://registry.npmjs.org","name":"is-char","version":"2.0.0"}]}"#,
    )
    .unwrap();
    let manifest_before = fs::read(dir.join("package.json")).unwrap();
    let lock_before = fs::read(dir.join("tapid.lock")).unwrap();
    let sentinel_before = fs::read(dir.join("node_modules/keep/sentinel")).unwrap();
    let entries_before = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let output = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "is-char [dependencies] declared=^1.0.0 locked=1.0.0 compatible=1.1.0 available=2.0.0"
    ));
    assert_eq!(fs::read(dir.join("package.json")).unwrap(), manifest_before);
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock_before);
    assert_eq!(
        fs::read(dir.join("node_modules/keep/sentinel")).unwrap(),
        sentinel_before
    );
    let entries_after = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(entries_after, entries_before);
    cleanup(dir);
}

#[test]
fn remove_resolves_remaining_dependencies_and_cleans_stale_materialization() {
    let dir = temp_dir("remove-re-resolve");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"removed":"1.0.0","keep":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    for name in ["removed", "keep"] {
        fs::create_dir_all(dir.join(format!("node_modules/{name}"))).unwrap();
        fs::write(
            dir.join(format!("node_modules/{name}/package.json")),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
    }
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"keep","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "remove",
            "removed",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let updated = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(!updated.contains("\"removed\""));
    assert!(updated.contains("\"keep\": \"1.0.0\""));
    assert!(!dir.join("node_modules/removed").exists());
    assert!(dir.join("node_modules/keep/package.json").is_file());
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert_eq!(lock["packages"].as_object().unwrap().len(), 1);

    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let replay = run(
        &dir,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(dir.join("node_modules/keep/package.json").is_file());
    assert!(!dir.join("node_modules/removed").exists());
    cleanup(dir);
}

#[test]
fn update_preserves_ranges_unless_latest_is_requested() {
    let dir = temp_dir("update-range-behavior");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"is-char":"^1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.1.0","integrity":"{integrity}","artifact":"{artifact}"}},{{"registry":"https://registry.npmjs.org","name":"is-char","version":"2.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let update = run(
        &dir,
        &[
            "update",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    let after_update = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(after_update.contains("\"is-char\": \"^1.0.0\""));
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.contains("is-char@1.1.0"))
    );

    let latest = run(
        &dir,
        &[
            "update",
            "--latest",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        latest.status.success(),
        "{}",
        String::from_utf8_lossy(&latest.stderr)
    );
    let after_latest = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(after_latest.contains("\"is-char\": \"*\""));
    let latest_lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        latest_lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.contains("is-char@2.0.0"))
    );

    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let replay = run(
        &dir,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    cleanup(dir);
}

#[test]
fn lifecycle_workspace_selector_mutates_only_selected_member() {
    let dir = temp_dir("workspace-member-selection");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    let web_manifest = r#"{"name":"web","version":"1.0.0"}"#;
    let worker_manifest = r#"{"name":"worker","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/web")).unwrap();
    fs::create_dir_all(dir.join("packages/worker")).unwrap();
    fs::write(dir.join("packages/web/package.json"), web_manifest).unwrap();
    fs::write(dir.join("packages/worker/package.json"), worker_manifest).unwrap();

    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();
    let store = dir.join("store");

    let output = run(
        &dir,
        &[
            "add",
            "is-char@1.0.0",
            "--workspace",
            "web",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("packages/worker/package.json")).unwrap(),
        worker_manifest.as_bytes()
    );
    let updated_web = fs::read_to_string(dir.join("packages/web/package.json")).unwrap();
    assert!(
        updated_web.contains("is-char"),
        "selected workspace manifest was not updated; stdout={} stderr={}; manifest: {updated_web}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("packages/web/tapid.lock").is_file());
    assert!(!dir.join("tapid.lock").exists());
    assert!(
        dir.join("packages/web/node_modules/is-char/package.json")
            .is_file()
    );
    assert!(!dir.join("packages/worker/node_modules").exists());

    let default_remove = run(
        &dir.join("packages/web"),
        &[
            "remove",
            "is-char",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        default_remove.status.success(),
        "{}",
        String::from_utf8_lossy(&default_remove.stderr)
    );
    let web_after_remove = fs::read_to_string(dir.join("packages/web/package.json")).unwrap();
    assert!(!web_after_remove.contains("is-char"));
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("packages/worker/package.json")).unwrap(),
        worker_manifest.as_bytes()
    );
    assert!(!dir.join("packages/web/node_modules/is-char").exists());
    cleanup(dir);
}

#[test]
fn workspace_protocol_add_fails_closed_before_registry_or_project_mutation() {
    use tapid_store::Store;

    let dir = temp_dir("workspace-protocol-fail-closed");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    fs::create_dir_all(dir.join("packages/local")).unwrap();
    fs::write(
        dir.join("packages/local/package.json"),
        r#"{"name":"local","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.lock"), "old lock bytes\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();

    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let prior_source = dir.join("prior-store-tree");
    fs::create_dir_all(&prior_source).unwrap();
    fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
    let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store
        .activate_verified_tree(&prior_digest, &prior_source)
        .unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"local","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "add",
            "local@workspace:*",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workspace dependency reference"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("registry"),
        "unexpected registry fallback: {stderr}"
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"old lock bytes\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(store.verified_tree_path(&prior_digest).is_ok());
    cleanup(dir);
}

#[test]
fn npm_and_jsr_registry_identities_remain_distinct_for_related_packages() {
    let dir = temp_dir("npm-jsr-distinct-identities");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"is-char":"1.0.0","jsr:@arvid/is-char":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}},{{"registry":"https://jsr.io","name":"@arvid/is-char","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let packages = lock["packages"].as_object().unwrap();
    assert_eq!(packages.len(), 2);
    assert!(
        packages
            .keys()
            .any(|key| key.starts_with("https://registry.npmjs.org|is-char@1.0.0|"))
    );
    assert!(
        packages
            .keys()
            .any(|key| key.starts_with("https://jsr.io|@arvid/is-char@1.0.0|"))
    );
    assert!(dir.join("node_modules/is-char/package.json").is_file());
    assert!(
        dir.join("node_modules/@arvid/is-char/package.json")
            .is_file()
    );
    cleanup(dir);
}

#[test]
fn lifecycle_add_rolls_back_manifest_when_resolution_fails() {
    use tapid_store::Store;

    let dir = temp_dir("lifecycle-rollback");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join("tapid.lock"), "old lock bytes\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();
    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let source = dir.join("prior-store-tree");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), "prior store tree").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store.activate_verified_tree(&digest, &source).unwrap();
    let trees_before = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
                r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"unrelated","version":"1.0.0","integrity":"sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==","artifact":"base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA"}]}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "add",
            "is-char",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("resolution failed"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"old lock bytes\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(store.verified_tree_path(&digest).is_ok());
    let trees_after = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(trees_after, trees_before);
    cleanup(dir);
}

#[test]
fn lifecycle_add_rejects_invalid_fetched_range_without_state_changes() {
    let dir = temp_dir("lifecycle-invalid-fetched-range");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join("tapid.lock"), "old lock bytes\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","artifact":"unused","dependencies":{"broken":"not a valid range"}}]}"#,
    )
    .unwrap();
    let store = dir.join("previously-nonexistent-store");

    let output = run(
        &dir,
        &[
            "add",
            "parent@1.0.0",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency broken"), "{stderr}");
    assert!(stderr.contains("not a valid range"), "{stderr}");
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        manifest.as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("tapid.lock")).unwrap(),
        b"old lock bytes\n"
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"user data"
    );
    assert_eq!(
        fs::read(dir.join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert!(!store.exists());
    assert!(!dir.join(".tapid-lifecycle-journal.json").exists());
    cleanup(dir);
}

#[test]
fn add_peer_records_only_peer_requirement() {
    let dir = temp_dir("peer-cli-transaction");
    let manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    let fixture = dir.join("registry.json");
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://jsr.io","name":"@scope/peer","version":"1.0.0","artifact":"https://jsr.io/@scope/peer/1.0.0.tgz"}]}"#,
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "add",
            "--store-dir",
            dir.join("store").to_str().unwrap(),
            "react@^18.0.0",
            "--peer",
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let updated = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(updated.contains("\"peerDependencies\""));
    assert!(updated.contains("\"react\": \"^18.0.0\""));
    assert!(!updated.contains("\"dependencies\""));
    assert!(dir.join("tapid.lock").is_file());
    cleanup(dir);
}

#[test]
fn install_validates_peer_providers_and_persists_peer_context() {
    let dir = temp_dir("peer-context-install");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0","react":"18.2.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}","peerDependencies":{{"react":"^18.0.0"}}}},{{"registry":"https://registry.npmjs.org","name":"react","version":"18.2.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    let packages = lock["packages"].as_object().unwrap();
    assert!(
        packages
            .keys()
            .any(|key| { key.contains("|plugin@1.0.0|peer=name=react;version=18.2.0|") })
    );
    assert!(
        packages
            .keys()
            .any(|key| key.contains("|react@18.2.0|peer=-|"))
    );
    fs::remove_dir_all(dir.join("node_modules")).unwrap();
    let replay = run(
        &dir,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replayed_lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("tapid.lock")).unwrap()).unwrap();
    assert!(
        replayed_lock["packages"]
            .as_object()
            .unwrap()
            .keys()
            .any(|key| { key.contains("|plugin@1.0.0|peer=name=react;version=18.2.0|") })
    );
    cleanup(dir);
}

#[test]
fn install_rolls_back_when_a_required_peer_provider_is_missing() {
    use tapid_store::Store;

    let dir = temp_dir("peer-context-rollback");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join("tapid.lock"), "old lock\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/sentinel"), "keep").unwrap();
    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let source = dir.join("prior-store-tree");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("package.json"), "prior store tree").unwrap();
    let digest = tapid_archive::canonical_tree_digest(&source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store.activate_verified_tree(&digest, &source).unwrap();
    let trees_before = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","artifact":"{artifact}","peerDependencies":{{"react":"^18.0.0"}}}}]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("peer dependency"));
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        "old lock\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/sentinel")).unwrap(),
        "keep"
    );
    assert!(store.verified_tree_path(&digest).is_ok());
    let trees_after = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(trees_after, trees_before);
    cleanup(dir);
}

#[test]
fn invalid_fetched_dependency_range_does_not_create_store_state() {
    let dir = temp_dir("invalid-fetched-range-no-store");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let fixture = dir.join("registry.json");
    fs::write(
        &fixture,
        r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"parent","version":"1.0.0","artifact":"unused","dependencies":{"broken":"not a valid range"}}]}"#,
    )
    .unwrap();
    let store = dir.join("previously-nonexistent-store");

    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dependency broken"), "{stderr}");
    assert!(stderr.contains("not a valid range"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn invalid_online_root_range_does_not_create_store_state() {
    let dir = temp_dir("invalid-range-no-store");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"broken":"not a valid range"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    let store = dir.join("previously-nonexistent-store");

    let output = run(&dir, &["install", "--store-dir", store.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("broken"), "{stderr}");
    assert!(stderr.contains("not a valid range"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert!(!dir.join("tapid.lock").exists());
    assert!(!dir.join("node_modules").exists());
    assert!(!store.exists());
    cleanup(dir);
}

#[test]
fn install_rolls_back_when_a_required_peer_is_incompatible() {
    let dir = temp_dir("peer-context-incompatible-rollback");
    let manifest =
        r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0","react":"18.2.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join("tapid.lock"), "old lock\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/sentinel"), "keep").unwrap();
    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","artifact":"{artifact}","peerDependencies":{{"react":"^19.0.0"}}}},{{"registry":"https://registry.npmjs.org","name":"react","version":"18.2.0","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let store = dir.join("store");
    let output = run(
        &dir,
        &[
            "install",
            "--allow-unverified-registry-artifacts",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("peer dependency"));
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        "old lock\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/sentinel")).unwrap(),
        "keep"
    );
    cleanup(dir);
}

fn run_lifecycle_recovery_crash_case(crash_point: &str) {
    use tapid_store::Store;

    let dir = temp_dir("lifecycle-crash-recovery");
    let original_manifest = r#"{"name":"demo","version":"1.0.0"}"#;
    fs::write(dir.join("package.json"), original_manifest).unwrap();
    let original_lock = lock_for_manifest(original_manifest).to_json().unwrap();
    fs::write(dir.join("tapid.lock"), &original_lock).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
    fs::write(dir.join("node_modules/KEEP"), "previous activation").unwrap();

    let store_dir = dir.join("store");
    let store = Store::new(&store_dir);
    let prior_source = dir.join("prior-store-tree");
    fs::create_dir_all(&prior_source).unwrap();
    fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
    let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
        .unwrap()
        .parse::<tapid_core::ArtifactDigest>()
        .unwrap();
    store
        .activate_verified_tree(&prior_digest, &prior_source)
        .unwrap();
    let trees_before = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let fixture = dir.join("registry.json");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let crash = run_with_env(
        &dir,
        &[
            "add",
            "plugin@1.0.0",
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
        "TAPID_TEST_CRASH_POINT",
        crash_point,
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            crash.status.signal(),
            Some(6),
            "crash hook did not abort: {}",
            String::from_utf8_lossy(&crash.stderr)
        );
    }
    #[cfg(not(unix))]
    assert!(
        !crash.status.success(),
        "crash hook did not terminate process"
    );

    let recovery = run(
        &dir,
        &["outdated", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        recovery.status.success(),
        "recovery failed: {}",
        String::from_utf8_lossy(&recovery.stderr)
    );
    let trees_after = fs::read_dir(store_dir.join("trees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    if crash_point == "commit_decision" {
        assert_ne!(
            fs::read(dir.join("package.json")).unwrap(),
            original_manifest.as_bytes()
        );
        assert_ne!(
            fs::read(dir.join("tapid.lock")).unwrap(),
            original_lock.as_bytes()
        );
        assert!(!dir.join("node_modules/KEEP").exists());
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        assert!(trees_after.len() > trees_before.len());
        assert!(!dir.join(".tapid-lifecycle-journal.json").exists());
        assert!(fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tapid-node-modules-old-")
        }));
    } else {
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            original_manifest.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("tapid.lock")).unwrap(),
            original_lock.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"previous activation"
        );
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        assert_eq!(trees_after, trees_before);
    }
    cleanup(dir);
}

#[test]
fn lifecycle_recovers_previous_state_after_node_modules_backup_crash() {
    run_lifecycle_recovery_crash_case("node_modules_backed_up");
}

#[test]
fn lifecycle_recovers_previous_state_after_store_publication_crash() {
    run_lifecycle_recovery_crash_case("store_published");
}

#[test]
fn lifecycle_recovers_previous_state_after_lockfile_replacement_crash() {
    run_lifecycle_recovery_crash_case("lockfile_replaced");
}

#[test]
fn lifecycle_recovers_previous_state_after_activation_completion_crash() {
    run_lifecycle_recovery_crash_case("activation_complete");
}

#[test]
fn lifecycle_finishes_committed_state_after_commit_decision_crash() {
    run_lifecycle_recovery_crash_case("commit_decision");
}

#[test]
fn install_does_not_commit_verified_store_trees_when_materialization_fails() {
    let dir = temp_dir("store-rollback-on-activation");
    let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
    fs::write(dir.join("package.json"), manifest).unwrap();
    fs::write(dir.join("tapid.lock"), "old lock bytes\n").unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();
    let fixture = dir.join("registry.json");
    let store = dir.join("store");
    let artifact = "base64:H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";
    let integrity = "sha512-Z12EKCpZh3kuBL3pKV8o2ZuPciIuehb1HyMTRvu6Al6OCWioeFUYjtqd4t0Hr2/7GRSqyuzJ99duHhJSIFKIZQ==";
    fs::write(
        &fixture,
        format!(
            r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"{artifact}"}}]}}"#
        ),
    )
    .unwrap();

    let output = run(
        &dir,
        &[
            "install",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unmarked node_modules"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        "old lock bytes\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("node_modules/KEEP")).unwrap(),
        "user data"
    );
    assert!(!store.join("trees").exists());
    cleanup(dir);
}

#[test]
fn install_preserves_project_and_store_on_integrity_and_archive_failures() {
    use tapid_store::Store;

    for failure in ["integrity", "archive"] {
        let dir = temp_dir(&format!("rollback-{failure}"));
        let manifest = r#"{"name":"demo","version":"1.0.0","dependencies":{"plugin":"1.0.0"}}"#;
        fs::write(dir.join("package.json"), manifest).unwrap();
        fs::write(dir.join("tapid.lock"), "old lock bytes\n").unwrap();
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();
        fs::write(dir.join("node_modules/KEEP"), "user data").unwrap();

        let store_dir = dir.join("store");
        let store = Store::new(&store_dir);
        let prior_source = dir.join("prior-store-tree");
        fs::create_dir_all(&prior_source).unwrap();
        fs::write(prior_source.join("package.json"), "prior store tree").unwrap();
        let prior_digest = tapid_archive::canonical_tree_digest(&prior_source)
            .unwrap()
            .parse::<tapid_core::ArtifactDigest>()
            .unwrap();
        store
            .activate_verified_tree(&prior_digest, &prior_source)
            .unwrap();
        let trees_before = fs::read_dir(store_dir.join("trees"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();

        let archive = if failure == "integrity" {
            STANDARD.decode("H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA").unwrap()
        } else {
            b"not a gzip tar archive".to_vec()
        };
        let integrity = if failure == "integrity" {
            format!("sha512-{}", STANDARD.encode([0_u8; 64]))
        } else {
            format!("sha512-{}", STANDARD.encode(Sha512::digest(&archive)))
        };
        let fixture = dir.join("registry.json");
        fs::write(
            &fixture,
            format!(
                r#"{{"packages":[{{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","integrity":"{integrity}","artifact":"base64:{}"}}]}}"#,
                STANDARD.encode(&archive)
            ),
        )
        .unwrap();

        let output = run(
            &dir,
            &[
                "install",
                "--store-dir",
                store_dir.to_str().unwrap(),
                "--registry-fixture",
                fixture.to_str().unwrap(),
            ],
        );

        assert!(!output.status.success(), "{failure} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if failure == "integrity" {
            assert!(stderr.contains("integrity"), "unexpected error: {stderr}");
        } else {
            assert!(
                stderr.contains("invalid gzip header"),
                "unexpected error: {stderr}"
            );
        }
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(
            fs::read(dir.join("tapid.lock")).unwrap(),
            b"old lock bytes\n"
        );
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"user data"
        );
        assert_eq!(
            fs::read(dir.join(".tapid-managed")).unwrap(),
            b"tapid-managed-v1\n"
        );
        assert!(store.verified_tree_path(&prior_digest).is_ok());
        let trees_after = fs::read_dir(store_dir.join("trees"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            trees_after, trees_before,
            "store changed after {failure} failure"
        );
        cleanup(dir);
    }
}

#[test]
fn upgrade_is_exposed_as_a_cli_command() {
    let dir = temp_dir("upgrade-exposed");
    let output = run(&dir, &["--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("upgrade"));
    cleanup(dir);
}

fn lock_for_manifest(raw: &str) -> Lockfile {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = format!("sha256-{}", hex::encode(hasher.finalize()));
    Lockfile::new(&digest).unwrap()
}

fn write_prune_fixture(dir: &Path, manifest: &str, mismatch: bool) -> (PathBuf, String, String) {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    use tapid_store::Store;

    fs::write(dir.join("package.json"), manifest).unwrap();
    let store_root = dir.join("store");
    let store = Store::new(&store_root);
    let make_tree = |name: &str| {
        let source = dir.join(format!("source-{name}"));
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
        let digest = tapid_archive::canonical_tree_digest(&source).unwrap();
        let parsed = digest.parse::<tapid_core::ArtifactDigest>().unwrap();
        store.activate_verified_tree(&parsed, &source).unwrap();
        digest
    };
    let required_digest = make_tree("required");
    let orphan_digest = make_tree("orphan");
    let mut lock = lock_for_manifest(if mismatch {
        r#"{"name":"app","version":"1.0.0","dependencies":{"required":"2.0.0"}}"#
    } else {
        manifest
    });
    let required = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "required",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &required_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let orphan = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "orphan",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &orphan_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let required_key = required.key();
    lock.insert_package(required).unwrap();
    lock.insert_package(orphan).unwrap();
    lock.set_roots([required_key]).unwrap();
    fs::write(dir.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    (store_root, required_digest, orphan_digest)
}

#[test]
fn prune_preserves_workspace_member_peer_provider_and_removes_orphan() {
    use tapid_core::{PackageName, PackageVersion, PeerContext, PlatformContext};
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    use tapid_store::Store;

    let dir = temp_dir("prune-workspace-peer");
    let root_manifest = r#"{"name":"root","version":"1.0.0","workspaces":["packages/*"]}"#;
    let member_manifest =
        r#"{"name":"web","version":"1.0.0","dependencies":{"plugin":"1.0.0","react":"18.2.0"}}"#;
    fs::write(dir.join("package.json"), root_manifest).unwrap();
    let member = dir.join("packages/web");
    fs::create_dir_all(&member).unwrap();
    fs::write(member.join("package.json"), member_manifest).unwrap();
    let store_dir = member.join("store");
    let store = Store::new(&store_dir);
    let make_tree = |name: &str| {
        let source = member.join(format!("source-{name}"));
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
        let digest = tapid_archive::canonical_tree_digest(&source).unwrap();
        let parsed = digest.parse::<tapid_core::ArtifactDigest>().unwrap();
        store.activate_verified_tree(&parsed, &source).unwrap();
        digest
    };
    let plugin_digest = make_tree("plugin");
    let react_digest = make_tree("react");
    let orphan_digest = make_tree("orphan");
    let peer_context = PeerContext::default().with(
        "react".parse::<PackageName>().unwrap(),
        "18.2.0".parse::<PackageVersion>().unwrap(),
    );
    let platform_context = PlatformContext::new(None, None, None).unwrap();
    let integrity = format!("sha512-{}==", "A".repeat(86));
    let plugin = LockedPackage::new_with_context_and_provenance(
        "https://registry.npmjs.org",
        "plugin",
        "1.0.0",
        &integrity,
        &plugin_digest,
        (&peer_context, &platform_context),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let react = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "react",
        "18.2.0",
        &integrity,
        &react_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let orphan = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "orphan",
        "1.0.0",
        &integrity,
        &orphan_digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let plugin_key = plugin.key();
    let react_key = react.key();
    let mut lock = lock_for_manifest(member_manifest);
    lock.insert_package(plugin).unwrap();
    lock.insert_package(react).unwrap();
    lock.insert_package(orphan).unwrap();
    lock.set_roots([plugin_key, react_key]).unwrap();
    fs::write(member.join("tapid.lock"), lock.to_json().unwrap()).unwrap();
    fs::create_dir_all(member.join("node_modules/old")).unwrap();
    fs::write(member.join("node_modules/old/file"), "old").unwrap();
    fs::write(member.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();

    let output = run(
        &dir,
        &[
            "prune",
            "--workspace",
            "web",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(member.join("node_modules/plugin/package.json").is_file());
    assert!(member.join("node_modules/react/package.json").is_file());
    assert!(!member.join("node_modules/orphan").exists());
    assert!(!member.join("node_modules/old").exists());
    assert_eq!(
        fs::read(dir.join("package.json")).unwrap(),
        root_manifest.as_bytes()
    );
    cleanup(dir);
}

#[test]
fn prune_removes_only_unreachable_package_from_managed_node_modules() {
    let dir = temp_dir("prune-reachable");
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"required":"1.0.0"}}"#;
    let (store, _, _) = write_prune_fixture(&dir, manifest, false);
    let lock_before = fs::read_to_string(dir.join("tapid.lock")).unwrap();
    fs::create_dir_all(dir.join("node_modules/old")).unwrap();
    fs::write(dir.join("node_modules/old/file"), "old").unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();

    let output = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/required/package.json").is_file());
    assert!(!dir.join("node_modules/orphan").exists());
    assert!(!dir.join("node_modules/old").exists());
    assert_eq!(
        fs::read_to_string(dir.join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        lock_before
    );
    cleanup(dir);
}

#[test]
fn prune_preserves_unmarked_node_modules() {
    let dir = temp_dir("prune-unmarked");
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"required":"1.0.0"}}"#;
    let (store, _, _) = write_prune_fixture(&dir, manifest, false);
    fs::create_dir_all(dir.join("node_modules/old")).unwrap();
    fs::write(dir.join("node_modules/old/file"), "old").unwrap();

    let output = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unmarked node_modules"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/old/file").is_file());
    cleanup(dir);
}

#[test]
fn prune_preserves_tree_on_lockfile_manifest_mismatch() {
    let dir = temp_dir("prune-mismatch");
    let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"required":"1.0.0"}}"#;
    let (store, _, _) = write_prune_fixture(&dir, manifest, true);
    fs::create_dir_all(dir.join("node_modules/old")).unwrap();
    fs::write(dir.join("node_modules/old/file"), "old").unwrap();
    fs::write(dir.join(".tapid-managed"), "tapid-managed-v1\n").unwrap();

    let output = run(&dir, &["prune", "--store-dir", store.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("manifest digest mismatch"));
    assert!(dir.join("node_modules/old/file").is_file());
    cleanup(dir);
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
fn run_requires_checked_in_configuration_before_execution() {
    let dir = temp_dir("run-missing-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: required run configuration is missing: tapid.toml\n"
    );
    assert!(output.stdout.is_empty());
    cleanup(dir);
}

#[test]
fn run_accepts_process_memory_stats_opt_in_and_aliases() {
    let dir = temp_dir("run-process-memory-stats-arg");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"build":"exit 0"}}"#,
    )
    .unwrap();

    for flag in [
        "--allow-process-memory-stats",
        "--allow-procfs",
        "--allow-memory-read",
    ] {
        let output = run(
            &dir,
            &[
                "run",
                "build",
                flag,
                "--node-runtime",
                env!("CARGO_BIN_EXE_tapid"),
            ],
        );
        assert_eq!(output.status.code(), Some(1), "flag {flag}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "error: required run configuration is missing: tapid.toml\n",
            "flag {flag} was not parsed as a Tapid option"
        );
        assert!(output.stdout.is_empty());
    }
    cleanup(dir);
}

#[cfg(target_os = "linux")]
#[test]
fn run_prints_libuv_process_memory_opt_in_hint_once() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("run-process-memory-stats-hint");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"build":"node"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.defaults]\nassurance = \"restricted\"\nread = [\".\"]\nsubprocess = true\n\n[run.scripts.build]\n",
    )
    .unwrap();
    let runtime_dir = dir.join("runtime");
    fs::create_dir(&runtime_dir).unwrap();
    let fake_node = runtime_dir.join("node");
    fs::write(
        &fake_node,
        "#!/bin/sh\nprintf '%s\\n' 'RUN_MARKER [Error: EACCES: permission denied, uv_resident_set_memory]' >&2\nexit 19\n",
    )
    .unwrap();
    fs::set_permissions(&fake_node, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run(
        &dir,
        &[
            "run",
            "build",
            "--node-runtime",
            fake_node.to_str().unwrap(),
        ],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(19), "{stderr}");
    assert_eq!(stderr.matches("RUN_MARKER").count(), 1, "{stderr}");
    assert_eq!(
        stderr
            .matches("retry with --allow-process-memory-stats")
            .count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains("aliases: --allow-memory-read, --allow-procfs"),
        "{stderr}"
    );
    cleanup(dir);
}

#[cfg(target_os = "linux")]
#[test]
fn run_memory_stats_opt_in_uses_private_procfs_or_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("run-process-memory-stats-opt-in");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"probe":"node"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.defaults]\nassurance = \"restricted\"\nread = [\".\"]\nsubprocess = true\nenvironment = [\"TAPID_TEST_HOST_PID\"]\n\n[run.scripts.probe]\n",
    )
    .unwrap();
    let runtime_dir = dir.join("runtime");
    fs::create_dir(&runtime_dir).unwrap();
    let fake_node = runtime_dir.join("node");
    fs::write(
        &fake_node,
        "#!/bin/sh\n/bin/cat /proc/self/statm\nstatus=$?\nif [ \"$status\" -ne 0 ]; then exit \"$status\"; fi\nif [ -r \"/proc/$TAPID_TEST_HOST_PID/statm\" ]; then exit 43; fi\nif printf x > /proc/self/comm; then exit 44; fi\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&fake_node, fs::Permissions::from_mode(0o755)).unwrap();

    let host_pid = std::process::id().to_string();
    let arguments = [
        "run",
        "probe",
        "--allow-process-memory-stats",
        "--node-runtime",
        fake_node.to_str().unwrap(),
    ];
    let is_root = unsafe { libc::geteuid() == 0 };
    let can_use_sudo = !is_root
        && Command::new("sudo")
            .args(["-n", "true"])
            .output()
            .is_ok_and(|output| output.status.success());
    let output = if can_use_sudo {
        Command::new("sudo")
            .args(["-n", "--", "env"])
            .arg(format!("TAPID_TEST_HOST_PID={host_pid}"))
            .arg(env!("CARGO_BIN_EXE_tapid"))
            .args(arguments)
            .current_dir(&dir)
            .output()
            .unwrap()
    } else {
        run_with_env(&dir, &arguments, "TAPID_TEST_HOST_PID", &host_pid)
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && stderr.contains("unsupported-containment") {
        assert!(
            stdout.is_empty(),
            "child ran before fail-closed rejection: {stdout}"
        );
        cleanup(dir);
        return;
    }
    assert!(output.status.success(), "stdout={stdout} stderr={stderr}");
    let stats = stdout.split_whitespace().collect::<Vec<_>>();
    assert!(!stats.is_empty(), "expected process stats, stdout={stdout}");
    assert!(
        stats.iter().all(|field| field.parse::<u64>().is_ok()),
        "stdout={stdout}"
    );
    assert!(
        stderr.contains("private PID and mount namespaces"),
        "{stderr}"
    );
    assert!(stderr.contains("procfs read-only"), "{stderr}");
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn run_rejects_a_non_regular_configuration_without_opening_it() {
    let dir = temp_dir("run-special-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    let status = Command::new("mkfifo")
        .arg(dir.join("tapid.toml"))
        .status()
        .unwrap();
    assert!(status.success());

    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot read run configuration 'tapid.toml'")
    );
    cleanup(dir);
}

#[test]
fn run_without_runtime_flag_discovers_node_then_reaches_sandbox_preflight() {
    let dir = temp_dir("run-discovered-runtime");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev]\n").unwrap();
    let runtime_dir = dir.join("host-runtime");
    fs::create_dir(&runtime_dir).unwrap();
    let runtime = runtime_dir.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &runtime).unwrap();

    let output = run_with_isolated_path(
        &dir,
        &[
            "run",
            "dev",
            "--",
            "--hostname",
            "127.0.0.1",
            "--port",
            "3001",
        ],
        runtime_dir.as_os_str(),
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sandbox execution failed (unsupported-containment)"),
        "{stderr}"
    );
    assert!(!stderr.contains("required arguments were not provided"));
    cleanup(dir);
}

#[cfg(unix)]
#[test]
fn run_preserves_non_utf8_forwarded_argument_through_cli_boundary() {
    use std::os::unix::ffi::OsStringExt;

    let dir = temp_dir("run-non-utf8-argument");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev]\n").unwrap();
    let runtime = dir.join("node");
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &runtime).unwrap();
    let bad = std::ffi::OsString::from_vec(b"bad-\xff-arg".to_vec());

    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .current_dir(&dir)
        .env_clear()
        .args(["run", "dev", "--node-runtime"])
        .arg(&runtime)
        .arg("--")
        .arg(&bad)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported-containment"), "{stderr}");
    assert!(!stderr.contains("invalid UTF-8"));
    cleanup(dir);
}

#[test]
fn run_rejects_malformed_configuration_stably() {
    let dir = temp_dir("run-malformed-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev\nnetwork = true").unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: invalid run configuration (malformed)\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_oversized_configuration_before_parsing() {
    let dir = temp_dir("run-oversized-config");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        vec![b' '; tapid_runner::MAX_CONFIG_BYTES + 1],
    )
    .unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: invalid run configuration (capacity-exceeded)\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_oversized_manifest_before_policy_loading() {
    let dir = temp_dir("run-oversized-manifest");
    fs::write(dir.join("package.json"), vec![b' '; 1_048_577]).unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: manifest exceeds maximum size of 1048576 bytes\n"
    );
    cleanup(dir);
}

#[test]
fn run_requires_an_exact_script_profile() {
    let dir = temp_dir("run-missing-profile");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.defaults]\nnetwork = false\n").unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: run policy profile is missing for script: dev\n"
    );
    cleanup(dir);
}

#[test]
fn run_fails_closed_before_spawn_without_printing_secret_values() {
    let dir = temp_dir("run-unsupported");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"printf spawned > SHOULD_NOT_EXIST"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.scripts.dev]\nenvironment = [\"SECRET_TOKEN\"]\n",
    )
    .unwrap();
    let secret = "tapid-super-secret-value";
    let runtime = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::copy(env!("CARGO_BIN_EXE_tapid"), &runtime).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args([
            OsStr::new("run"),
            OsStr::new("dev"),
            OsStr::new("--node-runtime"),
            runtime.as_os_str(),
            OsStr::new("--"),
            OsStr::new("--hostname"),
            OsStr::new("127.0.0.1"),
            OsStr::new("--port"),
            OsStr::new("4173"),
        ])
        .current_dir(&dir)
        .env("SECRET_TOKEN", secret)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sandbox execution failed (unsupported-containment)"));
    assert!(stderr.contains("no process was started"));
    assert!(stderr.contains("no enforcement receipt was issued"));
    assert!(!stderr.contains(secret));
    assert!(!dir.join("SHOULD_NOT_EXIST").exists());
    cleanup(dir);
}

#[test]
fn run_rejects_reserved_path_allowlisting_before_runner_execution() {
    let dir = temp_dir("run-path-reserved");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tapid.toml"),
        "[run.scripts.dev]\nenvironment = [\"PATH\"]\n",
    )
    .unwrap();
    let output = run(
        &dir,
        &["run", "dev", "--node-runtime", env!("CARGO_BIN_EXE_tapid")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: run policy cannot allowlist reserved environment variable PATH\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_a_missing_node_runtime_stably() {
    let dir = temp_dir("run-runtime-missing");
    fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{"dev":"exit 0"}}"#,
    )
    .unwrap();
    fs::write(dir.join("tapid.toml"), "[run.scripts.dev]\n").unwrap();
    let output = run(&dir, &["run", "dev", "--node-runtime", "missing-node"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: node runtime must be an executable named node or node.exe\n"
    );
    cleanup(dir);
}

#[test]
fn run_rejects_missing_script_stably_before_policy_loading() {
    let dir = temp_dir("run-missing-script");
    fs::write(
        dir.join("package.json"),
        r#"{"name":"demo","version":"1.0.0","scripts":{}}"#,
    )
    .unwrap();
    let missing = run(
        &dir,
        &[
            "run",
            "missing",
            "--node-runtime",
            env!("CARGO_BIN_EXE_tapid"),
        ],
    );
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
fn legacy_registry_replay_rejects_without_mutation() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    for mode in ["--offline", "--frozen"] {
        let dir = temp_dir("legacy-registry");
        let manifest = r#"{"name":"app","version":"1.0.0","dependencies":{"demo":"1.0.0"}}"#;
        fs::write(dir.join("package.json"), manifest).unwrap();
        let mut lock = lock_for_manifest(manifest);
        let package = LockedPackage::new_with_provenance(
            "https://registry.example.test",
            "demo",
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &format!("sha256-{}", "b".repeat(64)),
            RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        let key = package.key();
        lock.insert_package(package).unwrap();
        lock.set_roots([key]).unwrap();
        let old = lock.to_json().unwrap().replace(
            "https://registry.example.test",
            "https://REGISTRY.example.test:443",
        );
        fs::write(dir.join("tapid.lock"), &old).unwrap();
        let store = dir.join("store");
        fs::create_dir(&store).unwrap();
        fs::write(store.join("sentinel"), "store unchanged").unwrap();
        fs::create_dir(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules/sentinel"), "layout unchanged").unwrap();
        let output = run(
            &dir,
            &["install", mode, "--store-dir", store.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("noncanonical persisted registry identity"),
            "{error}"
        );
        assert!(
            error.contains("backup") && error.contains("online"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(dir.join("tapid.lock")).unwrap(), old);
        assert_eq!(
            fs::read_to_string(dir.join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(store.join("sentinel")).unwrap(),
            "store unchanged"
        );
        assert_eq!(fs::read_dir(&store).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(dir.join("node_modules/sentinel")).unwrap(),
            "layout unchanged"
        );
        assert_eq!(fs::read_dir(dir.join("node_modules")).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            4,
            "rejection must not acquire activation state"
        );
        cleanup(dir);
    }
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
    // Exercise the documented explicit recovery using a graph with exact roots
    // and transitive edges, preserving the incompatible lock independently.
    let canonical_lock = fs::read_to_string(dir.join("tapid.lock")).unwrap();
    let legacy_lock = canonical_lock.replace(
        "https://registry.npmjs.org",
        "https://REGISTRY.npmjs.org:443",
    );
    fs::write(dir.join("tapid.lock"), &legacy_lock).unwrap();
    fs::copy(dir.join("tapid.lock"), dir.join("tapid.lock.preserved")).unwrap();
    let rejected = run(&dir, &["install", "--frozen"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("noncanonical persisted registry identity")
    );
    let recovered = run(
        &dir,
        &["install", "--registry-fixture", fixture.to_str().unwrap()],
    );
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock.preserved")).unwrap(),
        legacy_lock
    );
    assert_eq!(
        fs::read_to_string(dir.join("tapid.lock")).unwrap(),
        canonical_lock
    );
    assert!(run(&dir, &["lock", "verify"]).status.success());
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
