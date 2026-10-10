use super::*;

const ARTIFACT: &str = "H4sIAGAyj2oC/+3NsQoCMQyA4c4+hWSWmki5wbcpUg8V2+OqLuK7W3U4cBYR/L/lT7JkiJtD7NNyeNXva8nuw7TpQni2ea9qsGl+3M26lbm5ui8411Mc23v3n66S4zHJWralyEIuaay7kttuXr3KbeYAAAAAAAAAAAAAAAAAAL/oDtGfbE0AKAAA";

fn locked_project() -> tapid_test_support::TempProject {
    let project = tapid_test_support::TempProject::new("ci-locked").unwrap();
    project
        .write(
            "package.json",
            br#"{"name":"demo","version":"1.0.0","dependencies":{"foo":"^1.0.0"}}"#,
        )
        .unwrap();
    let integrity = format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(STANDARD.decode(ARTIFACT).unwrap()))
    );
    let fixture = serde_json::json!({"packages": [{"registry": "https://registry.npmjs.org", "name": "foo", "version": "1.0.0", "integrity": integrity, "artifact": format!("base64:{ARTIFACT}")}]});
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = run(
        project.path(),
        &[
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            project.path().join("store").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(project.path().join("tapid.lock")).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = "https://registry.npmjs.org/foo/-/foo-1.0.0.tgz".into();
    }
    project
        .write("tapid.lock", lock.to_string().as_bytes())
        .unwrap();
    project
}

#[test]
fn ci_validates_fixture_exceptions_on_warm_and_offline_paths() {
    for offline in [false, true] {
        for fixture_case in [
            "missing",
            "malformed",
            "wrong-name",
            "wrong-version",
            "wrong-registry",
            "valid",
        ] {
            let project = locked_project();
            let dir = project.path().to_path_buf();
            let store = dir.join("store");
            let mut lock: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
            for package in lock["packages"].as_object_mut().unwrap().values_mut() {
                package.as_object_mut().unwrap().remove("artifactUrl");
            }
            project
                .write("tapid.lock", lock.to_string().as_bytes())
                .unwrap();
            project.write("node_modules/KEEP", b"previous").unwrap();
            let fixture_path = if fixture_case == "missing" {
                "missing.json"
            } else {
                "registry.json"
            };
            if fixture_case == "malformed" {
                project.write("registry.json", b"invalid fixture").unwrap();
            } else if fixture_case.starts_with("wrong-") {
                let mut fixture: serde_json::Value =
                    serde_json::from_slice(&fs::read(dir.join("registry.json")).unwrap()).unwrap();
                let (field, value) = match fixture_case {
                    "wrong-name" => ("name", "other"),
                    "wrong-version" => ("version", "2.0.0"),
                    "wrong-registry" => ("registry", "https://other.example"),
                    _ => unreachable!(),
                };
                fixture["packages"][0][field] = value.into();
                project
                    .write("registry.json", fixture.to_string().as_bytes())
                    .unwrap();
            }
            let before_lock = fs::read(dir.join("tapid.lock")).unwrap();
            let before_manifest = fs::read(dir.join("package.json")).unwrap();
            let mut args = vec![
                "ci",
                "--registry-fixture",
                fixture_path,
                "--store-dir",
                store.to_str().unwrap(),
            ];
            if offline {
                args.push("--offline");
            }
            let output = run(&dir, &args);
            if fixture_case == "valid" {
                assert!(output.status.success(), "offline={offline}: {output:?}");
                assert!(!dir.join("node_modules/KEEP").exists());
            } else {
                assert_eq!(
                    output.status.code(),
                    Some(1),
                    "{fixture_case} offline={offline}: {output:?}"
                );
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("REGISTRY_METADATA_INVALID"),
                    "{output:?}"
                );
                assert_eq!(
                    fs::read(dir.join("node_modules/KEEP")).unwrap(),
                    b"previous"
                );
            }
            assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), before_lock);
            assert_eq!(fs::read(dir.join("package.json")).unwrap(), before_manifest);
        }
    }
}

#[test]
fn ci_fixture_exception_requires_every_url_less_transitive_identity() {
    for offline in [false, true] {
        let project = locked_project();
        let dir = project.path().to_path_buf();
        let store = dir.join("store");
        let mut fixture: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("registry.json")).unwrap()).unwrap();
        let mut transitive = fixture["packages"][0].clone();
        transitive["name"] = "bar".into();
        fixture["packages"][0]["dependencies"] = serde_json::json!({"bar": "1.0.0"});
        fixture["packages"].as_array_mut().unwrap().push(transitive);
        project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        // Refresh deliberately so the CI test lock contains the new transitive edge.
        let output = run(
            &dir,
            &[
                "update",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
                store.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{output:?}");
        let before_lock = fs::read(dir.join("tapid.lock")).unwrap();
        fixture["packages"].as_array_mut().unwrap().pop();
        project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        project.write("node_modules/KEEP", b"previous").unwrap();
        let mut args = vec![
            "ci",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ];
        if offline {
            args.push("--offline");
        }
        let output = run(&dir, &args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "offline={offline}: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("REGISTRY_METADATA_INVALID"),
            "{output:?}"
        );
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), before_lock);
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"previous"
        );
    }
}

#[test]
fn ci_requires_complete_lockfiles_with_warm_and_cold_stores_including_offline() {
    for cold in [false, true] {
        for offline in [false, true] {
            let project = locked_project();
            let dir = project.path().to_path_buf();
            let store = dir.join("store");
            let mut lock: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
            for package in lock["packages"].as_object_mut().unwrap().values_mut() {
                package.as_object_mut().unwrap().remove("artifactUrl");
            }
            project
                .write("tapid.lock", lock.to_string().as_bytes())
                .unwrap();
            project.write("node_modules/KEEP", b"previous").unwrap();
            if cold {
                fs::remove_dir_all(&store).unwrap();
            }
            let before_lock = fs::read(dir.join("tapid.lock")).unwrap();
            let mut args = vec!["ci", "--store-dir", store.to_str().unwrap()];
            if offline {
                args.push("--offline");
            }
            let output = run(&dir, &args);
            assert_eq!(
                output.status.code(),
                Some(1),
                "cold={cold} offline={offline}: {output:?}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("LOCKFILE_INVALID"), "{stderr}");
            assert!(stderr.contains("tapid install"), "{stderr}");
            assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), before_lock);
            assert_eq!(
                fs::read(dir.join("node_modules/KEEP")).unwrap(),
                b"previous"
            );
        }
    }
}

#[test]
fn ci_downloads_missing_locked_artifacts_without_resolving_or_rewriting() {
    let project = locked_project();
    let dir = project.path().to_path_buf();
    let lock = fs::read(dir.join("tapid.lock")).unwrap();
    let manifest = fs::read(dir.join("package.json")).unwrap();
    fs::remove_dir_all(dir.join("store")).unwrap();
    // Registry dependencies deliberately disagree with the lock. CI must use locked edges.
    let mut fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("registry.json")).unwrap()).unwrap();
    fixture["packages"][0]["dependencies"] = serde_json::json!({"unavailable": "9.0.0"});
    let mut newer = fixture["packages"][0].clone();
    newer["version"] = "1.9.0".into();
    fixture["packages"].as_array_mut().unwrap().push(newer);
    project
        .write("registry.json", fixture.to_string().as_bytes())
        .unwrap();
    let output = run(
        &dir,
        &[
            "ci",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            project.path().join("store").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/foo/package.json").is_file());
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock);
    assert_eq!(fs::read(dir.join("package.json")).unwrap(), manifest);
    // A warm cache must not need fixture metadata or download access.
    project.write("registry.json", b"invalid").unwrap();
    let output = run(
        &dir,
        &[
            "ci",
            "--offline",
            "--store-dir",
            project.path().join("store").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run(
        &dir,
        &[
            "ci",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            project.path().join("store").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ci_rejects_invalid_locked_installs_without_replacing_previous_outputs() {
    for failure in [
        "missing-lock",
        "manifest",
        "integrity",
        "tree-digest",
        "unverified",
        "offline",
        "missing-url",
        "corrupt-cache",
        "registry-route",
        "unmanaged",
        "artifact-origin",
    ] {
        let project = locked_project();
        let dir = project.path().to_path_buf();
        let store = dir.join("store");
        project
            .write("node_modules/KEEP", b"previous install")
            .unwrap();
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("tapid.lock")).unwrap()).unwrap();
        let package = lock["packages"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        let digest = package["treeDigest"].as_str().unwrap().to_owned();
        let diagnostic = match failure {
            "missing-lock" => {
                fs::remove_file(dir.join("tapid.lock")).unwrap();
                "LOCKFILE_MISSING"
            }
            "manifest" => {
                project
                    .write(
                        "package.json",
                        br#"{"name":"changed","version":"1.0.0","dependencies":{"foo":"^1.0.0"}}"#,
                    )
                    .unwrap();
                "LOCK_MANIFEST_MISMATCH"
            }
            "integrity" => {
                package["artifactIntegrity"] =
                    format!("sha512-{}", STANDARD.encode([0u8; 64])).into();
                "INTEGRITY_MISMATCH"
            }
            "tree-digest" => {
                let wrong = format!("sha256-{}", "0".repeat(64));
                package["treeDigest"] = wrong.clone().into();
                package["unpackedDigest"] = wrong.into();
                "INTEGRITY_MISMATCH"
            }
            "unverified" => {
                package["registryIntegrityDeclared"] = false.into();
                "LOCKFILE_INVALID"
            }
            "offline" => "STORE_CONTENT_UNAVAILABLE",
            "missing-url" => {
                package.as_object_mut().unwrap().remove("artifactUrl");
                "LOCKFILE_INVALID"
            }
            "corrupt-cache" => {
                project
                    .write(format!("store/trees/{digest}/package.json"), b"tampered")
                    .unwrap();
                "INTEGRITY_MISMATCH"
            }
            "registry-route" => {
                project
                    .write(
                        "tapid.toml",
                        b"[registries.default]\nurl='https://mirror.example'\n",
                    )
                    .unwrap();
                "LOCKFILE_INVALID"
            }
            "unmanaged" => {
                fs::remove_file(dir.join(".tapid-managed")).unwrap();
                "MATERIALIZATION_FAILED"
            }
            "artifact-origin" => {
                package["artifactUrl"] = "https://unconfigured.example/foo.tgz".into();
                "REGISTRY_TRANSPORT_FAILED"
            }
            _ => unreachable!(),
        };
        if failure != "missing-lock" {
            project
                .write("tapid.lock", lock.to_string().as_bytes())
                .unwrap();
        }
        if failure != "corrupt-cache" {
            fs::remove_dir_all(&store).unwrap();
        }
        let before_lock = fs::read(dir.join("tapid.lock")).ok();
        let before_manifest = fs::read(dir.join("package.json")).unwrap();
        let mut args = vec!["ci", "--store-dir", store.to_str().unwrap()];
        if failure == "offline" {
            args.push("--offline");
        } else if failure != "missing-url" && failure != "artifact-origin" {
            args.extend(["--registry-fixture", "registry.json"]);
        }
        let output = run(&dir, &args);
        assert_eq!(output.status.code(), Some(1), "{failure}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{failure}: {stderr}");
        assert_eq!(
            fs::read(dir.join("tapid.lock")).ok(),
            before_lock,
            "{failure}"
        );
        assert_eq!(
            fs::read(dir.join("package.json")).unwrap(),
            before_manifest,
            "{failure}"
        );
        assert_eq!(
            fs::read(dir.join("node_modules/KEEP")).unwrap(),
            b"previous install",
            "{failure}"
        );
        if failure != "corrupt-cache" && store.join("trees").exists() {
            assert_eq!(
                fs::read_dir(store.join("trees")).unwrap().count(),
                0,
                "{failure}"
            );
        }
    }
}

#[test]
fn ci_rejects_package_mutation_and_unverified_options() {
    let project = tapid_test_support::TempProject::new("ci-options").unwrap();
    let dir = project.path().to_path_buf();
    for args in [
        vec!["ci", "foo"],
        vec!["ci", "--allow-unverified-registry-artifacts"],
        vec!["ci", "--frozen"],
    ] {
        let output = run(&dir, &args);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }
    let output = run(&dir, &["ci", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("without version resolution"));
    assert!(help.contains("--offline"));
}

#[test]
fn ci_validates_workspace_members_and_preserves_aliases_from_an_empty_store() {
    let project = locked_project();
    let dir = project.path().to_path_buf();
    let store = dir.join("store");
    project.write("package.json", br#"{"name":"demo","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"workspace:*","renamed":"npm:foo@^1.0.0"}}"#).unwrap();
    let member = br#"{"name":"member","version":"1.0.0","dependencies":{"foo":"^1.0.0"}}"#;
    project
        .write("packages/member/package.json", member)
        .unwrap();
    let output = run(
        &dir,
        &[
            "install",
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
    let lock = fs::read(dir.join("tapid.lock")).unwrap();
    fs::remove_dir_all(&store).unwrap();
    let args = [
        "ci",
        "--registry-fixture",
        "registry.json",
        "--store-dir",
        store.to_str().unwrap(),
    ];
    let output = run(&dir, &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.join("node_modules/renamed/package.json").is_file());
    assert!(dir.join("node_modules/member/package.json").is_file());
    assert!(dir.join("node_modules/foo/package.json").is_file());
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock);
    let replay = run(
        &dir,
        &[
            "install",
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
    project.write("node_modules/KEEP", b"previous").unwrap();
    project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"2.0.0","dependencies":{"foo":"^1.0.0"}}"#,
        )
        .unwrap();
    fs::remove_dir_all(&store).unwrap();
    project
        .write("registry.json", b"invalid fixture must not be read")
        .unwrap();
    let output = run(&dir, &args);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("workspace membership or member manifest changed")
    );
    assert_eq!(
        fs::read(dir.join("node_modules/KEEP")).unwrap(),
        b"previous"
    );
    assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock);
}

#[test]
fn ci_recovers_store_and_project_after_precommit_and_postcommit_crashes() {
    for checkpoint in [
        "store_published",
        "node_modules_backed_up",
        "activation_complete",
        "commit_decision",
    ] {
        let project = locked_project();
        let dir = project.path().to_path_buf();
        let store = dir.join("store");
        let manifest = fs::read(dir.join("package.json")).unwrap();
        let lock = fs::read(dir.join("tapid.lock")).unwrap();
        project.write("node_modules/KEEP", b"previous").unwrap();
        fs::remove_dir_all(&store).unwrap();
        let args = [
            "ci",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
            store.to_str().unwrap(),
        ];
        let output = run_with_env(&dir, &args, "TAPID_TEST_CRASH_POINT", checkpoint);
        assert!(
            !output.status.success(),
            "{checkpoint}: crash hook did not terminate process"
        );
        let output = run_with_env(&dir, &args, "TAPID_TEST_RECOVER_ONLY", "1");
        assert!(
            output.status.success(),
            "{checkpoint}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(dir.join("package.json")).unwrap(), manifest);
        assert_eq!(fs::read(dir.join("tapid.lock")).unwrap(), lock);
        if checkpoint == "commit_decision" {
            assert!(!dir.join("node_modules/KEEP").exists());
            assert!(dir.join("node_modules/foo/package.json").is_file());
            assert_eq!(fs::read_dir(store.join("trees")).unwrap().count(), 1);
        } else {
            assert_eq!(
                fs::read(dir.join("node_modules/KEEP")).unwrap(),
                b"previous"
            );
            assert_eq!(
                fs::read_dir(store.join("trees"))
                    .map(|entries| entries.count())
                    .unwrap_or(0),
                0
            );
        }
        assert!(!dir.join(".tapid-lifecycle-journal.json").exists());
    }
}
