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
fn imported_selection_comparison_ignores_verification_receipts() {
    let before = ImportedNpmLockfile::import(INPUT, MANIFEST, DIGEST).unwrap();
    let mut after = before.clone();
    let trees = before
        .graph()
        .unwrap()
        .packages
        .keys()
        .map(|path| (path.clone(), DIGEST.to_owned()))
        .collect();
    after.record_verified_trees(trees).unwrap();
    let receipts = before
        .graph()
        .unwrap()
        .packages
        .iter()
        .map(|(path, package)| {
            (
                path.clone(),
                ImportedNpmArtifactReceipt {
                    tree_digest: DIGEST.into(),
                    resolved: package.resolved.clone(),
                    integrity: package.integrity.to_string(),
                },
            )
        })
        .collect();
    after.record_verified_artifacts(receipts).unwrap();
    let after = ImportedNpmLockfile::from_json(&after.to_json().unwrap()).unwrap();
    assert_eq!(
        before.compare_selections(&after).unwrap(),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            added: 0,
            changed: 0,
            reused: before.graph().unwrap().packages.len(),
            removed: 0
        })
    );
}

fn compare_inputs(before: &Value, after: &Value) -> ImportedSelectionComparison {
    let import = |input: &Value| {
        ImportedNpmLockfile::import(
            &input.to_string(),
            &input["packages"][""].to_string(),
            DIGEST,
        )
        .unwrap()
    };
    let before = import(before);
    let after = import(after);
    let comparison = before.compare_selections(&after).unwrap();
    if let ImportedSelectionComparison::Available(counts) = comparison {
        assert_eq!(
            before.graph().unwrap().packages.len(),
            counts.removed + counts.changed + counts.reused
        );
        assert_eq!(
            after.graph().unwrap().packages.len(),
            counts.added + counts.changed + counts.reused
        );
    }
    comparison
}

#[test]
fn imported_selection_comparison_normalizes_platform_sets() {
    let mut before: Value = serde_json::from_str(INPUT).unwrap();
    before["packages"]["node_modules/native"]["os"] = serde_json::json!(["linux", "darwin"]);
    let mut after = before.clone();
    after["packages"]["node_modules/native"]["os"] =
        serde_json::json!(["darwin", "linux", "linux"]);
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            reused: 5,
            ..Default::default()
        })
    );
}

#[test]
fn imported_selection_comparison_classifies_pins_placements_and_topology() {
    let before: Value = serde_json::from_str(INPUT).unwrap();
    let mut after = before.clone();
    after["packages"]["node_modules/parent"]["resolved"] =
        serde_json::json!("https://registry.example.com/parent/-/parent-1.0.0.tgz");
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            changed: 1,
            reused: 4,
            ..Default::default()
        })
    );
    let mut after = before.clone();
    after["packages"]["node_modules/parent/node_modules/shared"]["version"] =
        serde_json::json!("2.0.1");
    after["packages"]["node_modules/parent/node_modules/shared"]["resolved"] =
        serde_json::json!("https://registry.npmjs.org/shared/-/shared-2.0.1.tgz");
    after["packages"]["node_modules/parent"]["dependencies"]["shared"] = serde_json::json!("^2");
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            changed: 1,
            reused: 4,
            ..Default::default()
        })
    );
    let mut after = before.clone();
    let moved = after["packages"]
        .as_object_mut()
        .unwrap()
        .remove("node_modules/parent/node_modules/shared")
        .unwrap();
    after["packages"]["node_modules/shared-two"] = moved;
    // An alias edge preserves the actual identity at its new placement.
    after["packages"]["node_modules/shared-two"]["name"] = serde_json::json!("shared");
    after["packages"]["node_modules/parent"]["dependencies"] =
        serde_json::json!({"shared-two":"npm:shared@2.0.0"});
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            added: 1,
            removed: 1,
            changed: 1,
            reused: 3
        })
    );
    let mut after = before.clone();
    after["packages"][""]["dependencies"]
        .as_object_mut()
        .unwrap()
        .remove("consumer");
    after["packages"]
        .as_object_mut()
        .unwrap()
        .remove("node_modules/consumer");
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Unavailable(ImportedSelectionUnavailable::EntryPointsChanged)
    );
    let empty = serde_json::json!({"lockfileVersion":3,"packages":{"":{}}});
    assert_eq!(
        compare_inputs(&empty, &empty),
        ImportedSelectionComparison::Available(Default::default())
    );
}

#[test]
fn imported_selection_comparison_detects_each_selection_field() {
    let before: Value = serde_json::from_str(INPUT).unwrap();
    for (field, value) in [
        (
            "resolved",
            serde_json::json!("https://other.example/parent/-/parent-1.0.0.tgz"),
        ),
        (
            "integrity",
            before["packages"]["node_modules/shared"]["integrity"].clone(),
        ),
        ("optional", serde_json::json!(true)),
        ("os", serde_json::json!(["linux"])),
        ("cpu", serde_json::json!(["x64"])),
        ("libc", serde_json::json!(["glibc"])),
    ] {
        let mut after = before.clone();
        after["packages"]["node_modules/parent"][field] = value;
        assert_eq!(
            compare_inputs(&before, &after),
            ImportedSelectionComparison::Available(ImportedSelectionCounts {
                changed: 1,
                reused: 4,
                ..Default::default()
            }),
            "{field}"
        );
    }
    let mut after = before.clone();
    after["packages"]["node_modules/parent"]["optionalDependencies"] =
        serde_json::json!({"shared":"2.0.0"});
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            changed: 1,
            reused: 4,
            ..Default::default()
        })
    );
    let mut after = before.clone();
    after["packages"]["node_modules/consumer"]["peerDependenciesMeta"] =
        serde_json::json!({"shared":{"optional":true}});
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            changed: 1,
            reused: 4,
            ..Default::default()
        })
    );
}

#[test]
fn imported_selection_comparison_guards_workspace_topology() {
    let before = member_placement_lock();
    assert_eq!(
        compare_inputs(&before, &before),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            reused: 3,
            ..Default::default()
        })
    );
    let mut after = before.clone();
    after["packages"]["packages/a"]["version"] = serde_json::json!("1.0.1");
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Unavailable(
            ImportedSelectionUnavailable::WorkspaceTopologyChanged
        )
    );
    let mut after = before.clone();
    after["packages"]["packages/a"]["dependencies"] = serde_json::json!({});
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Unavailable(ImportedSelectionUnavailable::EntryPointsChanged)
    );
}

#[test]
fn imported_selection_comparison_retains_alias_identity_and_peer_context() {
    let reference: Value = serde_json::from_str(INPUT).unwrap();
    let mut before = serde_json::json!({"lockfileVersion":3,"packages":{
        "":{"dependencies":{"local":"npm:shared@1"}},
        "node_modules/local":reference["packages"]["node_modules/shared"]
    }});
    before["packages"]["node_modules/local"]["name"] = serde_json::json!("shared");
    let mut after = before.clone();
    after["packages"][""]["dependencies"]["local"] = serde_json::json!("npm:parent@1");
    let mut parent = reference["packages"]["node_modules/parent"].clone();
    parent.as_object_mut().unwrap().remove("dependencies");
    parent["name"] = serde_json::json!("parent");
    after["packages"]["node_modules/local"] = parent;
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            changed: 1,
            ..Default::default()
        })
    );

    let mut parent = reference["packages"]["node_modules/parent"].clone();
    parent["dependencies"]["consumer"] = serde_json::json!("1");
    let mut consumer = reference["packages"]["node_modules/consumer"].clone();
    consumer["peerDependencies"]["shared"] = serde_json::json!("2");
    let before = serde_json::json!({"lockfileVersion":3,"packages":{
        "":{"dependencies":{"parent":"1"}},
        "node_modules/parent":parent,
        "node_modules/parent/node_modules/consumer":consumer,
        "node_modules/shared":reference["packages"]["node_modules/parent/node_modules/shared"]
    }});
    let mut after = before.clone();
    let shared = after["packages"]
        .as_object_mut()
        .unwrap()
        .remove("node_modules/shared")
        .unwrap();
    after["packages"]["node_modules/parent/node_modules/shared"] = shared;
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Available(ImportedSelectionCounts {
            added: 1,
            changed: 2,
            reused: 0,
            removed: 1
        })
    );
}

#[test]
fn imported_selection_comparison_guards_optional_entry_points_and_links() {
    let before: Value = serde_json::from_str(INPUT).unwrap();
    let mut after = before.clone();
    after["packages"][""]
        .as_object_mut()
        .unwrap()
        .remove("optionalDependencies");
    after["packages"][""]["dependencies"]["native"] = serde_json::json!("1.0.0");
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Unavailable(ImportedSelectionUnavailable::EntryPointsChanged)
    );
    let before = member_placement_lock();
    let mut after = before.clone();
    after["packages"]["packages/a/children/b"]
        .as_object_mut()
        .unwrap()
        .remove("optionalDependencies");
    after["packages"]["packages/a/children/b"]["dependencies"]["native"] = serde_json::json!("1");
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Unavailable(ImportedSelectionUnavailable::EntryPointsChanged)
    );
    let mut after = before.clone();
    after["packages"]["packages/a/node_modules/b"] =
        serde_json::json!({"resolved":"packages/a/children/b","link":true});
    assert_eq!(
        compare_inputs(&before, &after),
        ImportedSelectionComparison::Unavailable(
            ImportedSelectionUnavailable::WorkspaceTopologyChanged
        )
    );
}
