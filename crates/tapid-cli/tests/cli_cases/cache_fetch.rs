use std::fs;

fn project_snapshot(
    root: &std::path::Path,
    store: &str,
) -> std::collections::BTreeMap<std::path::PathBuf, (u8, Vec<u8>)> {
    fn visit(
        root: &std::path::Path,
        directory: &std::path::Path,
        excluded: &std::path::Path,
        result: &mut std::collections::BTreeMap<std::path::PathBuf, (u8, Vec<u8>)>,
    ) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path == excluded {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).unwrap();
            let value = if metadata.file_type().is_symlink() {
                (
                    2,
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                )
            } else if metadata.is_dir() {
                visit(root, &path, excluded, result);
                (1, Vec::new())
            } else {
                (0, fs::read(&path).unwrap())
            };
            result.insert(path.strip_prefix(root).unwrap().to_owned(), value);
        }
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &root.join(store), &mut result);
    result
}

#[test]
fn cache_fetch_cold_preserves_project_and_enables_offline_frozen_install() {
    let scenario = super::command_support::CliFixture::new("cache-fetch-cold");
    scenario
        .project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#,
        )
        .unwrap();
    scenario
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/npm-import/registry.json"),
        )
        .unwrap();
    let installed = scenario
        .command(&[
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(scenario.project.path().join("seed"))
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let root = scenario.project.path();
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tapid.lock")).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = "https://registry.npmjs.org/exact.tgz".into();
    }
    fs::write(root.join("tapid.lock"), lock.to_string()).unwrap();
    fs::remove_dir_all(root.join("node_modules")).unwrap();
    fs::remove_file(root.join(".tapid-activation.lock")).unwrap();
    let manifest = fs::read(root.join("package.json")).unwrap();
    let locked = fs::read(root.join("tapid.lock")).unwrap();
    let snapshot = project_snapshot(root, "prepared");
    let fetched = scenario
        .command(&[
            "cache",
            "fetch",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(root.join("prepared"))
        .output()
        .unwrap();
    assert!(fetched.status.success(), "{fetched:?}");
    assert_eq!(project_snapshot(root, "prepared"), snapshot);
    let value: serde_json::Value = serde_json::from_slice(&fetched.stdout).unwrap();
    assert_eq!(value["data"]["counters"]["fetched_verified_trees"], 2);
    assert_eq!(value["data"]["counters"]["newly_published_trees"], 2);
    assert_eq!(value["data"]["counters"]["fixture_reads"], 2);
    assert_eq!(value["data"]["available_at_completion"], 2);
    assert_eq!(fs::read(root.join("package.json")).unwrap(), manifest);
    assert_eq!(fs::read(root.join("tapid.lock")).unwrap(), locked);
    for path in [
        "node_modules",
        ".tapid-activation.lock",
        ".tapid-transaction.json",
        "prepared/.tapid-lifecycle-key",
    ] {
        assert!(!root.join(path).exists(), "{path}");
    }
    fs::remove_file(root.join("registry.json")).unwrap();
    let offline = scenario
        .command(&["install", "--offline", "--frozen", "--store-dir"])
        .arg(root.join("prepared"))
        .output()
        .unwrap();
    assert!(offline.status.success(), "{offline:?}");
    assert_eq!(fs::read(root.join("tapid.lock")).unwrap(), locked);
}

fn fetch_project(label: &str) -> super::command_support::CliFixture {
    let scenario = super::command_support::CliFixture::new(label);
    scenario
        .project
        .write(
            "package.json",
            br#"{"name":"app","version":"1.0.0","dependencies":{"parent":"1.0.0"}}"#,
        )
        .unwrap();
    scenario
        .project
        .write(
            "registry.json",
            include_bytes!("../fixtures/npm-import/registry.json"),
        )
        .unwrap();
    let output = scenario
        .command(&[
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(scenario.project.path().join("seed"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let path = scenario.project.path().join("tapid.lock");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for package in lock["packages"].as_object_mut().unwrap().values_mut() {
        package["artifactUrl"] = "https://registry.npmjs.org/exact.tgz".into();
    }
    fs::write(path, lock.to_string()).unwrap();
    scenario
}

fn fetch_json(
    scenario: &super::command_support::CliFixture,
    store: &str,
) -> (std::process::Output, serde_json::Value) {
    let snapshot = project_snapshot(scenario.project.path(), store);
    let output = scenario
        .command(&[
            "cache",
            "fetch",
            "--json",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(scenario.project.path().join(store))
        .output()
        .unwrap();
    assert_eq!(project_snapshot(scenario.project.path(), store), snapshot);
    let value = serde_json::from_slice(&output.stdout).unwrap();
    (output, value)
}

#[test]
fn cache_fetch_missing_receipt_is_unsupported_before_any_retrieval() {
    let scenario = fetch_project("fetch-missing-receipt");
    let path = scenario.project.path().join("tapid.lock");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    lock["packages"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .last()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("treeDigest");
    fs::write(&path, lock.to_string()).unwrap();
    let before = fs::read(&path).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["errors"][0]["code"], "CACHE_FETCH_MISSING_RECEIPT");
    assert_eq!(value["data"]["counters"]["unsupported_instances"], 1);
    assert_eq!(value["data"]["counters"]["fixture_reads"], 0);
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(!scenario.project.path().join("cold").exists());
}

#[test]
fn cache_fetch_mixed_and_warm_counts_are_actual_and_preserve_installed_layout() {
    let scenario = fetch_project("fetch-mixed");
    let root = scenario.project.path();
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tapid.lock")).unwrap()).unwrap();
    let parent = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .find(|p| p["name"] == "parent")
        .unwrap()["treeDigest"]
        .as_str()
        .unwrap();
    fs::remove_dir_all(root.join("seed/trees").join(parent)).unwrap();
    let installed = fs::read(root.join("node_modules/parent/package.json")).unwrap();
    let (output, value) = fetch_json(&scenario, "seed");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(value["data"]["counters"]["already_verified_trees"], 1);
    assert_eq!(value["data"]["counters"]["fetched_verified_trees"], 1);
    assert_eq!(value["data"]["counters"]["newly_published_trees"], 1);
    assert_eq!(value["data"]["logical_instances"], 2);
    assert_eq!(value["data"]["unique_trees"], 2);
    fs::remove_file(root.join("registry.json")).unwrap();
    let (output, value) = fetch_json(&scenario, "seed");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(value["data"]["counters"]["already_verified_trees"], 2);
    assert_eq!(value["data"]["counters"]["fetched_verified_trees"], 0);
    assert_eq!(value["data"]["counters"]["fixture_reads"], 0);
    assert_eq!(
        fs::read(root.join("node_modules/parent/package.json")).unwrap(),
        installed
    );
}

#[test]
fn cache_fetch_failure_retains_verified_work_without_claiming_publication() {
    let scenario = fetch_project("fetch-failure-counts");
    let root = scenario.project.path();
    let path = root.join("registry.json");
    let mut fixture: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fixture["packages"]
        .as_array_mut()
        .unwrap()
        .retain(|p| !(p["name"] == "shared" && p["version"] == "2.0.0"));
    fs::write(path, fixture.to_string()).unwrap();
    let lock = fs::read(root.join("tapid.lock")).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["data"]["counters"]["fetched_verified_trees"], 1);
    assert_eq!(value["data"]["counters"]["newly_published_trees"], 0);
    assert_eq!(value["data"]["counters"]["unavailable_instances"], 1);
    assert!(value["data"]["available_at_completion"].is_null());
    assert!(!root.join("cold/trees").exists());
    assert_eq!(fs::read(root.join("tapid.lock")).unwrap(), lock);
    assert_eq!(value["changes"]["state"], "unchanged");
}

#[test]
fn cache_fetch_corrupt_warm_tree_is_a_failure_without_retrieval() {
    let scenario = fetch_project("fetch-corrupt-warm");
    let root = scenario.project.path();
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tapid.lock")).unwrap()).unwrap();
    let digest = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["treeDigest"]
        .as_str()
        .unwrap();
    let path = root.join("seed/trees").join(digest).join("package.json");
    fs::write(&path, b"corrupt").unwrap();
    fs::remove_file(root.join("registry.json")).unwrap();
    let (output, value) = fetch_json(&scenario, "seed");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["errors"][0]["code"], "INTEGRITY_MISMATCH");
    assert_eq!(value["data"]["counters"]["fixture_reads"], 0);
    assert_eq!(value["data"]["counters"]["failed_instances"], 1);
    assert_eq!(value["data"]["counters"]["unprocessed_instances"], 1);
    assert_eq!(fs::read(path).unwrap(), b"corrupt");
}

#[test]
fn cache_fetch_rejects_changed_origin_even_when_cache_is_warm() {
    let scenario = fetch_project("fetch-origin");
    scenario
        .project
        .write(
            "tapid.toml",
            b"[registries.default]\nurl = \"https://registry.example.test\"\n",
        )
        .unwrap();
    let (output, value) = fetch_json(&scenario, "seed");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["errors"][0]["code"], "REGISTRY_CONFIGURATION_INVALID");
    assert_eq!(value["data"]["counters"]["fixture_reads"], 0);
    assert_eq!(value["data"]["counters"]["already_verified_trees"], 0);
}

#[test]
fn cache_fetch_source_readiness_does_not_require_lifecycle_policy() {
    let scenario = fetch_project("fetch-no-policy");
    scenario
        .project
        .write("tapid.lifecycle.toml", b"invalid lifecycle policy")
        .unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(value["data"]["lifecycle_ready"], false);
    assert!(
        !scenario
            .project
            .path()
            .join("cold/.tapid-lifecycle-key")
            .exists()
    );
}

#[test]
fn cache_fetch_rejects_pins_provenance_credentials_stale_inputs_and_recovery() {
    for mode in [
        "pin",
        "provenance",
        "credentials",
        "stale",
        "recovery",
        "missing",
        "invalid",
    ] {
        let scenario = fetch_project(&format!("fetch-preflight-{mode}"));
        let root = scenario.project.path();
        let path = root.join("tapid.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let expected = match mode {
            "pin" => {
                for package in lock["packages"].as_object_mut().unwrap().values_mut() {
                    package.as_object_mut().unwrap().remove("artifactUrl");
                }
                fs::write(&path, lock.to_string()).unwrap();
                "CACHE_FETCH_MISSING_PIN"
            }
            "provenance" => {
                for package in lock["packages"].as_object_mut().unwrap().values_mut() {
                    package["registryIntegrityDeclared"] = false.into();
                }
                fs::write(&path, lock.to_string()).unwrap();
                "INTEGRITY_MISMATCH"
            }
            "credentials" => {
                scenario.project.write("tapid.toml", b"[registries.default]\nurl = \"https://registry.npmjs.org\"\ntoken-env = \"TAPID_TEST_FETCH_MISSING_TOKEN\"\n").unwrap();
                "REGISTRY_AUTH_MISSING"
            }
            "stale" => {
                scenario
                    .project
                    .write(
                        "package.json",
                        b"{\"name\":\"changed\",\"version\":\"1.0.0\"}",
                    )
                    .unwrap();
                "LOCK_MANIFEST_MISMATCH"
            }
            "recovery" => {
                scenario
                    .project
                    .write(".tapid-lifecycle-journal.json", b"pending")
                    .unwrap();
                "RECOVERY_FAILED"
            }
            "missing" => {
                fs::remove_file(&path).unwrap();
                "LOCKFILE_MISSING"
            }
            _ => {
                fs::write(&path, b"invalid").unwrap();
                "LOCKFILE_INVALID"
            }
        };
        let before = fs::read(&path).ok();
        let installed = fs::read(root.join("node_modules/parent/package.json")).unwrap();
        let (output, value) = fetch_json(&scenario, "cold");
        assert_eq!(output.status.code(), Some(1), "{mode}: {output:?}");
        assert_eq!(value["errors"][0]["code"], expected, "{mode}: {value}");
        assert_eq!(value["data"]["counters"]["fixture_reads"], 0, "{mode}");
        assert_eq!(fs::read(&path).ok(), before);
        assert_eq!(
            fs::read(root.join("node_modules/parent/package.json")).unwrap(),
            installed
        );
        assert!(!root.join("cold").exists());
    }
}

#[test]
fn cache_fetch_rejects_imported_locks_with_and_without_receipts() {
    for receipts in [false, true] {
        let scenario = super::command_support::CliFixture::new("fetch-import");
        scenario
            .project
            .write(
                "package.json",
                include_bytes!("../fixtures/npm-import/package.json"),
            )
            .unwrap();
        scenario
            .project
            .write(
                "package-lock.json",
                include_bytes!("../fixtures/npm-import/package-lock.json"),
            )
            .unwrap();
        let imported = scenario
            .command(&["import-package-lock", "package-lock.json"])
            .output()
            .unwrap();
        assert!(imported.status.success(), "{imported:?}");
        if receipts {
            scenario
                .project
                .write(
                    "registry.json",
                    include_bytes!("../fixtures/npm-import/registry.json"),
                )
                .unwrap();
            let installed = scenario
                .command(&[
                    "install",
                    "--frozen",
                    "--registry-fixture",
                    "registry.json",
                    "--store-dir",
                ])
                .arg(scenario.project.path().join("seed"))
                .output()
                .unwrap();
            assert!(installed.status.success(), "{installed:?}");
        }
        let locked = fs::read(scenario.project.path().join("tapid.lock")).unwrap();
        let (output, value) = fetch_json(&scenario, "cold");
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            value["errors"][0]["code"],
            "CACHE_FETCH_IMPORTED_LOCK_UNSUPPORTED"
        );
        assert!(
            value["data"]["counters"]["unsupported_instances"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(
            fs::read(scenario.project.path().join("tapid.lock")).unwrap(),
            locked
        );
        assert!(!scenario.project.path().join("cold").exists());
    }
}

#[test]
fn cache_fetch_workspace_and_copied_sources_enable_offline_frozen_install() {
    use base64::Engine;
    for workspace in [false, true] {
        let scenario = super::command_support::CliFixture::new("fetch-workspace-copied");
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/npm-aliases.json")).unwrap();
        if workspace {
            scenario.project.write("package.json", br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"member":"workspace:*"}}"#).unwrap();
            scenario
                .project
                .write(
                    "packages/member/package.json",
                    br#"{"name":"member","version":"1.0.0","dependencies":{"h3":"1.0.0"}}"#,
                )
                .unwrap();
        } else {
            let package = fixture["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "h3" && p["version"] == "1.0.0")
                .unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    package["artifact"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("base64:")
                        .unwrap(),
                )
                .unwrap();
            scenario.project.write("vendor/pkg.tgz", &bytes).unwrap();
            scenario.project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"copied":"file:vendor/pkg.tgz"}}"#).unwrap();
        }
        scenario
            .project
            .write("registry.json", fixture.to_string().as_bytes())
            .unwrap();
        let root = scenario.project.path();
        let seeded = scenario
            .command(&[
                "install",
                "--registry-fixture",
                "registry.json",
                "--store-dir",
            ])
            .arg(root.join("seed"))
            .output()
            .unwrap();
        assert!(seeded.status.success(), "{seeded:?}");
        let path = root.join("tapid.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for package in lock["packages"].as_object_mut().unwrap().values_mut() {
            if package["registry"].as_str().unwrap().starts_with("https:") {
                package["artifactUrl"] = "https://registry.npmjs.org/exact.tgz".into();
            }
        }
        fs::write(&path, lock.to_string()).unwrap();
        let locked = fs::read(&path).unwrap();
        let (output, value) = fetch_json(&scenario, "cold");
        assert!(output.status.success(), "workspace={workspace}: {output:?}");
        if !workspace {
            assert_eq!(value["data"]["counters"]["local_reads"], 1);
        }
        fs::remove_file(root.join("registry.json")).unwrap();
        let offline = scenario
            .command(&["install", "--offline", "--frozen", "--store-dir"])
            .arg(root.join("cold"))
            .output()
            .unwrap();
        assert!(
            offline.status.success(),
            "workspace={workspace}: {offline:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), locked);
        if !workspace {
            fs::write(root.join("vendor/pkg.tgz"), b"changed").unwrap();
            let (output, value) = fetch_json(&scenario, "cold");
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(value["errors"][0]["code"], "INTEGRITY_MISMATCH");
        }
    }
}

#[test]
fn cache_fetch_shared_contexts_have_bounded_details_and_distinct_tree_counts() {
    let scenario = fetch_project("fetch-bounded-contexts");
    let path = scenario.project.path().join("tapid.lock");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let (key, package) = lock["packages"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, p)| p["name"] == "parent")
        .map(|(key, p)| (key.clone(), p.clone()))
        .unwrap();
    for index in 0..105 {
        let peer = format!("name=react;version=18.0.{index}");
        let new_key = key.replace("peer=-", &format!("peer={peer}"));
        let mut package = package.clone();
        package["peerContext"] = peer.into();
        lock["packages"]
            .as_object_mut()
            .unwrap()
            .insert(new_key, package);
    }
    fs::write(&path, lock.to_string()).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(value["data"]["logical_instances"], 107);
    assert_eq!(value["data"]["unique_trees"], 2);
    assert_eq!(value["data"]["counters"]["fixture_reads"], 2);
    assert_eq!(value["data"]["entries_total"], 107);
    assert_eq!(value["data"]["entries_emitted"], 100);
    assert_eq!(value["data"]["entries_omitted"], 7);
    assert_eq!(value["data"]["entries"].as_array().unwrap().len(), 100);
}

#[test]
fn cache_fetch_integrity_rejections_are_failed_not_unavailable() {
    for archive in [false, true] {
        let scenario = fetch_project("fetch-integrity");
        let path = scenario.project.path().join("tapid.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let package = lock["packages"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        if archive {
            use base64::Engine;
            package["artifactIntegrity"] = format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode([0; 64])
            )
            .into();
        } else {
            package["treeDigest"] = format!("sha256-{}", "0".repeat(64)).into();
        }
        fs::write(&path, lock.to_string()).unwrap();
        let before = fs::read(&path).unwrap();
        let (output, value) = fetch_json(&scenario, "cold");
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(value["errors"][0]["code"], "INTEGRITY_MISMATCH");
        assert_eq!(value["data"]["counters"]["failed_instances"], 1);
        assert_eq!(value["data"]["counters"]["unavailable_instances"], 0);
        assert_eq!(value["data"]["counters"]["newly_published_trees"], 0);
        assert!(!scenario.project.path().join("cold/trees").exists());
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[test]
fn cache_fetch_missing_warm_marker_is_verification_failure_not_unavailability() {
    let scenario = fetch_project("fetch-missing-marker");
    let root = scenario.project.path();
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tapid.lock")).unwrap()).unwrap();
    let digest = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["treeDigest"]
        .as_str()
        .unwrap();
    fs::remove_file(root.join("seed/trees").join(digest).join(".tapid-tree")).unwrap();
    let (output, value) = fetch_json(&scenario, "seed");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["errors"][0]["code"], "INTEGRITY_MISMATCH");
    assert_eq!(value["data"]["counters"]["unavailable_instances"], 0);
    assert_eq!(value["data"]["counters"]["failed_instances"], 1);
    assert_eq!(value["data"]["counters"]["fixture_reads"], 0);
}

#[test]
fn cache_fetch_sources_do_not_supply_authenticated_lifecycle_outputs() {
    let scenario = fetch_project("fetch-derived-output");
    let path = scenario.project.path().join("tapid.lock");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    lock["lockfileVersion"] = 9.into();
    let package = lock["packages"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    package["derivedHooks"] = serde_json::json!([{"hook":"postinstall", "key":format!("sha256-{}", "1".repeat(64)), "treeDigest":format!("sha256-{}", "2".repeat(64)),"attestation":format!("hmac-sha256-{}", "0".repeat(64))}]);
    fs::write(&path, lock.to_string()).unwrap();
    let before = fs::read(&path).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(value["data"]["lifecycle_ready"], false);
    assert!(
        !scenario
            .project
            .path()
            .join("cold/.tapid-lifecycle-key")
            .exists()
    );
    let output = scenario
        .command(&["install", "--offline", "--frozen", "--store-dir"])
        .arg(scenario.project.path().join("cold"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn cache_fetch_pinned_git_ignores_ref_refresh() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    let scenario = super::command_support::CliFixture::new("fetch-pinned-git");
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/npm-aliases.json")).unwrap();
    let mut package = fixture["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "h3" && p["version"] == "1.0.0")
        .unwrap()
        .clone();
    let bytes = STANDARD
        .decode(
            package["artifact"]
                .as_str()
                .unwrap()
                .strip_prefix("base64:")
                .unwrap(),
        )
        .unwrap();
    package["registry"] = format!(
        "git+https://example.test/repo.git#{}!sha256-{:x}",
        "a".repeat(40),
        Sha256::digest(&bytes)
    )
    .into();
    package["git_ref"] = "main".into();
    scenario.project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"copied":"git+https://example.test/repo.git#main"}}"#).unwrap();
    scenario
        .project
        .write(
            "registry.json",
            serde_json::json!({"packages":[package.clone()]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let root = scenario.project.path();
    let output = scenario
        .command(&[
            "install",
            "--registry-fixture",
            "registry.json",
            "--store-dir",
        ])
        .arg(root.join("seed"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let locked = fs::read(root.join("tapid.lock")).unwrap();
    package["git_ref"] = "unrelated-ref".into();
    scenario
        .project
        .write(
            "registry.json",
            serde_json::json!({"packages":[package]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(value["data"]["counters"]["fixture_reads"], 1);
    fs::remove_file(root.join("registry.json")).unwrap();
    let offline = scenario
        .command(&["install", "--offline", "--frozen", "--store-dir"])
        .arg(root.join("cold"))
        .output()
        .unwrap();
    assert!(offline.status.success(), "{offline:?}");
    assert_eq!(fs::read(root.join("tapid.lock")).unwrap(), locked);
}

fn fetch_test_tar(path: &str, content: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..path.len()].copy_from_slice(path.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum = header.iter().map(|byte| *byte as u32).sum::<u32>();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut archive = header.to_vec();
    archive.extend_from_slice(content);
    archive.resize(archive.len().div_ceil(512) * 512 + 1024, 0);
    archive
}

#[test]
fn cache_fetch_never_executes_root_or_dependency_hooks() {
    let scenario = super::command_support::CliFixture::new("fetch-no-hooks");
    let archive = fetch_test_tar("package/package.json", br#"{"name":"hooked","version":"1.0.0","scripts":{"postinstall":"echo forbidden > dependency-hook-ran"}}"#);
    scenario.project.write("vendor/pkg.tar", &archive).unwrap();
    scenario.project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"hooked":"file:vendor/pkg.tar"},"scripts":{"postinstall":"echo forbidden > root-hook-ran"}}"#).unwrap();
    let root = scenario.project.path();
    let output = scenario
        .command(&["install", "--store-dir"])
        .arg(root.join("seed"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let (output, _) = fetch_json(&scenario, "cold");
    assert!(output.status.success(), "{output:?}");
    assert!(!root.join("root-hook-ran").exists());
    assert!(
        !root
            .join("node_modules/hooked/dependency-hook-ran")
            .exists()
    );
    assert!(!root.join("cold/.tapid-lifecycle-key").exists());
    let trees = fs::read_dir(root.join("cold/trees")).unwrap();
    for tree in trees {
        assert!(!tree.unwrap().path().join("dependency-hook-ran").exists());
    }
}

#[test]
fn cache_fetch_archive_limits_reject_before_tree_publication() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha512};
    let scenario = fetch_project("fetch-archive-limits");
    // Existing adversarial fixture: a gzip tar header declaring 512 MiB + 1 byte.
    let encoded = "H4sIAAAAAAAAEytITM5OTE/Vz0ksSk9loA0wMDAwMDMxAdMGBgYI2gTKMjCEiUHEDQ2MTQ0YFAxo5B4UUFpckljEYEAXuwYhAAAASQbGAAIAAA==";
    let bytes = STANDARD.decode(encoded).unwrap();
    let path = scenario.project.path().join("tapid.lock");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    lock["packages"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()["artifactIntegrity"] =
        format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes))).into();
    fs::write(path, lock.to_string()).unwrap();
    let path = scenario.project.path().join("registry.json");
    let mut fixture: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fixture["packages"][0]["artifact"] = format!("base64:{encoded}").into();
    fs::write(path, fixture.to_string()).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["errors"][0]["code"], "ARCHIVE_INVALID");
    assert_eq!(value["data"]["counters"]["failed_instances"], 1);
    assert_eq!(value["data"]["counters"]["unavailable_instances"], 0);
    assert!(!scenario.project.path().join("cold/trees").exists());
}

#[test]
fn cache_fetch_incompatible_target_fails_before_retrieval() {
    let scenario = fetch_project("fetch-target");
    let path = scenario.project.path().join("tapid.lock");
    let text = fs::read_to_string(&path).unwrap();
    let foreign = if cfg!(windows) { "linux" } else { "win32" };
    fs::write(
        &path,
        text.replace("os=;cpu=;libc=", &format!("os={foreign};cpu=;libc=")),
    )
    .unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["errors"][0]["code"], "CACHE_FETCH_TARGET_UNSUPPORTED");
    assert_eq!(value["data"]["counters"]["unsupported_instances"], 2);
    assert_eq!(value["data"]["counters"]["fixture_reads"], 0);
    assert!(!scenario.project.path().join("cold").exists());
}

#[test]
fn cache_fetch_malformed_artifact_encoding_is_failed_not_unavailable() {
    let scenario = fetch_project("fetch-invalid-encoding");
    let path = scenario.project.path().join("registry.json");
    let mut fixture: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fixture["packages"][0]["artifact"] = "base64:invalid!".into();
    fs::write(path, fixture.to_string()).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["data"]["counters"]["failed_instances"], 1);
    assert_eq!(value["data"]["counters"]["unavailable_instances"], 0);
}

#[test]
fn cache_fetch_human_and_json_agree_with_explicit_project_selection() {
    let scenario = fetch_project("fetch-output-agreement");
    let root = scenario.project.path();
    let (output, json) = fetch_json(&scenario, "seed");
    assert!(output.status.success(), "{output:?}");
    let elsewhere = tapid_test_support::TempHome::new("fetch-working-directory").unwrap();
    let output = scenario
        .command(&["cache", "fetch", "--project-dir"])
        .arg(root)
        .arg("--store-dir")
        .arg(root.join("seed"))
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(&format!(
            "Target: {}",
            json["data"]["target"].as_str().unwrap()
        )),
        "{text}"
    );
    assert!(text.contains("complete_locked_graph"));
    assert!(text.contains("verified sources only"));
    let counters = text
        .lines()
        .find_map(|line| line.strip_prefix("Completed work: "))
        .unwrap();
    let counters: serde_json::Value = serde_json::from_str(counters).unwrap();
    assert_eq!(counters, json["data"]["counters"]);
}

#[test]
fn cache_fetch_failed_shared_tree_keeps_later_contexts_unprocessed() {
    use base64::Engine;
    let scenario = fetch_project("fetch-shared-failure");
    let path = scenario.project.path().join("tapid.lock");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let (key, mut package) = lock["packages"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, p)| p["name"] == "parent")
        .map(|(k, p)| (k.clone(), p.clone()))
        .unwrap();
    package["peerContext"] = "name=react;version=18.0.0".into();
    lock["packages"].as_object_mut().unwrap().insert(
        key.replace("peer=-", "peer=name=react;version=18.0.0"),
        package,
    );
    for package in lock["packages"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .filter(|p| p["name"] == "parent")
    {
        package["artifactIntegrity"] = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode([0; 64])
        )
        .into();
    }
    fs::write(path, lock.to_string()).unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["data"]["counters"]["failed_instances"], 1);
    assert_eq!(value["data"]["counters"]["unprocessed_instances"], 2);
}

#[test]
fn cache_fetch_failure_details_are_sanitized_bounded_and_have_sorted_pointers() {
    let scenario = super::command_support::CliFixture::new("fetch-untrusted-details");
    scenario
        .project
        .write("package.json", br#"{"name":"app","version":"1.0.0"}"#)
        .unwrap();
    let mut packages = serde_json::Map::new();
    for index in 0..12 {
        let key = format!(
            "https://user:never-log-this@registry.example/{index}/{}?token=never-log-this\u{1b}",
            "a".repeat(5000)
        );
        packages.insert(key, serde_json::json!({}));
    }
    scenario
        .project
        .write(
            "tapid.lock",
            serde_json::json!({"lockfileVersion":7,"packages":packages})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let (output, value) = fetch_json(&scenario, "cold");
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("never-log-this"));
    for entry in value["data"]["entries"].as_array().unwrap() {
        let key = entry["package"].as_str().unwrap();
        assert!(key.len() <= 4096);
        assert!(!key.chars().any(char::is_control));
    }
    let pointers = value["truncated_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(pointers.len(), 12);
    let mut sorted = pointers.clone();
    sorted.sort_unstable();
    assert_eq!(pointers, sorted);
}
