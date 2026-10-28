use tapid_registry_client::{NpmInventory, NpmTagTarget};

#[test]
fn inventory_retains_uninstallable_history_and_dangling_latest() {
    let inventory = NpmInventory::from_metadata(
        &"https://registry.npmjs.org".parse().unwrap(),
        "foo".parse().unwrap(),
        br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"}},"dist-tags":{"latest":"9.0.0"}}"#,
    ).unwrap();
    assert_eq!(inventory.versions().len(), 1);
    assert!(inventory.tags_reported());
    assert_eq!(inventory.versions()[0].to_string(), "1.0.0");
    assert_eq!(
        inventory.tags()["latest"],
        NpmTagTarget::MissingVersion("9.0.0".parse().unwrap())
    );
}

#[test]
fn inventory_accounts_for_unsupported_keys_and_validates_only_selected_facts() {
    let inventory = NpmInventory::from_metadata(
        &"https://private.example".parse().unwrap(),
        "@scope/foo".parse().unwrap(),
        br#"{"name":"@scope/foo","versions":{
            "1.0.0":{"name":"@scope/foo","version":"1.0.0","peerDependencies":{"host":"^2"},"peerDependenciesMeta":{"host":{"optional":true}},"engines":{"node":">=22","unknown":"custom"},"deprecated":"old","bin":{"foo":"index.js"},"dist":false},
            "2.0.0":false,"3.0.0+build":{},"bad":{}
        }}"#,
    ).unwrap();
    assert_eq!(inventory.unsupported_versions(), 2);
    let facts = inventory.facts(&"1.0.0".parse().unwrap()).unwrap();
    assert_eq!(facts.identity.name.as_str(), "@scope/foo");
    assert_eq!(facts.peers.as_ref().unwrap()[0].requirement, "^2");
    assert!(facts.peers.as_ref().unwrap()[0].optional);
    assert_eq!(facts.engines.as_ref().unwrap()["unknown"], "custom");
    assert_eq!(facts.deprecated.as_deref(), Some("old"));
    assert_eq!(
        facts.executables.as_deref(),
        Some(["foo".to_owned()].as_slice())
    );
    assert!(inventory.facts(&"2.0.0".parse().unwrap()).is_err());
}

#[test]
fn inventory_live_uses_full_metadata_and_matches_snapshot() {
    use tapid_registry_client::{HttpResponse, HttpTransport, NpmRegistry, TransportError};
    struct Fixture;
    const BODY: &[u8] = br#"{"name":"@scope/foo","versions":{"1.0.0":{"name":"@scope/foo","version":"1.0.0","bin":"cli.js"}},"dist-tags":{"latest":"1.0.0","bad":false}}"#;
    impl HttpTransport for Fixture {
        fn get(&self, _: &str) -> Result<HttpResponse, TransportError> {
            panic!("full Accept required")
        }
        fn get_with_accept(&self, url: &str, accept: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, "https://private.example/@scope%2Ffoo");
            assert_eq!(accept, "application/json");
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".into()),
                body: BODY.to_vec(),
            })
        }
    }
    let origin = "https://private.example".parse().unwrap();
    let live = NpmRegistry::new(Fixture, origin)
        .inventory("@scope/foo")
        .unwrap();
    assert_eq!(live.tags()["bad"], NpmTagTarget::Unsupported);
    assert_eq!(
        live.facts(&"1.0.0".parse().unwrap()).unwrap().executables,
        Some(vec!["foo".into()])
    );
}

#[test]
fn inventory_rejects_oversize_snapshots_before_json_parsing() {
    let body = vec![b' '; 32 * 1024 * 1024 + 1];
    let error = NpmInventory::from_metadata(
        &"https://registry.npmjs.org".parse().unwrap(),
        "foo".parse().unwrap(),
        &body,
    )
    .unwrap_err();
    assert!(
        matches!(error, tapid_registry_client::MetadataError::InvalidJson(message) if message == "metadata exceeds 32 MiB")
    );
}

#[test]
fn inventory_rejects_malformed_selected_declarations_and_conflicting_identity() {
    for entry in [
        json_entry("name", serde_json::json!("other")),
        json_entry("version", serde_json::json!("9.0.0")),
        json_entry("peerDependencies", serde_json::json!({"host":false})),
        json_entry("engines", serde_json::json!({"node":false})),
        json_entry("deprecated", serde_json::json!(false)),
        json_entry("bin", serde_json::json!({"foo":false})),
    ] {
        let body =
            serde_json::to_vec(&serde_json::json!({"name":"foo","versions":{"1.0.0":entry}}))
                .unwrap();
        let inventory = NpmInventory::from_metadata(
            &"https://registry.npmjs.org".parse().unwrap(),
            "foo".parse().unwrap(),
            &body,
        )
        .unwrap();
        assert!(inventory.facts(&"1.0.0".parse().unwrap()).is_err());
    }
    for body in [
        b"[]".as_slice(),
        br#"{"name":"other","versions":{}}"#,
        br#"{"name":"foo","versions":false}"#,
        br#"{"name":"foo","versions":{},"dist-tags":false}"#,
    ] {
        assert!(
            NpmInventory::from_metadata(
                &"https://registry.npmjs.org".parse().unwrap(),
                "foo".parse().unwrap(),
                body
            )
            .is_err()
        );
    }
}

fn json_entry(field: &str, value: serde_json::Value) -> serde_json::Value {
    let mut entry = serde_json::json!({"name":"foo","version":"1.0.0"});
    entry[field] = value;
    entry
}

#[test]
fn inventory_accepts_metadata_at_the_input_limit() {
    let mut body =
        br#"{"name":"foo","versions":{"1.0.0":{"name":"foo","version":"1.0.0"}}}"#.to_vec();
    body.resize(32 * 1024 * 1024, b' ');
    let started = std::time::Instant::now();
    let inventory = NpmInventory::from_metadata(
        &"https://registry.npmjs.org".parse().unwrap(),
        "foo".parse().unwrap(),
        &body,
    )
    .unwrap();
    assert_eq!(inventory.versions()[0].to_string(), "1.0.0");
    assert!(!inventory.tags_reported());
    assert_eq!(
        inventory.facts(&"1.0.0".parse().unwrap()).unwrap().peers,
        None
    );
    eprintln!(
        "inventory input-limit fixture: {} bytes, {:?}",
        body.len(),
        started.elapsed()
    );
}
