use std::collections::BTreeMap;
use tapid_lockfile::{LockedPackage, Lockfile, RegistryIntegrityProvenance};

#[test]
fn imported_paths_preserve_alias_placements_and_contextual_duplicates() {
    let mut input: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-import-package-lock.json")).unwrap();
    input["packages"][""]["dependencies"]["renamed"] = serde_json::json!("npm:shared@1.0.0");
    let mut alias = input["packages"]["node_modules/shared"].clone();
    alias["name"] = serde_json::json!("shared");
    input["packages"]["node_modules/renamed"] = alias;
    let mut nested = input["packages"]["node_modules/consumer"].clone();
    nested["peerDependencies"]["shared"] = serde_json::json!("2.0.0");
    input["packages"]["node_modules/parent"]["dependencies"]["consumer"] =
        serde_json::json!("1.0.0");
    input["packages"]["node_modules/parent/node_modules/consumer"] = nested;
    let lock = tapid_lockfile::ImportedNpmLockfile::import(
        &input.to_string(),
        &input["packages"][""].to_string(),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let before = lock.to_json().unwrap();
    let graph = lock.graph().unwrap();
    let roots = graph
        .roots
        .iter()
        .map(|(name, key)| tapid_lockfile::LockedGraphRoot {
            key: key.clone(),
            name: name.clone(),
            kind: "dependencies".into(),
        })
        .collect::<Vec<_>>();
    let paths = graph.paths_to(&roots, &"consumer".parse().unwrap());
    assert!(!paths.truncated);
    assert_eq!(paths.paths.len(), 2);
    assert_eq!(
        paths.paths[0].steps.last().unwrap().key,
        "node_modules/consumer"
    );
    assert_eq!(
        paths.paths[1].steps.last().unwrap().key,
        "node_modules/parent/node_modules/consumer"
    );
    assert_ne!(
        graph.peer_context("node_modules/consumer"),
        graph.peer_context("node_modules/parent/node_modules/consumer")
    );
    let shared = graph.paths_to(&roots, &"shared".parse().unwrap());
    assert!(
        shared
            .paths
            .iter()
            .any(|path| path
                .steps
                .last()
                .is_some_and(|step| step.key == "node_modules/renamed"
                    && step.via.as_deref() == Some("renamed")))
    );
    let native = graph.paths_to(&roots, &"native".parse().unwrap());
    assert_eq!(
        native.paths.len(),
        1,
        "recorded optional platforms must not be host-filtered"
    );
    assert_eq!(lock.to_json().unwrap(), before);
}

#[test]
fn imported_workspace_paths_retain_member_source_and_exact_peer_placement() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-import-package-lock.json")).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
    let member = serde_json::json!({"name":"member","version":"1.0.0","dependencies":{"consumer":"1"},"optionalDependencies":{"native":"1"}});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root, "packages/member":member,
        "node_modules/member":{"resolved":"packages/member","link":true},
        "packages/member/node_modules/consumer":reference["packages"]["node_modules/consumer"],
        "packages/member/node_modules/shared":reference["packages"]["node_modules/shared"],
        "packages/member/node_modules/native":reference["packages"]["node_modules/native"]
    }});
    let lock = tapid_lockfile::ImportedNpmLockfile::import(
        &input.to_string(),
        &root.to_string(),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let graph = lock.graph().unwrap();
    let roots = [tapid_lockfile::LockedGraphRoot {
        key: "packages/member".into(),
        name: "member".into(),
        kind: "workspace".into(),
    }];
    let paths = graph.paths_to(&roots, &"shared".parse().unwrap());
    assert!(!paths.truncated);
    assert_eq!(paths.paths.len(), 1);
    let steps = &paths.paths[0].steps;
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0].key, "packages/member");
    assert_eq!(steps[0].source_kind, "workspace");
    assert_eq!(steps[2].key, "packages/member/node_modules/shared");
    assert_eq!(steps[2].edge_kind.as_deref(), Some("peer"));
    assert_eq!(
        graph
            .paths_to(&roots, &"native".parse().unwrap())
            .paths
            .len(),
        1
    );
}

#[test]
fn imported_workspace_selection_traverses_local_workspace_dependencies() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-import-package-lock.json")).unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"]});
    let input = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,
        "packages/a":{"name":"a","version":"1.0.0","dependencies":{"b":"1"}},
        "packages/b":{"name":"b","version":"1.0.0","dependencies":{"a":"1","shared":"1"}},
        "node_modules/a":{"resolved":"packages/a","link":true},
        "node_modules/b":{"resolved":"packages/b","link":true},
        "node_modules/shared":reference["packages"]["node_modules/shared"]
    }});
    let lock = tapid_lockfile::ImportedNpmLockfile::import(
        &input.to_string(),
        &root.to_string(),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let graph = lock.graph().unwrap();
    let paths = graph.paths_to_key(
        &[tapid_lockfile::LockedGraphRoot {
            key: "packages/a".into(),
            name: "a".into(),
            kind: "workspace".into(),
        }],
        "node_modules/shared",
    );
    assert!(!paths.truncated);
    assert_eq!(paths.paths.len(), 1);
    assert_eq!(
        paths.paths[0]
            .steps
            .iter()
            .map(|step| step.key.as_str())
            .collect::<Vec<_>>(),
        ["packages/a", "packages/b", "node_modules/shared"]
    );
    assert_eq!(paths.paths[0].steps[1].source_kind, "workspace");
    assert_eq!(paths.paths[0].steps[1].via.as_deref(), Some("b"));
}

#[test]
fn imported_exact_paths_do_not_spend_the_display_budget_on_other_aliases() {
    let mut input: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/npm-import-package-lock.json")).unwrap();
    let mut package = input["packages"]["node_modules/shared"].clone();
    package["name"] = serde_json::json!("shared");
    for index in 0..51 {
        let name = format!("alias{index:02}");
        input["packages"][""]["dependencies"][&name] = serde_json::json!("npm:shared@1.0.0");
        input["packages"][format!("node_modules/{name}")] = package.clone();
    }
    let lock = tapid_lockfile::ImportedNpmLockfile::import(
        &input.to_string(),
        &input["packages"][""].to_string(),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let graph = lock.graph().unwrap();
    let roots = graph
        .roots
        .iter()
        .map(|(name, key)| tapid_lockfile::LockedGraphRoot {
            key: key.clone(),
            name: name.clone(),
            kind: "dependencies".into(),
        })
        .collect::<Vec<_>>();
    let by_name = graph.paths_to(&roots, &"shared".parse().unwrap());
    assert_eq!(by_name.paths.len(), 50);
    assert!(by_name.truncated);
    let exact = graph.paths_to_key(&roots, "node_modules/alias50");
    assert_eq!(exact.paths.len(), 1);
    assert!(!exact.truncated);
    assert_eq!(exact.paths[0].steps[0].via.as_deref(), Some("alias50"));
}

fn package(registry: &str, name: &str) -> LockedPackage {
    LockedPackage::new_with_provenance(
        registry,
        name,
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap()
}

#[test]
fn projection_preserves_registry_collisions_and_alias_bindings() {
    let public = package("https://registry.npmjs.org", "shared");
    let private = package("https://private.example", "shared");
    let public_key = public.key();
    let private_key = private.key();
    let mut root = package("https://registry.npmjs.org", "root");
    root.add_alias_dependency("private-alias", &private_key)
        .unwrap();
    root.add_dependency("shared", &public_key).unwrap();
    let root_key = root.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([root, public, private]).unwrap();
    lock.set_roots([root_key.clone()]).unwrap();
    lock.set_root_bindings(BTreeMap::from([("root".into(), root_key.clone())]))
        .unwrap();
    let graph = lock.graph().unwrap();
    assert_eq!(graph.nodes().len(), 3);
    assert_ne!(
        graph.nodes()[&public_key].source,
        graph.nodes()[&private_key].source
    );
    assert_eq!(graph.roots(), &[("root".to_owned(), root_key.clone())]);
    let edges = &graph.edges()[&root_key];
    assert!(
        edges
            .iter()
            .any(|edge| edge.name == "private-alias" && edge.target == private_key)
    );
    assert!(
        edges
            .iter()
            .any(|edge| edge.name == "shared" && edge.target == public_key)
    );
    let paths = graph.paths_to_key(
        &[tapid_lockfile::LockedGraphRoot {
            key: root_key,
            name: "root".into(),
            kind: "dependencies".into(),
        }],
        &private_key,
    );
    assert!(!paths.truncated);
    assert_eq!(paths.paths.len(), 1);
    assert_eq!(paths.paths[0].steps[1].key, private_key);
    assert_eq!(
        paths.paths[0].steps[1].via.as_deref(),
        Some("private-alias")
    );
}

#[test]
fn projection_keeps_multiple_contextual_roots_with_the_same_actual_name() {
    let first = package("https://registry.npmjs.org", "shared");
    let second = package("https://private.example", "shared");
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.set_roots(Vec::<String>::new()).unwrap();
    let keys = [first.key(), second.key()];
    lock.insert_packages([first, second]).unwrap();
    lock.set_roots(keys.clone()).unwrap();
    let graph = lock.graph().unwrap();
    assert_eq!(graph.roots().len(), 2);
}

#[test]
fn shared_paths_preserve_transitive_aliases_and_stop_cycles() {
    let mut root = package("https://registry.npmjs.org", "root");
    let mut target = package("https://private.example", "target");
    let root_key = root.key();
    let target_key = target.key();
    root.add_alias_dependency("target-alias", &target_key)
        .unwrap();
    target.add_dependency("root", &root_key).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([root, target]).unwrap();
    lock.set_roots([root_key.clone()]).unwrap();
    let graph = lock.graph().unwrap();
    let paths = graph.paths_to(
        &[tapid_lockfile::LockedGraphRoot {
            key: root_key,
            name: "root".into(),
            kind: "dependencies".into(),
        }],
        &"target".parse().unwrap(),
    );
    assert!(!paths.truncated);
    assert_eq!(paths.paths.len(), 1);
    assert_eq!(paths.paths[0].steps.len(), 2);
    assert_eq!(paths.paths[0].steps[1].via.as_deref(), Some("target-alias"));
    assert_eq!(paths.paths[0].steps[1].key, target_key);
}

#[test]
fn projection_revalidates_deserialized_edges_instead_of_repairing_them() {
    let root = package("https://registry.npmjs.org", "root");
    let root_key = root.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(root).unwrap();
    lock.set_roots([root_key.clone()]).unwrap();
    let mut wire = serde_json::to_value(&lock).unwrap();
    wire["packages"][&root_key]["dependencies"] = serde_json::json!({
        "missing": package("https://registry.npmjs.org", "missing").key()
    });
    // Direct serde decoding deliberately bypasses the owning validated reader.
    let mutated: Lockfile = serde_json::from_value(wire).unwrap();
    assert!(matches!(
        mutated.graph(),
        Err(tapid_lockfile::LockfileError::DanglingDependency { .. })
    ));
}

#[test]
fn workspace_identity_and_its_registry_dependencies_remain_traversable() {
    use tapid_lockfile::{LocalWorkspaceSource, LockedGraphRoot, LockedWorkspacePackage};
    let dependency = package("https://private.example", "dependency");
    let target_key = dependency.key();
    let source = LocalWorkspaceSource::new("packages/member", "member", "1.0.0").unwrap();
    let mut member =
        LockedWorkspacePackage::new(source, &format!("sha256-{}", "b".repeat(64))).unwrap();
    member.add_dependency("dependency", &target_key).unwrap();
    let member_key = member.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_graph([dependency], [member]).unwrap();
    lock.set_roots([member_key.clone()]).unwrap();
    let graph = lock.graph().unwrap();
    assert_eq!(
        graph.nodes()[&member_key]
            .source
            .workspace()
            .unwrap()
            .path(),
        "packages/member"
    );
    let paths = graph.paths_to(
        &[LockedGraphRoot {
            key: member_key,
            name: "member".into(),
            kind: "workspace".into(),
        }],
        &"dependency".parse().unwrap(),
    );
    assert_eq!(paths.paths[0].steps[0].source_kind, "workspace");
    assert_eq!(paths.paths[0].steps[1].key, target_key);
    assert!(!paths.truncated);
}

#[test]
fn copied_sources_keep_their_local_identity_and_are_not_public_registry_packages() {
    let digest = format!("sha256-{}", "a".repeat(64));
    let original = package("https://registry.npmjs.org", "tool");
    let copied = LockedPackage::new_with_provenance(
        &format!("file:vendor/tool.tgz#{digest}"),
        "tool",
        "1.0.0",
        original.artifact_integrity(),
        &digest,
        RegistryIntegrityProvenance::LocallyComputed,
    )
    .unwrap();
    let key = copied.key();
    let mut lock = Lockfile::new(&digest).unwrap();
    lock.insert_package(copied).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let before = lock.to_json().unwrap();
    let graph = lock.graph().unwrap();
    assert!(graph.nodes()[&key].source.registry().is_none());
    assert!(graph.nodes()[&key].source.copied().is_some());
    let paths = graph.paths_to(
        &[tapid_lockfile::LockedGraphRoot {
            key,
            name: "tool".into(),
            kind: "dependencies".into(),
        }],
        &"tool".parse().unwrap(),
    );
    assert_eq!(paths.paths[0].steps[0].source_kind, "copied");
    assert_eq!(lock.to_json().unwrap(), before);
}

#[test]
fn rootless_legacy_graph_does_not_invent_recorded_entry_points() {
    let registry_package = package("https://registry.npmjs.org", "legacy");
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(registry_package).unwrap();
    let mut wire = serde_json::to_value(&lock).unwrap();
    wire["lockfileVersion"] = 4.into();
    let legacy = Lockfile::from_json(&wire.to_string()).unwrap();
    let graph = legacy.graph().unwrap();
    assert_eq!(graph.nodes().len(), 1);
    assert!(graph.roots().is_empty());
    let reachable = graph.reachable();
    assert!(!reachable.roots_recorded);
    assert!(reachable.occurrences.is_empty());
}

#[test]
fn workspace_entry_points_are_not_lost_when_registry_roots_have_bindings() {
    let registry = package("https://registry.npmjs.org", "registry");
    let registry_key = registry.key();
    let workspace = tapid_lockfile::LockedWorkspacePackage::new(
        tapid_lockfile::LocalWorkspaceSource::new("packages/member", "member", "1.0.0").unwrap(),
        &format!("sha256-{}", "b".repeat(64)),
    )
    .unwrap();
    let workspace_key = workspace.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_graph([registry], [workspace]).unwrap();
    lock.set_roots([registry_key.clone(), workspace_key.clone()])
        .unwrap();
    lock.set_root_bindings(BTreeMap::from([("registry".into(), registry_key)]))
        .unwrap();
    let graph = lock.graph().unwrap();
    assert_eq!(graph.roots().len(), 2);
    assert!(
        graph
            .roots()
            .iter()
            .any(|(name, key)| name == "member" && key == &workspace_key)
    );
}

#[test]
fn reachable_inventory_excludes_orphans_and_preserves_aliases_through_cycles() {
    let mut root = package("https://registry.npmjs.org", "root");
    let mut child = package("https://private.example", "child");
    let orphan = package("https://registry.npmjs.org", "orphan");
    let root_key = root.key();
    let child_key = child.key();
    root.add_alias_dependency("first", &child_key).unwrap();
    root.add_alias_dependency("second", &child_key).unwrap();
    child.add_dependency("root", &root_key).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([root, child, orphan]).unwrap();
    lock.set_roots([root_key.clone()]).unwrap();
    let before = lock.to_json().unwrap();
    let inventory = lock.graph().unwrap().reachable();
    assert!(inventory.roots_recorded);
    assert_eq!(inventory.occurrences.len(), 2);
    assert_eq!(
        inventory.occurrences[&child_key]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(inventory.unresolved_peers.is_empty());
    assert_eq!(lock.to_json().unwrap(), before);
}

#[test]
fn reachable_inventory_retains_peer_candidates_but_reports_ambiguous_applicability() {
    let peer = tapid_core::PeerContext::default()
        .with("provider".parse().unwrap(), "1.0.0".parse().unwrap());
    let platform = tapid_core::PlatformContext::new(None, None, None).unwrap();
    let consumer = LockedPackage::new_with_context_and_provenance(
        "https://registry.npmjs.org",
        "consumer",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        (&peer, &platform),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let consumer_key = consumer.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(consumer).unwrap();
    lock.set_roots([consumer_key.clone()]).unwrap();
    let missing = lock.graph().unwrap().reachable();
    assert!(
        missing
            .unresolved_peers
            .contains(&(consumer_key.clone(), "provider".into()))
    );
    lock.insert_package(package("https://registry.npmjs.org", "provider"))
        .unwrap();
    let unique = lock.graph().unwrap().reachable();
    assert!(unique.unresolved_peers.is_empty());
    assert_eq!(unique.occurrences.len(), 2);
    let other_platform = tapid_core::PlatformContext::new(Some("linux"), None, None).unwrap();
    let other = LockedPackage::new_with_context_and_provenance(
        "https://registry.npmjs.org",
        "provider",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "a".repeat(64)),
        (&tapid_core::PeerContext::default(), &other_platform),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    lock.insert_package(other).unwrap();
    let ambiguous = lock.graph().unwrap().reachable();
    assert_eq!(ambiguous.occurrences.len(), 3);
    assert!(
        ambiguous
            .unresolved_peers
            .contains(&(consumer_key, "provider".into()))
    );
}
