use std::{fs, process::Command};
use tapid_test_support::{TempHome, TempProject};

#[test]
fn npm_import_update_converts_to_a_native_lock() {
    let project = TempProject::new("npm-import-update-empty").unwrap();
    let home = TempHome::new("npm-import-update-empty").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project
        .write(
            "package-lock.json",
            br#"{"lockfileVersion":3,"packages":{"":{"name":"example","version":"1.0.0"}}}"#,
        )
        .unwrap();
    project
        .write(
            "registry.json",
            include_bytes!("fixtures/npm-import/registry.json"),
        )
        .unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path())
            .env("LOCALAPPDATA", home.path())
            .env_remove("TAPID_NPM_TOKEN")
            .current_dir(project.path())
            .output()
            .unwrap()
    };
    let imported = invoke(&["import-package-lock", "package-lock.json"]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let store = project.path().join("store");
    let updated = invoke(&[
        "update",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        store.to_str().unwrap(),
    ]);
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let lock = tapid_lockfile::Lockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    assert!(lock.packages().is_empty());
    let replayed = invoke(&[
        "install",
        "--offline",
        "--frozen",
        "--store-dir",
        store.to_str().unwrap(),
    ]);
    assert!(
        replayed.status.success(),
        "{}",
        String::from_utf8_lossy(&replayed.stderr)
    );
}

#[test]
fn npm_import_empty_project_is_offline_and_deterministic() {
    let project = TempProject::new("npm-import-empty").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project.write("package-lock.json", br#"{"name":"example","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"example","version":"1.0.0"}}}"#).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(["import-package-lock", "package-lock.json"])
            .current_dir(project.path())
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let first = fs::read(project.path().join("tapid.lock")).unwrap();
    assert!(run().status.success());
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), first);
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 4);
}

#[test]
fn npm_import_frozen_installs_an_empty_graph_without_a_registry() {
    let project = TempProject::new("npm-import-frozen-empty").unwrap();
    project
        .write("package.json", br#"{"name":"example","version":"1.0.0"}"#)
        .unwrap();
    project
        .write(
            "package-lock.json",
            br#"{"lockfileVersion":3,"packages":{"":{"name":"example","version":"1.0.0"}}}"#,
        )
        .unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .current_dir(project.path())
            .output()
            .unwrap()
    };
    assert!(
        invoke(&["import-package-lock", "package-lock.json"])
            .status
            .success()
    );
    let store = project.path().join("store");
    let output = invoke(&[
        "install",
        "--json",
        "--frozen",
        "--store-dir",
        store.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["lifecycle"]["coverage"], "not_performed");
    assert_eq!(result["lifecycle"]["reason"], "imported_lock");
    assert_eq!(result["lifecycle"]["hooks_executed"], 0);
    assert!(project.path().join("node_modules").is_dir());
}

fn reference_project(label: &str) -> TempProject {
    let project = TempProject::new(label).unwrap();
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
    project
}
fn invoke(project: &TempProject, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tapid"))
        .args(args)
        .current_dir(project.path())
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn npm_import_rejects_symlinked_manifest_without_mutation() {
    let project = reference_project("npm-import-symlinked-manifest");
    let manifest_path = project.path().join("package.json");
    let manifest = fs::read(&manifest_path).unwrap();
    project.write("manifest-target.json", &manifest).unwrap();
    fs::remove_file(&manifest_path).unwrap();
    std::os::unix::fs::symlink("manifest-target.json", &manifest_path).unwrap();
    project.write("tapid.lock", b"prior lock bytes").unwrap();
    project
        .write("node_modules/keep.txt", b"prior active tree")
        .unwrap();
    project.write("store/keep.txt", b"prior store").unwrap();

    let output = invoke(&project, &["import-package-lock", "package-lock.json"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("package.json must be a regular, non-symlink file"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        b"prior lock bytes"
    );
    assert_eq!(
        fs::read(project.path().join("node_modules/keep.txt")).unwrap(),
        b"prior active tree"
    );
    assert_eq!(
        fs::read(project.path().join("store/keep.txt")).unwrap(),
        b"prior store"
    );
    assert_eq!(
        fs::read(project.path().join("manifest-target.json")).unwrap(),
        manifest
    );
    assert!(
        fs::symlink_metadata(manifest_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn npm_import_revalidates_routing_before_cached_or_fixture_artifacts() {
    for cached in [false, true] {
        let project = reference_project("npm-import-routing");
        assert!(
            invoke(&project, &["import-package-lock", "package-lock.json"])
                .status
                .success()
        );
        let store = project.path().join("store");
        let install = |offline| {
            let mut args = vec![
                "install",
                "--frozen",
                "--store-dir",
                store.to_str().unwrap(),
            ];
            if offline {
                args.push("--offline");
            } else {
                args.extend(["--registry-fixture", "registry.json"]);
            }
            invoke(&project, &args)
        };
        if cached {
            let output = install(false);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let lock = fs::read(project.path().join("tapid.lock")).unwrap();
        let manifest = fs::read(project.path().join("package.json")).unwrap();
        project
            .write("node_modules/keep.txt", b"prior active tree")
            .unwrap();
        project
            .write(".tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        project.write("store/keep.txt", b"prior store").unwrap();
        project.write("tapid.toml", b"[registries.default]\nurl = \"https://packages.example.invalid\"\ntoken-env = \"TAPID_IMPORT_ROUTE_TEST_TOKEN\"\n").unwrap();
        for offline in [false, true] {
            let output = install(offline);
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("registry identity mismatch"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
            assert_eq!(
                fs::read(project.path().join("package.json")).unwrap(),
                manifest
            );
            assert_eq!(
                fs::read(project.path().join("node_modules/keep.txt")).unwrap(),
                b"prior active tree"
            );
            assert_eq!(
                fs::read(project.path().join("store/keep.txt")).unwrap(),
                b"prior store"
            );
        }
        if cached {
            // Routing validation does not require credentials for store-only replay.
            project.write("tapid.toml", b"[registries.default]\nurl = \"https://registry.npmjs.org\"\ntoken-env = \"TAPID_IMPORT_ROUTE_TEST_TOKEN\"\n").unwrap();
            let output = install(true);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
        }
    }
}

#[test]
fn npm_import_rejects_replay_after_imported_integrity_changes() {
    let project = reference_project("npm-import-changed-integrity");
    assert!(
        invoke(&project, &["import-package-lock", "package-lock.json"])
            .status
            .success()
    );
    let store = project.path().join("store");
    let output = invoke(
        &project,
        &[
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("tapid.lock")).unwrap()).unwrap();
    lock["npmLock"]["packages"]["node_modules/shared"]["integrity"] =
        lock["npmLock"]["packages"]["node_modules/parent"]["integrity"].clone();
    project
        .write("tapid.lock", lock.to_string().as_bytes())
        .unwrap();

    let output = invoke(
        &project,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("verifiedArtifacts"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn npm_import_preserves_nested_versions_peers_sources_and_optional_constraints() {
    let project = reference_project("npm-import-reference");
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let npm_lock = fs::read(project.path().join("package-lock.json")).unwrap();
    let backup = TempProject::new("npm-installation-backup").unwrap();
    project
        .write("node_modules/original", b"original npm installation")
        .unwrap();
    fs::rename(
        project.path().join("node_modules"),
        backup.path().join("saved-tree"),
    )
    .unwrap();
    assert!(!project.path().join("tapid.lock").exists());
    let output = invoke(&project, &["import-package-lock", "package-lock.json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(invoke(&project, &["lock", "verify"]).status.success());
    let imported_bytes = fs::read_to_string(project.path().join("tapid.lock")).unwrap();
    assert_eq!(
        imported_bytes,
        include_str!("fixtures/npm-import/tapid.lock")
    );
    let imported = tapid_lockfile::ImportedNpmLockfile::from_json(&imported_bytes).unwrap();
    let graph = imported.graph().unwrap();
    assert_eq!(
        graph.packages["node_modules/shared"].version.to_string(),
        "1.0.0"
    );
    assert_eq!(
        graph.packages["node_modules/parent"].dependencies["shared"],
        "node_modules/parent/node_modules/shared"
    );
    assert_eq!(
        graph.packages["node_modules/consumer"].peers["shared"],
        "node_modules/shared"
    );
    assert!(graph.packages["node_modules/native"].optional);
    let store = project.path().join("store");
    let output = invoke(
        &project,
        &[
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let imported = tapid_lockfile::ImportedNpmLockfile::from_json(
        &fs::read_to_string(project.path().join("tapid.lock")).unwrap(),
    )
    .unwrap();
    assert_eq!(imported.graph().unwrap().packages, graph.packages);
    assert!(imported.verified_tree("node_modules/shared").is_some());
    assert!(
        imported
            .verified_tree("node_modules/parent/node_modules/shared")
            .is_some()
    );
    assert!(imported.verified_tree("node_modules/native").is_none());
    assert!(!project.path().join("node_modules/native").exists());
    for (name, version) in [
        ("parent", "1.0.0"),
        ("shared", "1.0.0"),
        ("consumer", "1.0.0"),
    ] {
        let installed: serde_json::Value = serde_json::from_slice(
            &fs::read(
                project
                    .path()
                    .join(format!("node_modules/{name}/package.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(installed["version"], version);
    }
    // Node follows the planned nested dependency and peer edges.
    if let Ok(output) = Command::new("node").args(["-e", "const assert = require('node:assert'); const {createRequire} = require('node:module'); const p = createRequire(require.resolve('parent')); const c = createRequire(require.resolve('consumer')); assert.equal(p('shared'), 'shared@2.0.0'); assert.equal(c('shared'), 'shared@1.0.0');"]).current_dir(project.path()).output() {
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    } else { assert!(std::env::var_os("TAPID_REQUIRE_NODE_ASSERTIONS").is_none(), "Node is required in CI"); }
    assert_eq!(
        fs::read(project.path().join(".tapid-managed")).unwrap(),
        b"tapid-managed-v1\n"
    );
    assert_eq!(
        fs::read(project.path().join("package-lock.json")).unwrap(),
        npm_lock
    );
    assert_eq!(
        fs::read(backup.path().join("saved-tree/original")).unwrap(),
        b"original npm installation"
    );
    project.write("registry.json", b"not JSON").unwrap();
    let lock_before = fs::read(project.path().join("tapid.lock")).unwrap();
    let output = invoke(
        &project,
        &[
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        lock_before
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    // With successful replay settled, preserve the replacement and restore the saved inputs.
    fs::rename(
        project.path().join("node_modules"),
        backup.path().join("replacement"),
    )
    .unwrap();
    fs::rename(
        backup.path().join("saved-tree"),
        project.path().join("node_modules"),
    )
    .unwrap();
    fs::remove_file(project.path().join(".tapid-managed")).unwrap();
    fs::remove_file(project.path().join("tapid.lock")).unwrap();
    assert!(!project.path().join("tapid.lock").exists());
    assert!(!project.path().join(".tapid-managed").exists());
    assert_eq!(
        fs::read(project.path().join("node_modules/original")).unwrap(),
        b"original npm installation"
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(project.path().join("package-lock.json")).unwrap(),
        npm_lock
    );
}

#[test]
fn npm_import_failures_leave_project_outputs_unchanged() {
    let project = reference_project("npm-import-failures");
    project.write("tapid.lock", b"prior lock bytes").unwrap();
    project
        .write("node_modules/keep.txt", b"prior active tree")
        .unwrap();
    project.write("store/keep.txt", b"prior store").unwrap();
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-import/package-lock.json")).unwrap();
    let mut cases = Vec::new();
    for (field, value, expected) in [
        (
            "link",
            serde_json::json!(true),
            "/packages/node_modules~1parent/link",
        ),
        (
            "inBundle",
            serde_json::json!(true),
            "/packages/node_modules~1parent/inBundle",
        ),
        (
            "resolved",
            serde_json::json!("https://registry.npmjs.org/parent/-/parent-9.0.0.tgz"),
            "/packages/node_modules~1parent/resolved",
        ),
        (
            "integrity",
            serde_json::json!("sha1-deadbeef"),
            "/packages/node_modules~1parent/integrity",
        ),
    ] {
        let mut input = original.clone();
        input["packages"]["node_modules/parent"][field] = value;
        cases.push((input.to_string(), expected));
    }
    let mut old = original.clone();
    old["lockfileVersion"] = serde_json::json!(2);
    cases.push((old.to_string(), "/lockfileVersion"));
    cases.push(("{broken JSON".into(), "JSON"));
    for (input, diagnostic) in cases {
        project
            .write("package-lock.json", input.as_bytes())
            .unwrap();
        let output = invoke(&project, &["import-package-lock", "package-lock.json"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            b"prior lock bytes"
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/keep.txt")).unwrap(),
            b"prior active tree"
        );
        assert_eq!(
            fs::read(project.path().join("store/keep.txt")).unwrap(),
            b"prior store"
        );
    }
    assert!(!project.path().join(".tapid-activation.lock").exists());
}

#[test]
fn npm_import_frozen_rejects_corrupt_or_misidentified_archives_without_activation() {
    for corrupt_bytes in [true, false] {
        let project = reference_project("npm-import-artifact-rejection");
        project
            .write("node_modules/keep.txt", b"prior active tree")
            .unwrap();
        project
            .write(".tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        let mut fixture: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/npm-import/registry.json")).unwrap();
        let mut npm: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/npm-import/package-lock.json"))
                .unwrap();
        if corrupt_bytes {
            fixture["packages"][0]["artifact"] = serde_json::json!("base64:YmFk");
        } else {
            fixture["packages"][0]["artifact"] = fixture["packages"][1]["artifact"].clone();
            fixture["packages"][0]["integrity"] = fixture["packages"][1]["integrity"].clone();
            npm["packages"]["node_modules/parent"]["integrity"] =
                fixture["packages"][1]["integrity"].clone();
        }
        project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        project
            .write("package-lock.json", npm.to_string().as_bytes())
            .unwrap();
        assert!(
            invoke(&project, &["import-package-lock", "package-lock.json"])
                .status
                .success()
        );
        let before = fs::read(project.path().join("tapid.lock")).unwrap();
        let store = project.path().join("store");
        let output = invoke(
            &project,
            &[
                "install",
                "--frozen",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(if corrupt_bytes {
                "integrity mismatch"
            } else {
                "archive identity"
            }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
        assert_eq!(
            fs::read(project.path().join("node_modules/keep.txt")).unwrap(),
            b"prior active tree"
        );
        assert!(
            !store.join("trees").exists()
                || fs::read_dir(store.join("trees")).unwrap().next().is_none()
        );
    }
}

#[test]
fn npm_import_refuses_recovery_state_without_recovering_it() {
    let project = reference_project("npm-import-pending");
    project
        .write(".tapid-lifecycle-journal.json", b"unfinished journal")
        .unwrap();
    project.write("tapid.lock", b"original lock").unwrap();
    let output = invoke(&project, &["import-package-lock", "package-lock.json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interrupted install"));
    assert_eq!(
        fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
        b"unfinished journal"
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        b"original lock"
    );
}

#[test]
fn npm_import_reference_matches_offline_npm_ci() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let project = reference_project("npm-import-npm-ci");
    let home = tapid_test_support::TempHome::new("npm-import-npm-ci").unwrap();
    let cache = project.path().join("npm-cache");
    project.write("empty.npmrc", b"").unwrap();
    project.write("global.npmrc", b"").unwrap();
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let run_npm = |args: &[&str]| {
        let mut command = Command::new(npm);
        command.env_clear();
        for variable in [
            "PATH",
            "SystemRoot",
            "COMSPEC",
            "PATHEXT",
            "TMP",
            "TEMP",
            "TMPDIR",
        ] {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        command
            .args(args)
            .args([
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--cache",
                cache.to_str().unwrap(),
                "--userconfig",
                project.path().join("empty.npmrc").to_str().unwrap(),
                "--globalconfig",
                project.path().join("global.npmrc").to_str().unwrap(),
            ])
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(project.path())
            .output()
    };
    // npm is only an optional development oracle, never an import/install requirement.
    if let Err(error) = run_npm(&["--version"]) {
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        return;
    }
    let fixture: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-import/registry.json")).unwrap();
    for (index, package) in fixture["packages"].as_array().unwrap().iter().enumerate() {
        let bytes = STANDARD
            .decode(
                package["artifact"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64:")
                    .unwrap(),
            )
            .unwrap();
        let filename = format!("archive-{index}.tgz");
        project.write(&filename, &bytes).unwrap();
        let output = run_npm(&["cache", "add", &filename]).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = run_npm(&["ci"]).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let placements = [
        "node_modules/parent",
        "node_modules/shared",
        "node_modules/parent/node_modules/shared",
        "node_modules/consumer",
    ];
    let npm_versions: Vec<serde_json::Value> = placements
        .iter()
        .map(|path| {
            let value: serde_json::Value = serde_json::from_slice(
                &fs::read(project.path().join(path).join("package.json")).unwrap(),
            )
            .unwrap();
            value["version"].clone()
        })
        .collect();
    assert!(!project.path().join("node_modules/native").exists());
    // Explicitly opt in to replacing the npm-created node_modules tree.
    project
        .write(".tapid-managed", b"tapid-managed-v1\n")
        .unwrap();
    let output = invoke(&project, &["import-package-lock", "package-lock.json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = project.path().join("tapid-store");
    let output = invoke(
        &project,
        &[
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (path, expected) in placements.iter().zip(npm_versions) {
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(project.path().join(path).join("package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["version"], expected);
    }
}

#[test]
fn npm_import_replay_revalidates_root_declarations() {
    let project = reference_project("npm-import-edited-roots");
    assert!(
        invoke(&project, &["import-package-lock", "package-lock.json"])
            .status
            .success()
    );
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("tapid.lock")).unwrap()).unwrap();
    lock["npmLock"]["packages"][""]["dependencies"]["shared"] = serde_json::json!("*");
    project
        .write("tapid.lock", lock.to_string().as_bytes())
        .unwrap();
    let before = fs::read(project.path().join("tapid.lock")).unwrap();
    let store = project.path().join("store");
    let output = invoke(
        &project,
        &[
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match package.json"));
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), before);
    assert!(!project.path().join("node_modules").exists());
}

#[test]
fn npm_import_ci_rejects_imported_schema_without_mutation() {
    let project = reference_project("npm-import-ci");
    assert!(
        invoke(&project, &["import-package-lock", "package-lock.json"])
            .status
            .success()
    );
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let store = project.path().join("store");
    let fixture = project.path().join("registry.json");
    for offline in [false, true] {
        let mut args = vec![
            "ci",
            "--store-dir",
            store.to_str().unwrap(),
            "--registry-fixture",
            fixture.to_str().unwrap(),
        ];
        if offline {
            args.push("--offline");
        }
        let output = invoke(&project, &args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("ci requires an ordinary verified-tree lock"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            manifest
        );
        assert!(!store.exists());
        assert!(!project.path().join("node_modules").exists());
    }
}

fn workspace_project() -> (TempProject, TempHome) {
    let project = TempProject::new("npm-import-workspaces").unwrap();
    let home = TempHome::new("npm-import-workspaces").unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*","apps/*"],"dependencies":{"member":"^1"}});
    let member = serde_json::json!({"name":"member","version":"1.0.0","main":"index.js","bin":"./index.js","funding":"https://example.com/fund","deprecated":"Use the replacement","dependencies":{"shared":"1","@scope/helper":"^1"},"optionalDependencies":{"native":"1"}});
    let second = serde_json::json!({"name":"second","version":"1.0.0","dependencies":{"shared":"2","parent":"1","member":"1"}});
    let helper = serde_json::json!({"name":"@scope/helper","version":"1.0.0","peerDependencies":{"shared":"1"}});
    let reference: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/npm-import/package-lock.json")).unwrap();
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":{"version":"1.0.0","bin":{"member":"index.js"},"funding":"https://example.com/fund","deprecated":"Use the replacement","dependencies":{"shared":"1","@scope/helper":"^1"},"optionalDependencies":{"native":"1"}},
        "packages/helper":helper,
        "packages/helper/node_modules/shared":reference["packages"]["node_modules/shared"],
        "apps/second":second,
        "node_modules/second":{"resolved":"apps/second","link":true},
        "apps/second/node_modules/member":{"resolved":"packages/member","link":true},
        "apps/second/node_modules/shared":reference["packages"]["node_modules/parent/node_modules/shared"],
        "node_modules/parent":reference["packages"]["node_modules/parent"],
        "node_modules/parent/node_modules/shared":reference["packages"]["node_modules/parent/node_modules/shared"],
        "node_modules/@scope/helper":{"resolved":"packages/helper","link":true},
        "node_modules/member":{"resolved":"packages/member","link":true},
        "node_modules/shared":reference["packages"]["node_modules/shared"],
        "node_modules/native":reference["packages"]["node_modules/native"]
    }});
    project
        .write("package.json", root.to_string().as_bytes())
        .unwrap();
    project
        .write(
            "packages/member/package.json",
            member.to_string().as_bytes(),
        )
        .unwrap();
    project
        .write(
            "packages/helper/package.json",
            helper.to_string().as_bytes(),
        )
        .unwrap();
    project
        .write(
            "packages/helper/index.js",
            b"module.exports = require('shared');",
        )
        .unwrap();
    project
        .write(
            "packages/member/index.js",
            b"module.exports = require('@scope/helper');",
        )
        .unwrap();
    project
        .write("apps/second/package.json", second.to_string().as_bytes())
        .unwrap();
    project
        .write(
            "apps/second/index.js",
            b"module.exports = [require('shared'), require('parent'), require('member')];",
        )
        .unwrap();
    project
        .write("package-lock.json", npm.to_string().as_bytes())
        .unwrap();
    project
        .write(
            "registry.json",
            include_bytes!("fixtures/npm-import/registry.json"),
        )
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            project.path().join("packages/member/index.js"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    (project, home)
}

#[test]
fn npm_import_workspace_links_install_and_replay_without_resolution() {
    let (project, home) = workspace_project();
    let backup = TempProject::new("workspace-npm-backup").unwrap();
    let npm_lock = fs::read(project.path().join("package-lock.json")).unwrap();
    for (ordinal, tree) in [
        "node_modules",
        "packages/member/node_modules",
        "apps/node_modules",
    ]
    .iter()
    .enumerate()
    {
        project
            .write(format!("{tree}/original"), b"saved npm tree")
            .unwrap();
        fs::rename(
            project.path().join(tree),
            backup.path().join(ordinal.to_string()),
        )
        .unwrap();
    }
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tapid"))
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path())
            .env("LOCALAPPDATA", home.path())
            .current_dir(project.path())
            .output()
            .unwrap()
    };
    let output = invoke(&["import-package-lock", "package-lock.json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(invoke(&["lock", "verify"]).status.success());
    let imported = fs::read(project.path().join("tapid.lock")).unwrap();
    assert!(
        invoke(&["import-package-lock", "package-lock.json"])
            .status
            .success()
    );
    assert_eq!(
        imported,
        fs::read(project.path().join("tapid.lock")).unwrap()
    );
    let store = project.path().join("store");
    let store = store.to_str().unwrap();
    for offline in [false, true] {
        let args = if offline {
            vec!["install", "--frozen", "--offline", "--store-dir", store]
        } else {
            vec![
                "install",
                "--frozen",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store,
            ]
        };
        let output = invoke(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::canonicalize(project.path().join("node_modules/member")).unwrap(),
            fs::canonicalize(project.path().join("packages/member")).unwrap()
        );
        assert!(
            project
                .path()
                .join("node_modules/shared/package.json")
                .is_file()
        );
        assert!(!project.path().join("node_modules/native").exists());
        #[cfg(unix)]
        assert!(
            project
                .path()
                .join("apps/second/node_modules/.bin/member")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        #[cfg(windows)]
        {
            assert!(
                project
                    .path()
                    .join("apps/second/node_modules/.bin/member.cmd")
                    .is_file()
            );
        }
        let node = Command::new("node")
            .args([
                "-e",
                "const a = require('node:assert'); a.equal(require('member'), 'shared@1.0.0'); a.deepEqual(require('second'), ['shared@2.0.0', 'parent@1.0.0', 'shared@1.0.0']); a.equal(require('node:module').createRequire(require.resolve('parent'))('shared'), 'shared@2.0.0');",
            ])
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(
            node.status.success(),
            "{}",
            String::from_utf8_lossy(&node.stderr)
        );
    }
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"1.0.0","dependencies":{"shared":"2"}}"#,
        )
        .unwrap();
    assert!(
        !invoke(&["install", "--frozen", "--offline", "--store-dir", store])
            .status
            .success()
    );
    assert_eq!(lock, fs::read(project.path().join("tapid.lock")).unwrap());
    assert_eq!(
        fs::read(project.path().join("package-lock.json")).unwrap(),
        npm_lock
    );
    for ordinal in 0..3 {
        assert_eq!(
            fs::read(backup.path().join(ordinal.to_string()).join("original")).unwrap(),
            b"saved npm tree"
        );
    }
}

#[test]
fn npm_import_workspace_failures_preserve_existing_project_state() {
    let project = TempProject::new("npm-import-workspace-failures").unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
    let member = serde_json::json!({"name":"member","version":"1.0.0"});
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":member,
        "node_modules/member":{"resolved":"packages/member","link":true}
    }});
    project
        .write("package.json", root.to_string().as_bytes())
        .unwrap();
    project
        .write(
            "packages/member/package.json",
            member.to_string().as_bytes(),
        )
        .unwrap();
    project.write("tapid.lock", b"prior lock").unwrap();
    project
        .write("node_modules/keep.txt", b"prior install")
        .unwrap();
    project.write("store/keep.txt", b"prior store").unwrap();
    for case in [
        "stale",
        "undeclared",
        "dangling",
        "escape",
        "nested",
        "ancestor-tree",
        "member-tree",
    ] {
        let mut input = npm.clone();
        match case {
            "stale" => input["packages"]["packages/member"]["version"] = serde_json::json!("2.0.0"),
            "undeclared" => {
                input["packages"]["outside"] = input["packages"]["packages/member"].take();
                input["packages"]
                    .as_object_mut()
                    .unwrap()
                    .remove("packages/member");
                input["packages"]["node_modules/member"]["resolved"] = serde_json::json!("outside");
                project
                    .write("outside/package.json", member.to_string().as_bytes())
                    .unwrap();
            }
            "dangling" => {
                input["packages"]["node_modules/member"]["resolved"] = serde_json::json!("missing")
            }
            "escape" => {
                input["packages"]["node_modules/member"]["resolved"] =
                    serde_json::json!("../outside")
            }
            "nested" => {
                input["packages"]["packages/member/node_modules/other"] =
                    serde_json::json!({"version":"1.0.0"})
            }
            "ancestor-tree" => {
                project
                    .write("packages/node_modules/leftover", b"prior ancestor install")
                    .unwrap();
            }
            "member-tree" => {
                project
                    .write(
                        "packages/member/node_modules/leftover",
                        b"prior member install",
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        project
            .write("package-lock.json", input.to_string().as_bytes())
            .unwrap();
        let output = invoke(&project, &["import-package-lock", "package-lock.json"]);
        assert!(!output.status.success(), "{case}");
        if case == "member-tree" || case == "ancestor-tree" {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tree = if case == "member-tree" {
                "packages/member/node_modules"
            } else {
                "packages/node_modules"
            };
            assert!(stderr.contains("unused backup location"), "{stderr}");
            assert!(
                stderr.contains(&project.path().join(tree).display().to_string()),
                "{stderr}"
            );
            assert_eq!(
                fs::read(project.path().join(tree).join("leftover")).unwrap(),
                if case == "member-tree" {
                    b"prior member install".as_slice()
                } else {
                    b"prior ancestor install".as_slice()
                }
            );
            assert!(
                !project
                    .path()
                    .join(tree)
                    .parent()
                    .unwrap()
                    .join(".tapid-managed")
                    .exists()
            );
            // Each topology is checked independently.
            fs::remove_dir_all(project.path().join(tree)).unwrap();
        }
        assert_eq!(
            fs::read(project.path().join("package-lock.json")).unwrap(),
            input.to_string().as_bytes()
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            b"prior lock"
        );
        assert_eq!(
            fs::read(project.path().join("node_modules/keep.txt")).unwrap(),
            b"prior install"
        );
        assert_eq!(
            fs::read(project.path().join("store/keep.txt")).unwrap(),
            b"prior store"
        );
        assert_eq!(
            fs::read(project.path().join("package.json")).unwrap(),
            root.to_string().as_bytes()
        );
        assert_eq!(
            fs::read(project.path().join("packages/member/package.json")).unwrap(),
            member.to_string().as_bytes()
        );
    }
}

#[cfg(unix)]
#[test]
fn npm_import_workspace_rejects_symlink_escape() {
    let project = TempProject::new("npm-import-workspace-symlink").unwrap();
    let outside = TempProject::new("npm-import-workspace-outside").unwrap();
    let root =
        serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/member"]});
    let member = serde_json::json!({"name":"member","version":"1.0.0"});
    outside
        .write("package.json", member.to_string().as_bytes())
        .unwrap();
    project
        .write("package.json", root.to_string().as_bytes())
        .unwrap();
    project.write("packages/keep.txt", b"keep").unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join("packages/member")).unwrap();
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":member,
        "node_modules/member":{"resolved":"packages/member","link":true}
    }});
    project
        .write("package-lock.json", npm.to_string().as_bytes())
        .unwrap();
    project.write("tapid.lock", b"prior lock").unwrap();
    assert!(
        !invoke(&project, &["import-package-lock", "package-lock.json"])
            .status
            .success()
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        b"prior lock"
    );
}

#[test]
fn npm_import_workspace_crash_recovery_settles_every_managed_tree() {
    for installed in [false, true] {
        for point in [
            "workspace_member_activated",
            "activation_complete",
            "commit_decision",
        ] {
            let (project, home) = workspace_project();
            let command = || {
                let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
                command
                    .current_dir(project.path())
                    .env("HOME", home.path())
                    .env("XDG_CACHE_HOME", home.path())
                    .env("LOCALAPPDATA", home.path());
                command
            };
            assert!(
                command()
                    .args(["import-package-lock", "package-lock.json"])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            let store = project.path().join("store");
            let install = [
                "install",
                "--frozen",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store.to_str().unwrap(),
            ];
            if installed {
                assert!(command().args(install).output().unwrap().status.success());
                project.write("node_modules/sentinel", b"old root").unwrap();
                project
                    .write("apps/second/node_modules/sentinel", b"old member")
                    .unwrap();
            }
            let lock = fs::read(project.path().join("tapid.lock")).unwrap();
            let output = command()
                .args(install)
                .env("TAPID_TEST_CRASH_POINT", point)
                .output()
                .unwrap();
            assert!(!output.status.success(), "{point}");
            let output = command()
                .args(install)
                .env("TAPID_TEST_RECOVER_ONLY", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{point}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if point == "commit_decision" {
                assert!(
                    project
                        .path()
                        .join("node_modules/shared/package.json")
                        .is_file()
                );
                assert!(
                    project
                        .path()
                        .join("apps/second/node_modules/shared/package.json")
                        .is_file()
                );
                assert!(!project.path().join("node_modules/sentinel").exists());
                assert!(
                    !project
                        .path()
                        .join("apps/second/node_modules/sentinel")
                        .exists()
                );
            } else {
                assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
                for root in [
                    project.path().to_path_buf(),
                    project.path().join("apps/second"),
                ] {
                    assert_eq!(
                        root.join("node_modules").exists(),
                        installed,
                        "{point}: {}",
                        root.display()
                    );
                    assert_eq!(root.join(".tapid-managed").exists(), installed);
                }
                if installed {
                    assert_eq!(
                        fs::read(project.path().join("node_modules/sentinel")).unwrap(),
                        b"old root"
                    );
                    assert_eq!(
                        fs::read(project.path().join("apps/second/node_modules/sentinel")).unwrap(),
                        b"old member"
                    );
                } else {
                    assert!(!store.join("trees").exists());
                }
            }
            for root in [
                project.path().to_path_buf(),
                project.path().join("apps/second"),
            ] {
                assert!(!root.join(".tapid-lifecycle-journal.json").exists());
                for entry in fs::read_dir(root).unwrap() {
                    let name = entry.unwrap().file_name();
                    let name = name.to_string_lossy();
                    assert!(
                        !name.starts_with(".tapid-install-stage-")
                            && !name.starts_with(".tapid-node-modules-old-")
                            && !name.starts_with(".tapid-managed-old-"),
                        "{name}"
                    );
                }
            }
        }
    }
}

#[test]
fn npm_import_workspace_activation_failure_restores_member_and_root() {
    let (project, home) = workspace_project();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .current_dir(project.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path())
            .env("LOCALAPPDATA", home.path());
        command
    };
    assert!(
        command()
            .args(["import-package-lock", "package-lock.json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let store = project.path().join("store");
    let install = [
        "install",
        "--frozen",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        store.to_str().unwrap(),
    ];
    assert!(command().args(install).output().unwrap().status.success());
    project.write("node_modules/sentinel", b"root").unwrap();
    project
        .write("apps/second/node_modules/sentinel", b"member")
        .unwrap();
    let lock = fs::read(project.path().join("tapid.lock")).unwrap();
    // A malformed root marker fails root activation after the member was activated.
    project.write(".tapid-managed", b"invalid marker").unwrap();
    let output = command().args(install).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(project.path().join("tapid.lock")).unwrap(), lock);
    assert_eq!(
        fs::read(project.path().join("node_modules/sentinel")).unwrap(),
        b"root"
    );
    assert_eq!(
        fs::read(project.path().join("apps/second/node_modules/sentinel")).unwrap(),
        b"member"
    );
    assert_eq!(
        fs::read(project.path().join(".tapid-managed")).unwrap(),
        b"invalid marker"
    );
    assert!(
        !project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
}

#[test]
fn npm_import_native_update_clears_owned_member_selections() {
    let (project, home) = workspace_project();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .current_dir(project.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path())
            .env("LOCALAPPDATA", home.path());
        command
    };
    assert!(
        command()
            .args(["import-package-lock", "package-lock.json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let store = project.path().join("store");
    let output = command()
        .args([
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        project
            .path()
            .join("apps/second/node_modules/shared/package.json")
            .is_file()
    );
    let path = project.path().join("apps/second/package.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["dependencies"]["shared"] = serde_json::json!("1");
    fs::write(path, manifest.to_string()).unwrap();
    let path = project.path().join("package.json");
    let mut root: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    root["dependencies"]["shared"] = serde_json::json!("1");
    fs::write(path, root.to_string()).unwrap();
    let output = command()
        .args([
            "update",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for member in ["apps/second", "packages/helper"] {
        assert!(
            !project
                .path()
                .join(member)
                .join("node_modules/shared")
                .exists()
        );
        assert!(project.path().join(member).join(".tapid-managed").is_file());
    }
    let output = command()
        .args([
            "install",
            "--offline",
            "--frozen",
            "--store-dir",
            store.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let node = Command::new("node").args(["-e", "require('node:assert').deepEqual(require('second'), ['shared@1.0.0', 'parent@1.0.0', 'shared@1.0.0']);"]).current_dir(project.path()).output().unwrap();
    assert!(
        node.status.success(),
        "{}",
        String::from_utf8_lossy(&node.stderr)
    );
}

#[test]
fn npm_import_workspace_hoisting_preserves_direct_registry_and_local_bindings() {
    for local in [false, true] {
        let project = TempProject::new("npm-import-workspace-hoisting").unwrap();
        let home = TempHome::new("npm-import-workspace-hoisting").unwrap();
        let reference: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/npm-import/package-lock.json"))
                .unwrap();
        let root = if local {
            serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/shared"],"dependencies":{"shared":"1","parent":"1"}})
        } else {
            serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/member"]})
        };
        let member = if local {
            serde_json::json!({"name":"shared","version":"1.0.0"})
        } else {
            serde_json::json!({"name":"member","version":"1.0.0","dependencies":{"shared":"1","parent":"1"}})
        };
        let mut npm = serde_json::json!({"lockfileVersion":3,"packages":{"":root}});
        let source = if local {
            "packages/shared"
        } else {
            "packages/member"
        };
        npm["packages"][source] = member.clone();
        npm["packages"][if local {
            "node_modules/shared"
        } else {
            "node_modules/member"
        }] = serde_json::json!({"resolved":source,"link":true});
        let parent = if local {
            "node_modules/parent"
        } else {
            "packages/member/node_modules/parent"
        };
        npm["packages"][parent] = reference["packages"]["node_modules/parent"].clone();
        npm["packages"][format!("{parent}/node_modules/shared")] =
            reference["packages"]["node_modules/parent/node_modules/shared"].clone();
        if !local {
            npm["packages"]["node_modules/shared"] =
                reference["packages"]["node_modules/shared"].clone();
        }
        project
            .write("package.json", root.to_string().as_bytes())
            .unwrap();
        project
            .write(
                format!("{source}/package.json"),
                member.to_string().as_bytes(),
            )
            .unwrap();
        project
            .write(
                format!("{source}/index.js"),
                if local {
                    b"module.exports = 'local@1.0.0';"
                } else {
                    b"module.exports = require('shared');"
                },
            )
            .unwrap();
        project
            .write("package-lock.json", npm.to_string().as_bytes())
            .unwrap();
        project
            .write(
                "registry.json",
                include_bytes!("fixtures/npm-import/registry.json"),
            )
            .unwrap();
        let command = || {
            let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
            command
                .current_dir(project.path())
                .env("HOME", home.path())
                .env("XDG_CACHE_HOME", home.path())
                .env("LOCALAPPDATA", home.path());
            command
        };
        assert!(
            command()
                .args(["import-package-lock", "package-lock.json"])
                .output()
                .unwrap()
                .status
                .success()
        );
        let store = project.path().join("store");
        let output = command()
            .args([
                "install",
                "--frozen",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{local}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let script = if local {
            "const a = require('node:assert'); a.equal(require('shared'), 'local@1.0.0'); a.equal(require('node:module').createRequire(require.resolve('parent'))('shared'), 'shared@2.0.0');"
        } else {
            "const a = require('node:assert'); a.equal(require('member'), 'shared@1.0.0'); const r = require('node:module').createRequire(require.resolve('member')); a.equal(require('node:module').createRequire(r.resolve('parent'))('shared'), 'shared@2.0.0');"
        };
        let output = Command::new("node")
            .args(["-e", script])
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{local}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn manual_migration_failures_preserve_saved_installation_and_inputs() {
    let project = reference_project("manual-migration-failure");
    let home = TempHome::new("manual-migration-failure").unwrap();
    let backup = TempProject::new("manual-migration-saved").unwrap();
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let npm = fs::read(project.path().join("package-lock.json")).unwrap();
    project.write("tapid.lock", b"prior Tapid lock").unwrap();
    project
        .write("node_modules/original", b"original npm installation")
        .unwrap();
    fs::rename(
        project.path().join("node_modules"),
        backup.path().join("saved-tree"),
    )
    .unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tapid"));
        command
            .current_dir(project.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path())
            .env("LOCALAPPDATA", home.path());
        command
    };
    // Unsupported input cannot publish over the prior lock.
    let mut unsupported: serde_json::Value = serde_json::from_slice(&npm).unwrap();
    unsupported["lockfileVersion"] = serde_json::json!(2);
    project
        .write("unsupported.json", unsupported.to_string().as_bytes())
        .unwrap();
    assert!(
        !command()
            .args(["import-package-lock", "unsupported.json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        b"prior Tapid lock"
    );
    assert!(
        command()
            .args(["import-package-lock", "package-lock.json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let imported = fs::read(project.path().join("tapid.lock")).unwrap();
    let store = project.path().join("store");
    let output = command()
        .args([
            "install",
            "--frozen",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ])
        .env("TAPID_TEST_FAIL_ACTIVATION", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("activation failed (injected)"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        imported
    );
    assert!(!project.path().join("node_modules").exists());
    assert!(!project.path().join(".tapid-managed").exists());
    assert!(
        !project
            .path()
            .join(".tapid-lifecycle-journal.json")
            .exists()
    );
    assert_eq!(
        fs::read(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(project.path().join("package-lock.json")).unwrap(),
        npm
    );
    assert_eq!(
        fs::read(backup.path().join("saved-tree/original")).unwrap(),
        b"original npm installation"
    );
    // The failed transaction is settled; restore the saved installation and prior lock.
    fs::rename(
        backup.path().join("saved-tree"),
        project.path().join("node_modules"),
    )
    .unwrap();
    project.write("tapid.lock", b"prior Tapid lock").unwrap();
    assert_eq!(
        fs::read(project.path().join("node_modules/original")).unwrap(),
        b"original npm installation"
    );
    assert_eq!(
        fs::read(project.path().join("tapid.lock")).unwrap(),
        b"prior Tapid lock"
    );
}
