use tapid_lockfile::{GraphSnapshot, LockedPackage, Lockfile, RegistryIntegrityProvenance};

fn package(name: &str, version: &str) -> LockedPackage {
    LockedPackage::new_with_provenance(
        "https://registry.example.test",
        name,
        version,
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "b".repeat(64)),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap()
}

#[test]
fn inspection_preserves_aliases_and_disconnected_instances() {
    let child = package("actual", "1.0.0");
    let disconnected = package("actual", "2.0.0");
    let mut parent = package("parent", "1.0.0");
    parent.add_alias_dependency("compat", &child.key()).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([parent.clone(), child, disconnected])
        .unwrap();
    lock.set_roots([parent.key()]).unwrap();
    lock.set_root_bindings([("entry".into(), parent.key())].into())
        .unwrap();
    let snapshot = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let graph = snapshot.inspect();
    assert_eq!(graph.schema, 7);
    assert_eq!(graph.instances.len(), 3);
    assert_eq!(graph.relationships.len(), 3);
    let edge = graph
        .relationships
        .iter()
        .find(|edge| edge.local_name.as_deref() == Some("compat"))
        .unwrap();
    let target = graph
        .instances
        .iter()
        .find(|instance| instance.reference == edge.target)
        .unwrap();
    assert_eq!(target.name, "actual");
    assert_eq!(target.version, "1.0.0");
    assert_eq!(
        target.identity,
        "https://registry.example.test|actual@1.0.0|peer=-|platform=os=;cpu=;libc="
    );
    assert_eq!(
        graph
            .instances
            .iter()
            .filter(|instance| instance.name == "actual")
            .count(),
        2
    );
}

#[test]
fn inspection_reports_legacy_gaps_and_rejects_untrusted_inputs() {
    let lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    value["lockfileVersion"] = 4.into();
    let snapshot = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    let graph = snapshot.inspect();
    assert!(!graph.availability.explicit_roots);
    assert!(!graph.availability.named_root_bindings);
    assert!(!graph.availability.exact_scripts);
    assert!(!graph.availability.host_toolchain);
    assert_eq!(graph.coverage.total_instances, 0);
    assert!(graph.coverage.analysis_complete);
    assert_eq!(
        GraphSnapshot::from_bytes(b"{bad").unwrap_err(),
        tapid_lockfile::SnapshotError::InvalidSnapshot
    );
    assert_eq!(
        GraphSnapshot::from_bytes(b"{\"lockfileVersion\":99}").unwrap_err(),
        tapid_lockfile::SnapshotError::UnsupportedFormat
    );
    assert_eq!(
        GraphSnapshot::from_bytes(&vec![b' '; 32 * 1024 * 1024 + 1]).unwrap_err(),
        tapid_lockfile::SnapshotError::InputLimit
    );
}

#[test]
fn comparison_retargets_recorded_sites_without_pairing_names() {
    let first = package("actual", "1.0.0");
    let second = package("actual", "2.0.0");
    let mut parent = package("parent", "1.0.0");
    parent.add_alias_dependency("compat", &first.key()).unwrap();
    let mut before = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    before
        .insert_packages([parent.clone(), first.clone(), second.clone()])
        .unwrap();
    before.set_roots([parent.key(), first.key()]).unwrap();
    before
        .set_root_bindings(
            [
                ("entry".into(), parent.key()),
                ("direct".into(), first.key()),
            ]
            .into(),
        )
        .unwrap();
    let mut after: serde_json::Value = serde_json::from_str(&before.to_json().unwrap()).unwrap();
    after["packages"][parent.key()]["dependencies"]["compat"] = second.key().into();
    after["rootBindings"]["direct"] = second.key().into();
    let mut roots = vec![parent.key(), second.key()];
    roots.sort();
    after["roots"] = serde_json::json!(roots);
    let left = GraphSnapshot::from_bytes(before.to_json().unwrap().as_bytes()).unwrap();
    let right = GraphSnapshot::from_bytes(after.to_string().as_bytes()).unwrap();
    let changes = left.compare(&right);
    assert!(changes.added_instances.is_empty());
    assert!(changes.removed_instances.is_empty());
    assert_eq!(changes.changed_selections, 2);
    assert_eq!(changes.relationship_changes.len(), 4); // two retargets + unnamed root set delta
    assert!(changes.evidence_changes.is_empty());
}

#[test]
fn comparison_separates_declaration_artifact_and_derived_evidence() {
    let selected = package("actual", "1.0.0");
    let key = selected.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(selected).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let mut before: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    before["lockfileVersion"] = 9.into();
    before["packages"][&key]["derivedHooks"] = serde_json::json!([{
        "attestation": format!("hmac-sha256-{}", "e".repeat(64)),
        "hook": "postinstall", "key": format!("sha256-{}", "c".repeat(64)),
        "treeDigest": format!("sha256-{}", "d".repeat(64))
    }]);
    let mut after = before.clone();
    after["rootManifestDigest"] = format!("sha256-{}", "f".repeat(64)).into();
    after["packages"][&key]["treeDigest"] = format!("sha256-{}", "f".repeat(64)).into();
    after["packages"][&key]["derivedHooks"][0]["attestation"] =
        format!("hmac-sha256-{}", "f".repeat(64)).into();
    let left = GraphSnapshot::from_bytes(before.to_string().as_bytes()).unwrap();
    let right = GraphSnapshot::from_bytes(after.to_string().as_bytes()).unwrap();
    let changes = left.compare(&right);
    assert!(changes.added_instances.is_empty());
    assert!(changes.removed_instances.is_empty());
    assert_eq!(changes.changed_selections, 0);
    assert!(changes.relationship_changes.is_empty());
    assert_eq!(
        changes
            .evidence_changes
            .iter()
            .map(|c| c.dimension.as_str())
            .collect::<Vec<_>>(),
        ["declarations", "source_artifact", "derived_outputs"]
    );
    assert!(changes.evidence_changes[2].attestation_changed);
    assert!(!format!("{changes:?}").contains("hmac-sha256-"));
}

#[test]
fn structural_limits_are_enforced_before_normalization() {
    use tapid_lockfile::SnapshotError;
    let packages = (0..100_001)
        .map(|i| format!("\"p{i}\":null"))
        .collect::<Vec<_>>()
        .join(",");
    let input = format!("{{\"lockfileVersion\":7,\"packages\":{{{packages}}}}}");
    assert_eq!(
        GraphSnapshot::from_bytes(input.as_bytes()).unwrap_err(),
        SnapshotError::AnalysisLimit
    );
    let roots = "null,".repeat(500_000) + "null";
    let input = format!("{{\"lockfileVersion\":7,\"roots\":[{roots}]}}");
    assert_eq!(
        GraphSnapshot::from_bytes(input.as_bytes()).unwrap_err(),
        SnapshotError::AnalysisLimit
    );
    let nested = format!(
        "{{\"lockfileVersion\":7,\"extra\":{}0{}}}",
        "[".repeat(129),
        "]".repeat(129)
    );
    assert_eq!(
        GraphSnapshot::from_bytes(nested.as_bytes()).unwrap_err(),
        SnapshotError::InvalidSnapshot
    );
}

#[test]
fn inspection_filters_include_incident_edges_without_expanding_scope() {
    use tapid_lockfile::GraphScope;
    let child = package("actual", "1.0.0");
    let mut parent = package("parent", "1.0.0");
    parent.add_alias_dependency("compat", &child.key()).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([parent.clone(), child]).unwrap();
    lock.set_roots([parent.key()]).unwrap();
    let snapshot = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let selected = snapshot
        .inspect_scope(&GraphScope::Packages(vec![
            "actual".into(),
            "actual".into(),
        ]))
        .unwrap();
    assert_eq!(selected.instances.len(), 1);
    assert_eq!(selected.relationships.len(), 1);
    assert_eq!(selected.coverage.total_instances, 2);
    assert_eq!(selected.coverage.excluded_instances, 1);
    assert_eq!(selected.coverage.excluded_relationships, 1);
    assert_eq!(selected.scope, GraphScope::Packages(vec!["actual".into()]));
    let empty = snapshot
        .inspect_scope(&GraphScope::Instances(vec!["unknown".into()]))
        .unwrap();
    assert!(empty.instances.is_empty());
    assert!(empty.relationships.is_empty());
    assert!(
        snapshot
            .inspect_scope(&GraphScope::Packages(vec!["a".into(); 65]))
            .is_err()
    );
    assert!(
        snapshot
            .inspect_scope(&GraphScope::Packages(vec!["a".repeat(4097)]))
            .is_err()
    );
}

#[test]
fn relationship_order_uses_categories_then_full_identity() {
    let child = package("actual", "1.0.0");
    let mut parent = package("parent", "1.0.0");
    parent.add_alias_dependency("compat", &child.key()).unwrap();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([parent.clone(), child.clone()])
        .unwrap();
    lock.set_roots([parent.key(), child.key()]).unwrap();
    lock.set_root_bindings(
        [
            ("entry".into(), parent.key()),
            ("compat".into(), child.key()),
        ]
        .into(),
    )
    .unwrap();
    let snapshot = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let graph = snapshot.inspect();
    assert_eq!(
        graph
            .relationships
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        ["root", "root", "root_binding", "root_binding", "dependency"]
    );
    assert_eq!(graph.relationships[0].target, graph.instances[0].reference);
    assert_eq!(graph.relationships[1].target, graph.instances[1].reference);
}

#[test]
fn source_context_and_workspace_cycles_keep_distinct_identities() {
    use tapid_lockfile::{LocalWorkspaceSource, LockedWorkspacePackage};
    let digest = format!("sha256-{}", "b".repeat(64));
    let integrity = format!("sha512-{}==", "A".repeat(86));
    let peer = tapid_core::PeerContext::default()
        .with("react".parse().unwrap(), "18.2.0".parse().unwrap());
    let platform =
        tapid_core::PlatformContext::new(Some("linux"), Some("x86_64"), Some("gnu")).unwrap();
    let contextual = LockedPackage::new_with_context_and_provenance(
        "https://registry.example.test",
        "actual",
        "1.0.0",
        &integrity,
        &digest,
        (&peer, &platform),
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let mut packages = vec![package("actual", "1.0.0"), contextual];
    for source in [
        "https://other.example.test".into(),
        format!("file:vendor/tool.tgz#{digest}"),
        format!(
            "git+https://example.test/repo.git#{}&archive={digest}",
            "c".repeat(40)
        ),
    ] {
        // Construct Git through the existing source interface to retain its wire grammar.
        let source = if source.starts_with("git+") {
            tapid_core::PackageSource::git(
                "https://example.test/repo.git",
                &"c".repeat(40),
                digest.parse().unwrap(),
            )
            .unwrap()
            .to_string()
        } else {
            source
        };
        packages.push(
            LockedPackage::new_with_provenance(
                &source,
                "actual",
                "1.0.0",
                &integrity,
                &digest,
                RegistryIntegrityProvenance::RegistryDeclared,
            )
            .unwrap(),
        );
    }
    let mut first = LockedWorkspacePackage::new(
        LocalWorkspaceSource::new("packages/first", "first", "1.0.0").unwrap(),
        &digest,
    )
    .unwrap();
    let mut second = LockedWorkspacePackage::new(
        LocalWorkspaceSource::new("packages/second", "second", "1.0.0").unwrap(),
        &digest,
    )
    .unwrap();
    first.add_dependency("second", &second.key()).unwrap();
    second.add_dependency("first", &first.key()).unwrap();
    let mut lock = Lockfile::new(&digest).unwrap();
    lock.insert_graph(packages, [first.clone(), second])
        .unwrap();
    lock.set_roots([first.key()]).unwrap();
    let bytes = lock.to_json().unwrap();
    let snapshot = GraphSnapshot::from_bytes(bytes.as_bytes()).unwrap();
    let graph = snapshot.inspect();
    assert_eq!(graph.schema, 10);
    assert_eq!(graph.instances.len(), 7);
    assert_eq!(graph.relationships.len(), 3);
    assert_eq!(
        graph
            .instances
            .iter()
            .map(|p| &p.reference)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        7
    );
    assert!(graph.instances.iter().any(|p| p.identity == "https://registry.example.test|actual@1.0.0|peer=name=react;version=18.2.0|platform=os=linux;cpu=x86_64;libc=gnu"));
    let comparison = snapshot.compare(&snapshot);
    assert_eq!(comparison.changed_selections, 0);
    assert!(comparison.relationship_changes.is_empty());
}

#[test]
fn comparison_scope_keeps_retargets_touching_only_the_before_side() {
    use tapid_lockfile::GraphScope;
    let first = package("actual", "1.0.0");
    let second = package("actual", "2.0.0");
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_packages([first.clone(), second.clone()])
        .unwrap();
    lock.set_roots([first.key()]).unwrap();
    lock.set_root_bindings([("compat".into(), first.key())].into())
        .unwrap();
    let before = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    lock.set_roots([second.key()]).unwrap();
    lock.set_root_bindings([("compat".into(), second.key())].into())
        .unwrap();
    let after = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let selected = before
        .inspect()
        .instances
        .into_iter()
        .find(|p| p.version == "1.0.0")
        .unwrap()
        .reference;
    let comparison = before
        .compare_scope(&after, &GraphScope::Instances(vec![selected]))
        .unwrap();
    assert_eq!(comparison.changed_selections, 1);
    assert_eq!(comparison.relationship_changes.len(), 2); // old root removal + full binding retarget
    let retarget = comparison
        .relationship_changes
        .iter()
        .find(|c| c.before.as_ref().is_some_and(|e| e.kind == "root_binding"))
        .unwrap();
    assert!(retarget.after.is_some());
    assert_eq!(comparison.before_coverage.excluded_instances, 1);
    assert_eq!(comparison.after_coverage.excluded_instances, 1);
}

#[test]
fn replaced_native_parents_do_not_pair_their_children_by_name() {
    fn lock(parent_version: &str, child_version: &str) -> Lockfile {
        let child = package("actual", child_version);
        let mut parent = package("parent", parent_version);
        parent.add_alias_dependency("compat", &child.key()).unwrap();
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        lock.insert_packages([parent.clone(), child]).unwrap();
        lock.set_roots([parent.key()]).unwrap();
        lock.set_root_bindings([("entry".into(), parent.key())].into())
            .unwrap();
        lock
    }
    let before =
        GraphSnapshot::from_bytes(lock("1.0.0", "1.0.0").to_json().unwrap().as_bytes()).unwrap();
    let after =
        GraphSnapshot::from_bytes(lock("2.0.0", "2.0.0").to_json().unwrap().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert_eq!(changes.added_instances.len(), 2);
    assert_eq!(changes.removed_instances.len(), 2);
    let dependency_changes: Vec<_> = changes
        .relationship_changes
        .iter()
        .filter(|c| c.before.as_ref().or(c.after.as_ref()).unwrap().kind == "dependency")
        .collect();
    assert_eq!(dependency_changes.len(), 2);
    assert!(
        dependency_changes
            .iter()
            .all(|c| c.before.is_none() || c.after.is_none())
    );
    let binding_changes: Vec<_> = changes
        .relationship_changes
        .iter()
        .filter(|c| c.before.as_ref().or(c.after.as_ref()).unwrap().kind == "root_binding")
        .collect();
    assert_eq!(binding_changes.len(), 1);
    assert!(binding_changes[0].before.is_some() && binding_changes[0].after.is_some());
}

#[test]
fn newly_recorded_binding_metadata_does_not_establish_a_selection_change() {
    let selected = package("actual", "1.0.0");
    let key = selected.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(selected).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let before = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    lock.set_root_bindings([("compat".into(), key)].into())
        .unwrap();
    let after = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let comparison = before.compare(&after);
    assert_eq!(comparison.changed_selections, 0);
    assert!(comparison.relationship_changes.is_empty());
    assert!(!comparison.before_availability.named_root_bindings);
    assert!(comparison.after_availability.named_root_bindings);
}

#[test]
fn legacy_unpacked_evidence_changes_are_not_silently_equal() {
    let selected = package("actual", "1.0.0");
    let key = selected.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(selected).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let before = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    value["packages"][&key]["unpackedDigest"] = format!("sha256-{}", "c".repeat(64)).into();
    let after = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    let comparison = before.compare(&after);
    assert_eq!(comparison.evidence_changes.len(), 1);
    assert_eq!(comparison.evidence_changes[0].dimension, "source_artifact");
    assert_eq!(comparison.changed_selections, 0);
}

#[test]
fn snapshot_digests_bind_bytes_without_changing_semantic_results() {
    let lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    let bytes = lock.to_json().unwrap();
    let before = GraphSnapshot::from_bytes(bytes.as_bytes()).unwrap();
    let after = GraphSnapshot::from_bytes(format!("{bytes}\n").as_bytes()).unwrap();
    let comparison = before.compare(&after);
    assert_ne!(comparison.before_digest, comparison.after_digest);
    assert_eq!(comparison.before_digest, before.inspect().snapshot_digest);
    assert_eq!(comparison.after_digest, after.inspect().snapshot_digest);
    assert!(comparison.added_instances.is_empty());
    assert!(comparison.removed_instances.is_empty());
    assert!(comparison.relationship_changes.is_empty());
    assert!(comparison.evidence_changes.is_empty());
}

#[test]
fn inclusive_limits_and_typed_graph_validation_remain_authoritative() {
    use tapid_lockfile::{GraphScope, SnapshotError};
    let empty = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    let mut bytes = empty.to_json().unwrap().into_bytes();
    bytes.resize(32 * 1024 * 1024, b' ');
    let snapshot = GraphSnapshot::from_bytes(&bytes).unwrap();
    assert!(
        snapshot
            .inspect_scope(&GraphScope::Packages(vec!["a".repeat(4096); 64]))
            .is_ok()
    );
    let packages = (0..100_000)
        .map(|i| format!("\"p{i}\":null"))
        .collect::<Vec<_>>()
        .join(",");
    let input = format!("{{\"lockfileVersion\":7,\"packages\":{{{packages}}}}}");
    assert_eq!(
        GraphSnapshot::from_bytes(input.as_bytes()).unwrap_err(),
        SnapshotError::InvalidSnapshot
    );
    let roots = "null,".repeat(499_999) + "null";
    let input = format!("{{\"lockfileVersion\":7,\"roots\":[{roots}]}}");
    assert_eq!(
        GraphSnapshot::from_bytes(input.as_bytes()).unwrap_err(),
        SnapshotError::InvalidSnapshot
    );
    let selected = package("actual", "1.0.0");
    let key = selected.key();
    let mut lock = empty;
    lock.insert_package(selected).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let mut malformed: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    malformed["packages"][&key]["dependencies"]["missing"] =
        package("missing", "1.0.0").key().into();
    assert_eq!(
        GraphSnapshot::from_bytes(malformed.to_string().as_bytes()).unwrap_err(),
        SnapshotError::InvalidSnapshot
    );
}

#[test]
fn validated_snapshot_debug_does_not_expose_store_attestations() {
    let selected = package("actual", "1.0.0");
    let key = selected.key();
    let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    lock.insert_package(selected).unwrap();
    lock.set_roots([key.clone()]).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    value["lockfileVersion"] = 9.into();
    value["packages"][&key]["derivedHooks"] = serde_json::json!([{
        "attestation": format!("hmac-sha256-{}", "e".repeat(64)),
        "hook": "postinstall", "key": format!("sha256-{}", "c".repeat(64)),
        "treeDigest": format!("sha256-{}", "d".repeat(64))
    }]);
    let snapshot = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    assert!(!format!("{snapshot:?}").contains("hmac-sha256-"));
}
