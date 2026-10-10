use std::{fs, process::Command};
use tapid_test_support::TempProject;

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
        "--frozen",
        "--store-dir",
        store.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
fn npm_import_preserves_nested_versions_peers_sources_and_optional_constraints() {
    let project = reference_project("npm-import-reference");
    let manifest = fs::read(project.path().join("package.json")).unwrap();
    let output = invoke(&project, &["import-package-lock", "package-lock.json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
