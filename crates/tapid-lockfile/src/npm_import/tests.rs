use super::*;
const INPUT: &str = include_str!("../../tests/fixtures/npm-import-package-lock.json");
const MANIFEST: &str = include_str!("../../tests/fixtures/npm-import-package.json");
const DIGEST: &str = "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
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
