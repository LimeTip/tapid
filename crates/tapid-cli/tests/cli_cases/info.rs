use serde_json::{Value, json};
use tapid_test_support::{TempHome, TempProject};

fn run(project: &TempProject, spec: &str, extra: &[&str]) -> std::process::Output {
    let home = TempHome::new("info").unwrap();
    let stores = ["tapid/store", "Library/Caches/tapid/store"];
    for store in stores {
        home.write(format!("{store}/sentinel"), b"unchanged store")
            .unwrap();
    }
    let output = super::command_support::command_in_home(
        project.path(),
        home.path(),
        &[
            "info",
            spec,
            "--json",
            "--registry-metadata",
            "metadata.json",
        ],
        true,
    )
    .args(extra)
    .output()
    .unwrap();
    for store in stores {
        assert_eq!(
            std::fs::read(home.path().join(store).join("sentinel")).unwrap(),
            b"unchanged store"
        );
        assert_eq!(
            std::fs::read_dir(home.path().join(store)).unwrap().count(),
            1
        );
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 2);
    output
}

#[test]
fn info_dangling_latest_retains_history_without_fallback_or_project_reads() {
    let project = TempProject::new("info").unwrap();
    project
        .write("package.json", b"malformed unrelated manifest")
        .unwrap();
    project.write("metadata.json", &serde_json::to_vec(&json!({"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"}},"dist-tags":{"latest":"9.0.0"}})).unwrap()).unwrap();
    let output = run(&project, "foo", &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["operation"], "info");
    assert_eq!(result["changes"]["state"], "unchanged");
    assert_eq!(result["data"]["selection"]["status"], "missing_tag_target");
    assert!(result["data"]["selection"]["version"].is_null());
    assert_eq!(result["data"]["versions"]["entries"][0], "1.0.0");
    let output = run(&project, "foo@1.0.0", &[]);
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["selection"]["version"], "1.0.0");
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 2);
}

#[test]
fn info_reports_declarations_separately_from_explicit_runtime_checks() {
    let project = TempProject::new("info-runtime").unwrap();
    project.write("metadata.json", &serde_json::to_vec(&json!({"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","engines":{"node":">=22","custom":"next"},"peerDependencies":{"host":"^2"},"peerDependenciesMeta":{"host":{"optional":true}},"deprecated":"https://user:secret@example.org/?token=secret","bin":{"foo":"index.js"}}},"dist-tags":{"latest":"1.0.0"}})).unwrap()).unwrap();
    let output = run(&project, "foo", &["--node-version", "20.0.0"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["checks"]["node"]["status"], "checked");
    assert_eq!(result["data"]["checks"]["node"]["outcome"], "mismatch");
    assert_eq!(
        result["data"]["facts"]["peers"]["entries"][0]["optional"],
        true
    );
    assert_eq!(
        result["data"]["facts"]["engines"]["entries"][0]["name"],
        "custom"
    );
    assert_eq!(
        result["data"]["checks"]["engine_requirements"][0]["status"],
        "unsupported"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert!(result["retry"].is_null());
    let output = run(&project, "foo", &[]);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["checks"]["node"]["status"], "not_checked");
}

#[test]
fn info_json_consumer_traverses_independent_pages_and_rejects_stale_tokens() {
    let project = TempProject::new("info-pages").unwrap();
    let mut versions = serde_json::Map::new();
    let mut tags = serde_json::Map::new();
    for i in 0..105 {
        let version = format!("1.0.{i}");
        versions.insert(version.clone(), json!({"name":"foo","version":version}));
        tags.insert(format!("tag-{i:03}"), json!(version));
    }
    let body =
        serde_json::to_vec(&json!({"name":"foo","versions":versions,"dist-tags":tags})).unwrap();
    project.write("metadata.json", &body).unwrap();
    let mut cursor = None::<String>;
    let mut seen = Vec::new();
    loop {
        let mut args = vec!["--page-size", "17"];
        if let Some(cursor) = &cursor {
            args.extend(["--versions-after", cursor]);
        }
        let output = run(&project, "foo@*", &args);
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["data"]["selection"]["version"], "1.0.104");
        seen.extend(
            result["data"]["versions"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned()),
        );
        cursor = result["data"]["versions"]["next"]
            .as_str()
            .map(str::to_owned);
        assert_eq!(result["data"]["tags"]["returned"], 17);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 105);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 105);
    let mut cursor = None::<String>;
    let mut seen_tags = Vec::new();
    loop {
        let mut args = vec!["--page-size", "23"];
        if let Some(cursor) = &cursor {
            args.extend(["--tags-after", cursor]);
        }
        let output = run(&project, "foo@*", &args);
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["data"]["selection"]["version"], "1.0.104");
        assert_eq!(result["data"]["versions"]["preceding"], 0);
        seen_tags.extend(
            result["data"]["tags"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tag| tag["name"].as_str().unwrap().to_owned()),
        );
        cursor = result["data"]["tags"]["next"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen_tags.len(), 105);
    seen_tags.sort();
    seen_tags.dedup();
    assert_eq!(seen_tags.len(), 105);
    let first: Value =
        serde_json::from_slice(&run(&project, "foo@*", &["--page-size", "1"]).stdout).unwrap();
    let token = first["data"]["versions"]["next"].as_str().unwrap();
    let wrong = run(&project, "foo@*", &["--tags-after", token]);
    assert_eq!(wrong.status.code(), Some(1));
    let mut changed = body;
    changed.push(b' ');
    project.write("metadata.json", &changed).unwrap();
    let stale = run(&project, "foo@*", &["--versions-after", token]);
    let stale: Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["errors"][0]["code"], "REGISTRY_CONTINUATION_STALE");
}

#[test]
fn info_bounds_actual_escaped_envelope_and_preserves_continuation_progress() {
    let project = TempProject::new("info-budget").unwrap();
    let mut versions = serde_json::Map::new();
    let mut tags = serde_json::Map::new();
    let mut peers = serde_json::Map::new();
    let mut engines = serde_json::Map::new();
    let mut bin = serde_json::Map::new();
    let escaped = format!("{}é\u{001b}", "\"\\".repeat(3000));
    for i in 0..100 {
        let version = format!("1.0.{i}-{}", "a".repeat(5000));
        versions.insert(version.clone(), json!({"name":"foo","version":version}));
        tags.insert(format!("tag-{i:03}-{escaped}"), json!(version));
        peers.insert(format!("peer-{i:03}"), json!(escaped));
        engines.insert(format!("engine-{i:03}-{escaped}"), json!(escaped));
        bin.insert(format!("bin-{i:03}-{escaped}"), json!("index.js"));
    }
    versions.insert("2.0.0".into(), json!({"name":"foo","version":"2.0.0","peerDependencies":peers,"engines":engines,"bin":bin}));
    project
        .write(
            "metadata.json",
            &serde_json::to_vec(&json!({"name":"foo","versions":versions,"dist-tags":tags}))
                .unwrap(),
        )
        .unwrap();
    let output = run(&project, "foo@2.0.0", &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "{} bytes",
        output.stdout.len()
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], "partial");
    assert!(result["truncated_fields"].as_array().unwrap().len() > 100);
    assert!(result["data"]["versions"]["returned"].as_u64().unwrap() > 0);
    let token = result["data"]["versions"]["next"].as_str().unwrap();
    let output = run(&project, "foo@2.0.0", &["--versions-after", token]);
    let next: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(next["data"]["versions"]["preceding"].as_u64().unwrap() > 0);
    assert!(output.stdout.len() <= 4 * 1024 * 1024);
}

#[test]
fn info_snapshots_ignore_credentials_and_preserve_pending_project_state() {
    let project = TempProject::new("info-read-only").unwrap();
    project.write("tapid.toml", b"[registries.'@scope']\nurl='https://private.example'\ntoken-env='UNAVAILABLE_TOKEN'\n").unwrap();
    project.write("metadata.json", br#"{"name":"@scope/foo","versions":{"1.0.0":{"name":"@scope/foo","version":"1.0.0"}},"dist-tags":{"latest":"1.0.0"}}"#).unwrap();
    let protected = [
        "package.json",
        "tapid.lock",
        ".tapid-transaction.json",
        ".tapid-lifecycle-journal.json",
        "node_modules/sentinel",
        "store/sentinel",
    ];
    for path in protected {
        project
            .write(path, b"malformed state must not be read or changed")
            .unwrap();
    }
    let output = run(&project, "npm:@scope/foo", &["--offline"]);
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["data"]["source"]["origin"],
        "https://private.example"
    );
    assert_eq!(result["data"]["source"]["mode"], "snapshot");
    let failure = run(&project, "npm:@scope/foo@missing", &[]);
    assert_eq!(failure.status.code(), Some(1));
    for path in protected {
        assert_eq!(
            std::fs::read(project.path().join(path)).unwrap(),
            b"malformed state must not be read or changed"
        );
    }
    assert!(!String::from_utf8_lossy(&output.stdout).contains("UNAVAILABLE_TOKEN"));
}

#[test]
fn info_rejects_unsupported_inputs_and_offline_without_snapshot_before_routing() {
    let project = TempProject::new("info-no-network").unwrap();
    project
        .write("tapid.toml", b"malformed config must not be read")
        .unwrap();
    for spec in [
        "jsr:@scope/foo",
        "git+https://example.org/foo",
        "file:foo",
        "workspace:foo",
        "local@npm:actual@1",
        "foo@",
        "foo@npm:actual",
    ] {
        let output = run(&project, spec, &[]);
        assert_eq!(output.status.code(), Some(1), "{spec}: {output:?}");
        assert!(output.stderr.is_empty());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["errors"][0]["code"], "INVALID_REQUEST",
            "{spec}: {result}"
        );
        assert!(result["data"].is_null());
    }
    let home = TempHome::new("info-offline").unwrap();
    let output = super::command_support::command_in_home(
        project.path(),
        home.path(),
        &["info", "foo", "--offline", "--json"],
        true,
    )
    .output()
    .unwrap();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["code"], "REGISTRY_METADATA_UNAVAILABLE");
}

#[test]
fn info_never_confirms_runtime_when_displayed_requirement_is_incomplete() {
    let project = TempProject::new("info-incomplete-runtime").unwrap();
    project.write("metadata.json", &serde_json::to_vec(&json!({"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","engines":{"node":format!(">=20{}", " ".repeat(5000))}}}})).unwrap()).unwrap();
    let output = run(&project, "foo@1.0.0", &["--node-version", "22.0.0"]);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["checks"]["node"]["status"], "unsupported");
    assert!(result["data"]["checks"]["node"]["outcome"].is_null());
    assert_eq!(
        result["data"]["checks"]["engine_requirements"][0]["status"],
        "unsupported"
    );
    assert_eq!(result["outcome"], "partial");
}

#[test]
fn info_selects_ranges_tags_and_prereleases_without_fallback() {
    let project = TempProject::new("info-selectors").unwrap();
    project.write("metadata.json", br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"},"1.1.0":{"name":"foo","version":"1.1.0"},"2.0.0-beta.1":{"name":"foo","version":"2.0.0-beta.1"},"2.0.0+build":{}},"dist-tags":{"latest":false,"next":"2.0.0-beta.1"}}"#).unwrap();
    for (selector, expected) in [
        ("foo@^1", "1.1.0"),
        ("foo@*", "1.1.0"),
        ("foo@next", "2.0.0-beta.1"),
        ("foo@2.0.0-beta.1", "2.0.0-beta.1"),
    ] {
        let output = run(&project, selector, &[]);
        assert!(output.status.success(), "{selector}: {output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["data"]["selection"]["version"], expected);
        assert_eq!(result["data"]["versions"]["unsupported"], 1);
        assert_eq!(result["data"]["versions"]["complete"], false);
        assert_eq!(result["outcome"], "partial");
    }
    for selector in ["foo", "foo@missing", "foo@9.0.0"] {
        let output = run(&project, selector, &[]);
        assert_eq!(output.status.code(), Some(1));
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(result["data"]["selection"]["version"].is_null());
    }
}

#[test]
fn info_rejects_malformed_cursors_and_accepts_changed_page_size() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let project = TempProject::new("info-cursors").unwrap();
    project.write("metadata.json", br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"},"2.0.0":{"name":"foo","version":"2.0.0"},"3.0.0":{"name":"foo","version":"3.0.0"}}}"#).unwrap();
    let first: Value =
        serde_json::from_slice(&run(&project, "foo@*", &["--page-size", "1"]).stdout).unwrap();
    let token = first["data"]["versions"]["next"].as_str().unwrap();
    let second: Value = serde_json::from_slice(
        &run(
            &project,
            "foo@*",
            &["--page-size", "2", "--versions-after", token],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(
        second["data"]["versions"]["entries"],
        json!(["2.0.0", "1.0.0"])
    );
    assert!(second["data"]["versions"]["next"].is_null());
    let mut impossible: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token).unwrap()).unwrap();
    impossible["next"] = json!(999);
    let impossible = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&impossible).unwrap());
    for bad in ["invalid".to_owned(), "a".repeat(2049), impossible] {
        let output = run(&project, "foo@*", &["--versions-after", &bad]);
        assert_eq!(output.status.code(), Some(1));
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["errors"][0]["code"], "REGISTRY_CONTINUATION_INVALID");
    }
}

#[test]
fn info_marks_unsupported_peer_and_engine_expressions_without_compatibility_claims() {
    let project = TempProject::new("info-expressions").unwrap();
    project.write("metadata.json", br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","peerDependencies":{"host":"workspace:*"},"engines":{"node":"latest"}}}}"#).unwrap();
    let output = run(&project, "foo@1.0.0", &["--node-version", "22.0.0"]);
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["data"]["checks"]["peer_requirements"][0]["status"],
        "unsupported"
    );
    assert_eq!(result["data"]["checks"]["node"]["status"], "unsupported");
    assert_eq!(result["data"]["checks"]["peers"], "not_checked");
    assert_eq!(result["outcome"], "partial");
}

#[test]
fn info_selection_failure_has_typed_source_context() {
    let project = TempProject::new("info-diagnostic").unwrap();
    project
        .write(
            "metadata.json",
            br#"{"name":"foo","versions":{},"dist-tags":{}}"#,
        )
        .unwrap();
    let output = run(&project, "foo", &[]);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["context"]["package"]["name"], "foo");
    assert_eq!(
        result["errors"][0]["context"]["package"]["source"],
        "https://registry.npmjs.org"
    );
    assert_eq!(
        result["errors"][0]["context"]["declaration"]["origin"],
        "metadata"
    );
    assert_eq!(
        result["errors"][0]["context"]["declaration"]["pointer"],
        "/dist-tags/latest"
    );
}

#[test]
fn info_bounds_nested_collections_and_leaves_omitted_node_unchecked() {
    let project = TempProject::new("info-nested").unwrap();
    let mut peers = serde_json::Map::new();
    let mut engines = serde_json::Map::new();
    let mut bin = serde_json::Map::new();
    for i in 0..101 {
        peers.insert(format!("peer-{i:03}"), json!("^1"));
        engines.insert(format!("engine-{i:03}"), json!("*"));
        bin.insert(format!("bin-{i:03}"), json!("index.js"));
    }
    engines.insert("node".into(), json!(">=20"));
    project.write("metadata.json", &serde_json::to_vec(&json!({"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","peerDependencies":peers,"engines":engines,"bin":bin}}})).unwrap()).unwrap();
    let output = run(&project, "foo@1.0.0", &["--node-version", "22.0.0"]);
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    for field in ["peers", "engines", "executables"] {
        assert_eq!(result["data"]["facts"][field]["returned"], 100);
        assert_eq!(result["data"]["facts"][field]["complete"], false);
    }
    assert_eq!(result["data"]["facts"]["engines"]["known_omitted"], 2);
    assert_eq!(result["data"]["checks"]["node"]["status"], "unsupported");
    assert_eq!(result["outcome"], "partial");
}

#[test]
fn info_preserves_unsupported_tag_coverage_even_when_selecting_an_exact_version() {
    let project = TempProject::new("info-tag-coverage").unwrap();
    project.write("metadata.json", br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"}},"dist-tags":{"broken":false}}"#).unwrap();
    let output = run(&project, "foo@1.0.0", &[]);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], "partial");
    assert_eq!(result["data"]["tags"]["unsupported"], 1);
    assert_eq!(result["data"]["tags"]["complete"], false);
}

#[test]
fn info_keeps_tapid_interpretation_out_of_registry_fact_records() {
    let project = TempProject::new("info-facts-checks").unwrap();
    project.write("metadata.json", br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0","peerDependencies":{"host":"^1"},"engines":{"custom":"next"}}}}"#).unwrap();
    let output = run(&project, "foo@1.0.0", &[]);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["data"]["facts"]["peers"]["entries"][0]
            .get("requirement_status")
            .is_none()
    );
    assert!(
        result["data"]["facts"]["engines"]["entries"][0]
            .get("check")
            .is_none()
    );
    assert_eq!(
        result["data"]["checks"]["engine_requirements"][0]["fact_index"],
        0
    );
    assert_eq!(
        result["data"]["checks"]["engine_requirements"][0]["status"],
        "unsupported"
    );
    assert_eq!(
        result["data"]["checks"]["peer_requirements"][0]["status"],
        "supported"
    );
}

#[test]
fn info_live_private_route_requires_credentials_before_network() {
    let project = TempProject::new("info-live-auth").unwrap();
    let home = TempHome::new("info-live-auth").unwrap();
    project.write("tapid.toml", b"[registries.'@scope']\nurl='https://private.example'\ntoken-env='TAPID_TEST_INFO_MISSING_TOKEN'\n").unwrap();
    let output = super::command_support::command_in_home(
        project.path(),
        home.path(),
        &["info", "@scope/foo", "--json"],
        true,
    )
    .env_remove("TAPID_TEST_INFO_MISSING_TOKEN")
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["code"], "REGISTRY_AUTH_MISSING");
    assert_eq!(result["changes"]["state"], "unchanged");
    assert!(result["data"].is_null());
    assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 1);
}

#[test]
fn info_malformed_winning_record_fails_without_selecting_a_lower_version() {
    let project = TempProject::new("info-malformed-winner").unwrap();
    project.write("metadata.json", br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"},"2.0.0":{"name":"other","version":"2.0.0"}},"dist-tags":{"latest":"2.0.0"}}"#).unwrap();
    for spec in ["foo", "foo@*", "foo@2.0.0"] {
        let output = run(&project, spec, &[]);
        assert_eq!(output.status.code(), Some(1), "{spec}: {output:?}");
        assert!(output.stderr.is_empty());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["errors"][0]["code"], "REGISTRY_METADATA_INVALID");
        assert!(result["data"].is_null());
        assert_eq!(result["changes"]["state"], "unchanged");
    }
    let output = run(&project, "foo@1.0.0", &[]);
    assert!(output.status.success(), "{output:?}");
}
