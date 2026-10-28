use super::*;
const INPUT: &str = include_str!("../../tests/fixtures/npm-import-package-lock.json");
const MANIFEST: &str = include_str!("../../tests/fixtures/npm-import-package.json");
const DIGEST: &str = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn npm_import_workspace_normalizes_bin_forms_and_retains_metadata() {
    for name in ["member", "@scope/member"] {
        let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
        let member = serde_json::json!({"name":name,"version":"1.0.0","bin":"./cli.js",
            "funding":{"type":"individual","url":"https://example.com/fund"},"deprecated":"Use the replacement"});
        let mut selected = member.clone();
        selected["bin"] = serde_json::json!({"member":"cli.js"});
        let input = serde_json::json!({"lockfileVersion":3,"packages":{
            "":root,"packages/member":selected,
            format!("node_modules/{name}"):{"resolved":"packages/member","link":true}
        }});
        let mut lock =
            ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).unwrap();
        let manifests = BTreeMap::from([(
            "packages/member".into(),
            (member.to_string(), DIGEST.into()),
        )]);
        lock.bind_workspace_manifests(&manifests).unwrap();
        let restored = ImportedNpmLockfile::from_json(&lock.to_json().unwrap()).unwrap();
        restored.validate_workspace_manifests(&manifests).unwrap();
        assert_eq!(
            restored.graph().unwrap().workspaces["packages/member"],
            selected
        );
        for bin in [
            serde_json::json!({"other":"cli.js"}),
            serde_json::json!({"member":"other.js"}),
            serde_json::json!("../cli.js"),
            Value::Null,
        ] {
            let mut changed = member.clone();
            changed["bin"] = bin;
            let manifests = BTreeMap::from([(
                "packages/member".into(),
                (changed.to_string(), DIGEST.into()),
            )]);
            assert!(restored.validate_workspace_manifests(&manifests).is_err());
        }
    }
}

#[test]
fn npm_import_workspace_compares_shorthand_bin_semantically() {
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
    let member = serde_json::json!({"name":"member","version":"1.0.0","bin":"cli.js"});
    let mut selected = member.clone();
    selected["bin"] = serde_json::json!({"member":"cli.js"});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":selected,
        "node_modules/member":{"resolved":"packages/member","link":true}
    }});
    let mut lock =
        ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).unwrap();
    lock.bind_workspace_manifests(&BTreeMap::from([(
        "packages/member".into(),
        (member.to_string(), DIGEST.into()),
    )]))
    .unwrap();
}
fn import(value: Value) -> Result<ImportedNpmLockfile, NpmImportError> {
    ImportedNpmLockfile::import(&value.to_string(), MANIFEST, DIGEST)
}
#[test]
fn npm_import_rejects_duplicate_json_fields() {
    let input = INPUT.replacen(
        "\"lockfileVersion\": 3",
        "\"lockfileVersion\": 2, \"lockfileVersion\": 3",
        1,
    );
    assert!(ImportedNpmLockfile::import(&input, MANIFEST, DIGEST).is_err());
}
#[test]
fn npm_import_rejects_unreachable_placements() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"]["node_modules/orphan"] = serde_json::json!({"name":"shared","version":"1.0.0","resolved":input["packages"]["node_modules/shared"]["resolved"],"integrity":input["packages"]["node_modules/shared"]["integrity"]});
    assert!(import(input).is_err());
}
#[test]
fn npm_import_rejects_wrong_name_type() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"]["node_modules/shared"]["name"] = serde_json::json!(42);
    assert!(import(input).is_err());
}

#[test]
fn npm_import_root_overlaps_require_matching_identities_and_versions() {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    for (production, development, selected_name, accepted) in [
        ("npm:a@1", "npm:b@1", "b", false),
        ("1", "npm:b@1", "b", false),
        ("npm:b@1", "1", "foo", false),
        ("npm:b@2", "npm:b@1", "b", false),
        ("npm:b@1", "npm:b@^1.0.0", "b", true),
        ("1", "^1.0.0", "foo", true),
    ] {
        // A plain range keeps the declared local name; aliases select another identity.
        let root = serde_json::json!({
            "name": "root",
            "version": "1.0.0",
            "dependencies": {"foo": production},
            "devDependencies": {"foo": development}
        });
        let input = serde_json::json!({
            "lockfileVersion": 3,
            "packages": {
                "": root,
                "node_modules/foo": {
                    "name": selected_name,
                    "version": "1.0.0",
                    "resolved": format!("https://registry.npmjs.org/{selected_name}/-/{selected_name}-1.0.0.tgz"),
                    "integrity": reference["packages"]["node_modules/shared"]["integrity"]
                }
            }
        });
        let result = ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST);
        if accepted {
            let graph = result.unwrap().graph().unwrap();
            assert_eq!(
                graph.packages[&graph.roots["foo"]].name.as_str(),
                selected_name
            );
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("overlapping root requirements disagree with selection"),
                "{production} / {development} / {selected_name}"
            );
        }
    }
}

#[test]
fn npm_import_platform_selection_preserves_required_and_optional_edges() {
    let input: Value = serde_json::from_str(INPUT).unwrap();
    let graph = import(input.clone()).unwrap().graph().unwrap();
    let selected = graph
        .selected_paths("linux", "x86_64", Some("glibc"))
        .unwrap();
    assert_eq!(selected.len(), 4);
    assert!(!selected.contains("node_modules/native"));
    let mut required = input;
    required["packages"]["node_modules/parent"]["os"] = serde_json::json!(["darwin"]);
    let graph = import(required).unwrap().graph().unwrap();
    assert!(
        graph
            .selected_paths("linux", "x86_64", Some("glibc"))
            .is_err()
    );
}
#[test]
fn npm_import_rejects_unsatisfied_or_missing_peer_providers() {
    for value in [
        serde_json::json!("2.0.0"),
        serde_json::json!("npm:absent@1.0.0"),
    ] {
        let mut input: Value = serde_json::from_str(INPUT).unwrap();
        input["packages"]["node_modules/consumer"]["peerDependencies"]["shared"] = value;
        assert!(import(input).is_err());
    }
}
#[test]
fn npm_import_allows_an_absent_optional_peer() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"]["node_modules/consumer"]["peerDependencies"]["missing"] =
        serde_json::json!("1.0.0");
    input["packages"]["node_modules/consumer"]["peerDependenciesMeta"] =
        serde_json::json!({"missing":{"optional":true}});
    let graph = import(input).unwrap().graph().unwrap();
    assert!(
        !graph.packages["node_modules/consumer"]
            .peers
            .contains_key("missing")
    );
}

#[test]
fn npm_import_rejects_conflicting_artifacts_across_peer_contexts() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    let mut nested = input["packages"]["node_modules/consumer"].clone();
    nested["peerDependencies"]["shared"] = serde_json::json!("2.0.0");
    nested["integrity"] = input["packages"]["node_modules/shared"]["integrity"].clone();
    input["packages"]["node_modules/parent"]["dependencies"]["consumer"] =
        serde_json::json!("1.0.0");
    input["packages"]["node_modules/parent/node_modules/consumer"] = nested;
    assert!(import(input).is_err());
}

#[test]
fn npm_import_ignores_libc_constraints_on_non_linux_platforms() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"]["node_modules/native"]["os"] = serde_json::json!([]);
    input["packages"]["node_modules/native"]["libc"] = serde_json::json!(["glibc"]);
    let graph = import(input).unwrap().graph().unwrap();
    assert!(
        graph
            .selected_paths("macos", "aarch64", None)
            .unwrap()
            .contains("node_modules/native")
    );
}

#[test]
fn npm_import_never_installs_an_optional_parent_with_a_missing_required_child() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"][""]["dependencies"]
        .as_object_mut()
        .unwrap()
        .remove("parent");
    input["packages"][""]["optionalDependencies"]["parent"] = serde_json::json!("1.0.0");
    input["packages"]["node_modules/parent/node_modules/shared"]["os"] =
        serde_json::json!(["darwin"]);
    let manifest = input["packages"][""].to_string();
    let graph = ImportedNpmLockfile::import(&input.to_string(), &manifest, DIGEST)
        .unwrap()
        .graph()
        .unwrap();
    assert!(
        graph
            .selected_paths("linux", "x86_64", Some("glibc"))
            .is_err()
    );
}

#[test]
fn npm_import_retains_alias_names_and_distinct_peer_contexts() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"][""]["dependencies"]["renamed"] = serde_json::json!("npm:shared@1.0.0");
    let mut alias = input["packages"]["node_modules/shared"].clone();
    alias["name"] = serde_json::json!("shared");
    input["packages"]["node_modules/renamed"] = alias;
    let mut nested = input["packages"]["node_modules/consumer"].clone();
    nested["peerDependencies"]["shared"] = serde_json::json!("2.0.0");
    input["packages"]["node_modules/parent"]["dependencies"]["consumer"] =
        serde_json::json!("1.0.0");
    input["packages"]["node_modules/parent/node_modules/consumer"] = nested;
    let manifest = input["packages"][""].to_string();
    let graph = ImportedNpmLockfile::import(&input.to_string(), &manifest, DIGEST)
        .unwrap()
        .graph()
        .unwrap();
    assert_eq!(graph.roots["renamed"], "node_modules/renamed");
    assert_eq!(
        graph.packages["node_modules/renamed"].name.as_str(),
        "shared"
    );
    assert_ne!(
        graph.peer_context("node_modules/consumer"),
        graph.peer_context("node_modules/parent/node_modules/consumer")
    );
}

#[test]
fn npm_import_preserves_workspace_links_and_member_registry_selections() {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
    let member =
        serde_json::json!({"name":"member","version":"1.0.0","dependencies":{"shared":"1"}});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":member,
        "node_modules/member":{"resolved":"packages/member","link":true},
        "node_modules/shared":reference["packages"]["node_modules/shared"]
    }});
    let lock = ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).unwrap();
    let graph = lock.graph().unwrap();
    assert_eq!(graph.roots["shared"], "node_modules/shared");
    assert_eq!(graph.packages.len(), 1);
    assert_eq!(
        lock.to_json().unwrap(),
        ImportedNpmLockfile::from_json(&lock.to_json().unwrap())
            .unwrap()
            .to_json()
            .unwrap()
    );
}

#[test]
fn npm_import_workspace_rejects_unsafe_and_unrepresentable_links() {
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
    let base = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":{"name":"member","version":"1.0.0"},
        "node_modules/member":{"resolved":"packages/member","link":true}
    }});
    for (field, value) in [
        ("resolved", serde_json::json!("../outside")),
        ("resolved", serde_json::json!("/absolute")),
        ("resolved", serde_json::json!("missing")),
        ("link", serde_json::json!(false)),
        ("link", serde_json::json!("true")),
        ("integrity", serde_json::json!("unexpected")),
    ] {
        let mut input = base.clone();
        input["packages"]["node_modules/member"][field] = value;
        assert!(
            ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).is_err(),
            "{field}"
        );
    }
    let mut stale = base.clone();
    stale["packages"][""]["dependencies"] = serde_json::json!({"member":"2"});
    assert!(
        ImportedNpmLockfile::import(
            &stale.to_string(),
            &stale["packages"][""].to_string(),
            DIGEST
        )
        .is_err()
    );
    let mut nested = base;
    nested["packages"]["packages/member/node_modules/other"] =
        serde_json::json!({"version":"1.0.0"});
    assert!(ImportedNpmLockfile::import(&nested.to_string(), &root.to_string(), DIGEST).is_err());
}

#[test]
fn npm_import_accepts_empty_workspace_declarations() {
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":[]});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{"":root}});
    assert!(ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).is_ok());
}

#[test]
fn npm_import_rejects_dangling_workspace_manifest_receipts() {
    let lock = ImportedNpmLockfile::import(INPUT, MANIFEST, DIGEST).unwrap();
    let mut wire: Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    wire["workspaceManifestDigests"] = serde_json::json!({"packages/unknown":DIGEST});
    assert!(ImportedNpmLockfile::from_json(&wire.to_string()).is_err());
}

#[test]
fn npm_import_workspace_preserves_optional_dependency_overrides() {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"shared":"2"},"optionalDependencies":{"shared":"1"}});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":{"name":"member","version":"1.0.0"},
        "node_modules/member":{"resolved":"packages/member","link":true},
        "node_modules/shared":reference["packages"]["node_modules/shared"]
    }});
    let graph = ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST)
        .unwrap()
        .graph()
        .unwrap();
    assert!(graph.optional_roots.contains("shared"));
}

#[test]
fn npm_import_workspace_rejects_optional_registry_edges_to_local_members() {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"shared":"1"}});
    let mut shared = reference["packages"]["node_modules/shared"].clone();
    shared["optionalDependencies"] = serde_json::json!({"member":"1"});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":{"name":"member","version":"1.0.0"},
        "node_modules/member":{"resolved":"packages/member","link":true},
        "node_modules/shared":shared
    }});
    assert!(ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).is_err());
}

#[test]
fn npm_import_accepts_workspace_names_omitted_by_npm() {
    let input = include_str!("../../tests/fixtures/npm-workspace-package-lock.json");
    let value: Value = serde_json::from_str(input).unwrap();
    let root = value["packages"][""].to_string();
    let lock = ImportedNpmLockfile::import(input, &root, DIGEST).unwrap();
    assert_eq!(
        lock.graph().unwrap().workspaces["packages/member"]["name"],
        "member"
    );
}

#[test]
fn npm_import_workspace_keeps_nested_registry_packages_with_local_names() {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"parent":"1"}});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/shared":{"name":"shared","version":"1.0.0"},
        "node_modules/shared":{"resolved":"packages/shared","link":true},
        "node_modules/parent":reference["packages"]["node_modules/parent"],
        "node_modules/parent/node_modules/shared":reference["packages"]["node_modules/parent/node_modules/shared"]
    }});
    let graph = ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST)
        .unwrap()
        .graph()
        .unwrap();
    assert_eq!(
        graph.packages["node_modules/parent"].dependencies["shared"],
        "node_modules/parent/node_modules/shared"
    );
}

fn member_placement_lock() -> Value {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/a","packages/a/children/b"]});
    let mut consumer = reference["packages"]["node_modules/consumer"].clone();
    consumer["peerDependencies"]["shared"] = serde_json::json!("2");
    serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,
        "packages/a":{"name":"a","version":"1.0.0","dependencies":{"shared":"2"}},
        "packages/a/children/b":{"name":"b","version":"1.0.0","dependencies":{"consumer":"1"},"optionalDependencies":{"native":"1"},"peerDependencies":{"shared":"2"}},
        "node_modules/a":{"resolved":"packages/a","link":true},
        "node_modules/b":{"resolved":"packages/a/children/b","link":true},
        "packages/a/node_modules/shared":reference["packages"]["node_modules/parent/node_modules/shared"],
        "packages/a/children/b/node_modules/consumer":consumer,
        "packages/a/children/b/node_modules/native":reference["packages"]["node_modules/native"]
    }})
}

#[test]
fn npm_import_member_placements_preserve_ancestor_peers_and_optional_selection() {
    let input = member_placement_lock();
    let lock = ImportedNpmLockfile::import(
        &input.to_string(),
        &input["packages"][""].to_string(),
        DIGEST,
    )
    .unwrap();
    let graph = lock.graph().unwrap();
    assert_eq!(
        graph.member_roots["packages/a/children/b"].dependencies["shared"],
        "packages/a/node_modules/shared"
    );
    assert_eq!(
        graph.packages["packages/a/children/b/node_modules/consumer"].peers["shared"],
        "packages/a/node_modules/shared"
    );
    let selected = graph.selected_paths("linux", "x64", Some("glibc")).unwrap();
    assert!(selected.contains("packages/a/children/b/node_modules/consumer"));
    assert!(!selected.contains("packages/a/children/b/node_modules/native"));
    let mut required = input.clone();
    required["packages"]["packages/a/children/b"]["dependencies"]["native"] =
        serde_json::json!("1");
    required["packages"]["packages/a/children/b"]
        .as_object_mut()
        .unwrap()
        .remove("optionalDependencies");
    let graph = ImportedNpmLockfile::import(
        &required.to_string(),
        &required["packages"][""].to_string(),
        DIGEST,
    )
    .unwrap()
    .graph()
    .unwrap();
    assert!(graph.selected_paths("linux", "x64", Some("glibc")).is_err());
}

#[test]
fn npm_import_member_placements_reject_shadow_mismatches_and_unknown_roots() {
    let input = member_placement_lock();
    let root = input["packages"][""].to_string();
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let mut mismatch = input.clone();
    mismatch["packages"]["packages/a/children/b/node_modules/shared"] =
        reference["packages"]["node_modules/shared"].clone();
    assert!(ImportedNpmLockfile::import(&mismatch.to_string(), &root, DIGEST).is_err());
    for path in [
        "undeclared/node_modules/shared",
        "packages/a/../other/node_modules/shared",
        "packages/a/node_modules/../shared",
    ] {
        let mut invalid = input.clone();
        invalid["packages"][path] = reference["packages"]["node_modules/shared"].clone();
        assert!(
            ImportedNpmLockfile::import(&invalid.to_string(), &root, DIGEST).is_err(),
            "{path}"
        );
    }
}

#[test]
fn npm_import_member_links_preserve_nearest_local_identity() {
    let mut input = member_placement_lock();
    input["packages"]["packages/a/children/b"]["dependencies"]["a"] = serde_json::json!("1");
    input["packages"]["packages/a/children/b"]["peerDependencies"]["a"] = serde_json::json!("1");
    input["packages"]["packages/a/children/b"]["peerDependenciesMeta"] =
        serde_json::json!({"a":{"optional":true}});
    input["packages"]["packages/a/children/b/node_modules/a"] =
        serde_json::json!({"resolved":"packages/a","link":true});
    let root = input["packages"][""].to_string();
    assert!(ImportedNpmLockfile::import(&input.to_string(), &root, DIGEST).is_ok());
    input["packages"]["packages/a/children/b"]["dependencies"]["a"] = serde_json::json!("2");
    assert!(ImportedNpmLockfile::import(&input.to_string(), &root, DIGEST).is_err());
}

#[test]
fn graph_cache_is_reused_and_deserialization_still_validates_graph() {
    let lock = ImportedNpmLockfile::import(INPUT, MANIFEST, DIGEST).unwrap();
    let first = lock.graph().unwrap();
    let second = lock.graph().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    let restored = ImportedNpmLockfile::from_json(&lock.to_json().unwrap()).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &restored.graph().unwrap(),
        &restored.graph().unwrap()
    ));

    let mut invalid: Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    invalid["npmLock"]["lockfileVersion"] = Value::from(1);
    assert!(ImportedNpmLockfile::from_json(&invalid.to_string()).is_err());
    let unchecked: ImportedNpmLockfile = serde_json::from_value(invalid).unwrap();
    assert!(unchecked.graph().is_err());
}

#[test]
fn imported_placements_reject_ordinary_selection_contract() {
    let lock = ImportedNpmLockfile::import(INPUT, MANIFEST, DIGEST).unwrap();
    let mut wire: Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    wire["selection"] = serde_json::json!({
        "target":{"os":"linux","cpu":"x86_64","libc":"gnu"},
        "profile":"development","omissions":[]
    });
    assert!(ImportedNpmLockfile::from_json(&wire.to_string()).is_err());
    wire.as_object_mut().unwrap().remove("selection");
    wire["lockfileVersion"] = crate::SELECTION_LOCKFILE_VERSION.into();
    assert!(ImportedNpmLockfile::from_json(&wire.to_string()).is_err());
}

#[test]
fn imported_omission_ledger_round_trip_and_hostile_inputs() {
    let manifest = r#"{"name":"app","optionalDependencies":{"missing":"^1"}}"#;
    let input = serde_json::json!({"lockfileVersion":3,"packages":{"":serde_json::from_str::<Value>(manifest).unwrap()}});
    let lock = ImportedNpmLockfile::import(&input.to_string(), manifest, DIGEST).unwrap();
    let mut wire: Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    wire["lockfileVersion"] = 12.into();
    wire["placementSelection"] = serde_json::json!({
        "target":{"os":"linux","cpu":"x86_64","libc":"gnu"},
        "profile":"development",
        "omissions":[{"declaringPlacement":"","name":"missing","requirement":"^1","reason":"metadataAbsent"}]
    });
    let restored = ImportedNpmLockfile::from_json(&wire.to_string()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&restored.to_json().unwrap()).unwrap(),
        wire
    );
    assert!(restored.validate_replay(manifest, DIGEST).is_err());
    let selection = restored.placement_selection().unwrap();
    let before_replay = restored.to_json().unwrap();
    for changed in [
        crate::SelectionTarget {
            os: "darwin".into(),
            ..selection.target.clone()
        },
        crate::SelectionTarget {
            cpu: "aarch64".into(),
            ..selection.target.clone()
        },
    ] {
        assert!(
            restored
                .validate_replay_with_selection(manifest, DIGEST, &changed, selection.profile)
                .is_err()
        );
    }
    assert!(
        restored
            .validate_replay_with_selection(
                manifest,
                "wrong-digest",
                &selection.target,
                selection.profile
            )
            .is_err()
    );
    assert!(
        lock.validate_replay_with_selection(manifest, DIGEST, &selection.target, selection.profile)
            .is_err()
    );

    assert!(
        restored
            .validate_replay_with_selection(manifest, DIGEST, &selection.target, selection.profile)
            .is_ok()
    );
    assert!(
        restored
            .validate_replay_with_selection(
                manifest,
                DIGEST,
                &selection.target,
                crate::SelectionProfile::Production
            )
            .is_err()
    );
    let mut changed = selection.target.clone();
    changed.libc = None;
    assert!(
        restored
            .validate_replay_with_selection(manifest, DIGEST, &changed, selection.profile)
            .is_err()
    );
    assert!(
        restored
            .validate_replay_with_selection(
                r#"{"name":"changed"}"#,
                DIGEST,
                &selection.target,
                selection.profile
            )
            .is_err()
    );

    assert_eq!(restored.to_json().unwrap(), before_replay);
    for (field, value) in [
        ("declaringPlacement", serde_json::json!("../escape")),
        (
            "declaringPlacement",
            serde_json::json!("node_modules/missing"),
        ),
        ("name", serde_json::json!("other")),
        ("requirement", serde_json::json!("^2")),
        ("reason", serde_json::json!("integrityMismatch")),
    ] {
        let mut hostile = wire.clone();
        hostile["placementSelection"]["omissions"][0][field] = value;
        assert!(
            ImportedNpmLockfile::from_json(&hostile.to_string()).is_err(),
            "{field}"
        );
    }
    let mut hostile = wire.clone();
    hostile["lockfileVersion"] = 8.into();
    assert!(ImportedNpmLockfile::from_json(&hostile.to_string()).is_err());
    wire.as_object_mut().unwrap().remove("placementSelection");
    assert!(ImportedNpmLockfile::from_json(&wire.to_string()).is_err());
}

#[test]
fn imported_omissions_preserve_declaring_placements_and_atomic_updates() {
    let root = serde_json::json!({"name":"app","workspaces":["packages/*"],"optionalDependencies":{"missing":"^1"}});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,
        "packages/member":{"name":"member","version":"1.0.0","optionalDependencies":{"missing":"^2"}},
        "node_modules/member":{"resolved":"packages/member","link":true}
    }});
    let mut lock =
        ImportedNpmLockfile::import(&input.to_string(), &root.to_string(), DIGEST).unwrap();
    let selection: ImportedSelection = serde_json::from_value(serde_json::json!({
        "target":{"os":"linux","cpu":"x86_64","libc":null},"profile":"production",
        "omissions":[
            {"declaringPlacement":"","name":"missing","requirement":"^1","reason":"metadataAbsent"},
            {"declaringPlacement":"packages/member","name":"missing","requirement":"^2","reason":"noUsableCandidate"}
        ]
    })).unwrap();
    lock.set_placement_selection(selection.clone()).unwrap();
    assert_eq!(lock.placement_selection(), Some(&selection));
    let before = lock.to_json().unwrap();
    for change in 0..5 {
        let mut invalid = selection.clone();
        match change {
            0 => invalid.omissions.reverse(),
            1 => invalid.omissions.push(invalid.omissions[1].clone()),
            2 => invalid.omissions[1].requirement = "^1".into(),
            3 => invalid.target.os = "../linux".into(),
            _ => invalid.omissions[0].name = "member".into(),
        }
        assert!(lock.set_placement_selection(invalid).is_err());
        assert_eq!(lock.to_json().unwrap(), before);
    }
    let restored = ImportedNpmLockfile::from_json(&before).unwrap();
    assert_eq!(restored.placement_selection(), Some(&selection));
}

#[test]
fn imported_omission_cannot_hide_selected_optional_provider() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    let name = input["packages"][""]["dependencies"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let requirement = input["packages"][""]["dependencies"][&name].clone();
    input["packages"][""]["optionalDependencies"][&name] = requirement.clone();
    let root = input["packages"][""].to_string();
    let mut lock = ImportedNpmLockfile::import(&input.to_string(), &root, DIGEST).unwrap();
    let selection: ImportedSelection = serde_json::from_value(serde_json::json!({
        "target":{"os":"linux","cpu":"x86_64","libc":null},"profile":"development",
        "omissions":[{"declaringPlacement":"","name":name,"requirement":requirement,"reason":"metadataAbsent"}]
    })).unwrap();
    let before = lock.to_json().unwrap();
    assert!(lock.set_placement_selection(selection).is_err());
    assert_eq!(lock.to_json().unwrap(), before);
}

#[test]
fn imported_omissions_retain_nested_alias_declarations_and_reject_hoisted_targets() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    input["packages"]["node_modules/parent"]["optionalDependencies"] =
        serde_json::json!({"renamed":"npm:absent@^2", "shared":"2.0.0"});
    let mut lock = ImportedNpmLockfile::import(&input.to_string(), MANIFEST, DIGEST).unwrap();
    let selection: ImportedSelection = serde_json::from_value(serde_json::json!({
        "target":{"os":"linux","cpu":"x86_64","libc":null},"profile":"development",
        "omissions":[{"declaringPlacement":"node_modules/parent","name":"renamed","requirement":"npm:absent@^2","reason":"noUsableCandidate"}]
    })).unwrap();
    lock.set_placement_selection(selection.clone()).unwrap();
    let before = lock.to_json().unwrap();
    assert_eq!(
        ImportedNpmLockfile::from_json(&before)
            .unwrap()
            .placement_selection(),
        Some(&selection)
    );
    let mut invalid = selection;
    invalid.omissions[0].name = "shared".into();
    invalid.omissions[0].requirement = "2.0.0".into();
    assert!(lock.set_placement_selection(invalid).is_err());
    assert_eq!(lock.to_json().unwrap(), before);
}

#[test]
fn imported_selection_writer_rejects_unchecked_schema_confusion() {
    let lock = ImportedNpmLockfile::import(INPUT, MANIFEST, DIGEST).unwrap();
    let mut wire: Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    wire["lockfileVersion"] = 12.into();
    let unchecked: ImportedNpmLockfile = serde_json::from_value(wire).unwrap();
    assert!(unchecked.to_json().is_err());
}

#[test]
fn imported_platform_cuts_preserve_alias_placements_and_required_reachability() {
    let mut input: Value = serde_json::from_str(INPUT).unwrap();
    let mut package = input["packages"]["node_modules/native"].clone();
    package["optional"] = true.into();
    package["name"] = "native".into();
    input["packages"]["node_modules/renamed"] = package;
    input["packages"][""]["optionalDependencies"]["renamed"] = "npm:native@1.0.0".into();
    let manifest = input["packages"][""].to_string();
    let mut lock = ImportedNpmLockfile::import(&input.to_string(), &manifest, DIGEST).unwrap();
    let target = crate::SelectionTarget {
        os: "linux".into(),
        cpu: "x86_64".into(),
        libc: None,
    };
    lock.record_platform_selection(target.clone(), crate::SelectionProfile::Development)
        .unwrap();
    let selection = lock.placement_selection().unwrap().clone();
    assert!(
        selection
            .omissions
            .iter()
            .any(|cut| cut.name == "renamed" && cut.requirement == "npm:native@1.0.0")
    );
    let before = lock.to_json().unwrap();
    let mut forged = selection.clone();
    forged
        .omissions
        .iter_mut()
        .find(|cut| cut.name == "renamed")
        .unwrap()
        .reason = crate::OmissionReason::MetadataAbsent;
    assert!(lock.set_placement_selection(forged).is_err());
    assert_eq!(lock.to_json().unwrap(), before);
    input["packages"]["node_modules/parent"]["dependencies"]["renamed"] = "npm:native@1.0.0".into();
    let mut required = ImportedNpmLockfile::import(&input.to_string(), &manifest, DIGEST).unwrap();
    let before = required.to_json().unwrap();
    assert!(
        required
            .record_platform_selection(target, crate::SelectionProfile::Development)
            .is_err()
    );
    assert_eq!(required.to_json().unwrap(), before);
}
