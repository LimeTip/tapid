use std::collections::BTreeMap;
use tapid_lockfile::{LockedPackage, Lockfile, RegistryIntegrityProvenance};

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
