use tapid_lockfile::{GraphSnapshot, ImportedNpmArtifactReceipt, ImportedNpmLockfile};

fn imported(version: &str) -> ImportedNpmLockfile {
    let manifest = r#"{"dependencies":{"compat":"npm:actual@*"}}"#;
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":{"dependencies":{"compat":"npm:actual@*"}},
        "node_modules/compat":{"name":"actual","version":version,
            "resolved":format!("https://registry.npmjs.org/actual/-/actual-{version}.tgz"),
            "integrity":format!("sha512-{}==", "A".repeat(86))}
    }});
    ImportedNpmLockfile::import(
        &npm.to_string(),
        manifest,
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap()
}

#[test]
fn imported_receipts_do_not_change_selections() {
    let mut lock = imported("1.0.0");
    let before = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let tree = format!("sha256-{}", "b".repeat(64));
    lock.record_verified_trees([("node_modules/compat".into(), tree.clone())].into())
        .unwrap();
    let graph = lock.graph().unwrap();
    let package = &graph.packages["node_modules/compat"];
    lock.record_verified_artifacts(
        [(
            "node_modules/compat".into(),
            ImportedNpmArtifactReceipt {
                tree_digest: tree,
                resolved: package.resolved.clone(),
                integrity: package.integrity.to_string(),
            },
        )]
        .into(),
    )
    .unwrap();
    let after = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert!(changes.added_instances.is_empty());
    assert!(changes.removed_instances.is_empty());
    assert_eq!(changes.changed_selections, 0);
    assert!(changes.relationship_changes.is_empty());
    assert_eq!(changes.evidence_changes.len(), 1);
    assert_eq!(
        changes.evidence_changes[0].dimension,
        "verification_receipts"
    );
    assert!(changes.before_availability.verification_receipts);
    let inspection = before.inspect();
    assert_eq!(inspection.instances[0].name, "actual");
    assert_eq!(
        inspection
            .relationships
            .iter()
            .find(|e| e.kind == "imported_entry")
            .unwrap()
            .local_name
            .as_deref(),
        Some("compat")
    );
}

#[test]
fn imported_placement_retargets_and_cross_format_is_explicitly_unsupported() {
    let before =
        GraphSnapshot::from_bytes(imported("1.0.0").to_json().unwrap().as_bytes()).unwrap();
    let after = GraphSnapshot::from_bytes(imported("2.0.0").to_json().unwrap().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert!(changes.selected_graph_supported);
    assert_eq!(changes.changed_selections, 2); // placement and root binding, overlapping views
    assert_eq!(changes.added_instances.len(), 1);
    assert_eq!(changes.removed_instances.len(), 1);
    let native = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "c".repeat(64))).unwrap();
    let native = GraphSnapshot::from_bytes(native.to_json().unwrap().as_bytes()).unwrap();
    for changes in [before.compare(&native), native.compare(&before)] {
        assert!(!changes.selected_graph_supported);
        assert_eq!(
            changes.unsupported_reason,
            Some("cross_format_selection_equivalence")
        );
        assert!(changes.added_instances.is_empty());
        assert_eq!(changes.evidence_changes.len(), 1);
        assert_eq!(changes.evidence_changes[0].dimension, "declarations");
    }
}

#[test]
fn imported_fixture_preserves_peers_duplicates_and_all_platforms() {
    let lock = ImportedNpmLockfile::import(
        include_str!("fixtures/npm-import-package-lock.json"),
        include_str!("fixtures/npm-import-package.json"),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let snapshot = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let graph = snapshot.inspect();
    assert_eq!(graph.instances.len(), 5);
    assert_eq!(
        graph
            .instances
            .iter()
            .filter(|p| p.name == "shared")
            .count(),
        2
    );
    assert!(graph.instances.iter().any(|p| p.name == "native"));
    let peer = graph
        .relationships
        .iter()
        .find(|e| e.kind == "peer")
        .unwrap();
    assert_eq!(peer.local_name.as_deref(), Some("shared"));
    let provider = graph
        .instances
        .iter()
        .find(|p| p.reference == peer.target)
        .unwrap();
    assert_eq!(provider.placement.as_deref(), Some("node_modules/shared"));
    assert!(snapshot.compare(&snapshot).relationship_changes.is_empty());
}

#[test]
fn imported_workspace_declaration_evidence_and_structural_bounds() {
    let root = serde_json::json!({"name":"root","workspaces":["packages/*"],"dependencies":{"member":"*"}});
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":root,"packages/member":{"name":"member","version":"1.0.0"},
        "node_modules/member":{"resolved":"packages/member","link":true}
    }});
    let mut lock = ImportedNpmLockfile::import(
        &npm.to_string(),
        &root.to_string(),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let before = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    let member = r#"{"name":"member","version":"1.0.0"}"#;
    lock.bind_workspace_manifests(
        &[(
            "packages/member".into(),
            (member.into(), format!("sha256-{}", "b".repeat(64))),
        )]
        .into(),
    )
    .unwrap();
    let after = GraphSnapshot::from_bytes(lock.to_json().unwrap().as_bytes()).unwrap();
    assert_eq!(before.inspect().instances.len(), 1);
    assert!(
        before
            .inspect()
            .relationships
            .iter()
            .any(|e| e.kind == "workspace_link")
    );
    let changes = before.compare(&after);
    assert_eq!(changes.changed_selections, 0);
    assert_eq!(changes.evidence_changes.len(), 1);
    assert_eq!(changes.evidence_changes[0].dimension, "declarations");
    let packages = (0..100_001)
        .map(|i| format!("\"p{i}\":null"))
        .collect::<Vec<_>>()
        .join(",");
    let hostile = format!("{{\"lockfileVersion\":8,\"npmLock\":{{\"packages\":{{{packages}}}}}}}");
    assert_eq!(
        GraphSnapshot::from_bytes(hostile.as_bytes()).unwrap_err(),
        tapid_lockfile::SnapshotError::AnalysisLimit
    );
}

#[test]
fn imported_artifact_changes_are_evidence_and_declarations_retain_requirements() {
    let lock = imported("1.0.0");
    let bytes = lock.to_json().unwrap();
    let before = GraphSnapshot::from_bytes(bytes.as_bytes()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    value["npmLock"]["packages"]["node_modules/compat"]["resolved"] =
        "https://registry.npmjs.org/mirror/actual/-/actual-1.0.0.tgz".into();
    value["npmLock"]["packages"][""]["dependencies"]["compat"] = "npm:actual@^1".into();
    let after = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert!(changes.added_instances.is_empty());
    assert_eq!(changes.changed_selections, 0);
    assert_eq!(
        changes
            .evidence_changes
            .iter()
            .map(|e| e.dimension.as_str())
            .collect::<Vec<_>>(),
        ["declarations", "source_artifact"]
    );
    assert!(changes.before_availability.declaration_requirements);
}

#[test]
fn availability_explains_missing_dimensions_without_claiming_equality() {
    let imported = GraphSnapshot::from_bytes(imported("1.0.0").to_json().unwrap().as_bytes())
        .unwrap()
        .inspect();
    assert_eq!(
        imported.availability.unavailable_reasons()["exact_scripts"],
        "not_recorded"
    );
    assert_eq!(
        imported.availability.unavailable_reasons()["host_toolchain"],
        "not_recorded"
    );
    assert!(
        !imported
            .availability
            .unavailable_reasons()
            .contains_key("verification_receipts")
    );
    let native = tapid_lockfile::Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    let native = GraphSnapshot::from_bytes(native.to_json().unwrap().as_bytes())
        .unwrap()
        .inspect();
    assert_eq!(
        native.availability.unavailable_reasons()["declaration_requirements"],
        "digest_evidence_only"
    );
    assert_eq!(
        native.availability.unavailable_reasons()["peer_provider_targets"],
        "context_does_not_identify_provider_placement"
    );
}

#[test]
fn imported_dependency_requirement_change_does_not_change_selected_edges() {
    let lock = ImportedNpmLockfile::import(
        include_str!("fixtures/npm-import-package-lock.json"),
        include_str!("fixtures/npm-import-package.json"),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let bytes = lock.to_json().unwrap();
    let before = GraphSnapshot::from_bytes(bytes.as_bytes()).unwrap();
    let mut changed: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    changed["npmLock"]["packages"]["node_modules/parent"]["dependencies"]["shared"] =
        "^2.0.0".into();
    let after = GraphSnapshot::from_bytes(changed.to_string().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert_eq!(changes.changed_selections, 0);
    assert!(changes.relationship_changes.is_empty());
    assert_eq!(changes.evidence_changes.len(), 1);
    assert_eq!(changes.evidence_changes[0].dimension, "declarations");
    assert_eq!(
        changes.evidence_changes[0].after["dependencies"],
        r#"{"shared":"^2.0.0"}"#
    );
}

#[test]
fn imported_peer_provider_versions_are_part_of_exact_consumer_context() {
    let lock = ImportedNpmLockfile::import(
        include_str!("fixtures/npm-import-package-lock.json"),
        include_str!("fixtures/npm-import-package.json"),
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    value["npmLock"]["packages"][""]["dependencies"]["shared"] = "*".into();
    value["npmLock"]["packages"]["node_modules/consumer"]["peerDependencies"]["shared"] =
        "*".into();
    let before = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    value["npmLock"]["packages"]["node_modules/shared"]["version"] = "1.1.0".into();
    value["npmLock"]["packages"]["node_modules/shared"]["resolved"] =
        "https://registry.npmjs.org/shared/-/shared-1.1.0.tgz".into();
    let after = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert!(changes.added_instances.iter().any(|p| p.name == "consumer"));
    assert!(
        changes
            .removed_instances
            .iter()
            .any(|p| p.name == "consumer")
    );
}

#[test]
fn imported_constraint_set_order_does_not_invent_selection_changes() {
    let lock = imported("1.0.0");
    let mut value: serde_json::Value = serde_json::from_str(&lock.to_json().unwrap()).unwrap();
    value["npmLock"]["packages"]["node_modules/compat"]["os"] =
        serde_json::json!(["linux", "darwin"]);
    let before = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    value["npmLock"]["packages"]["node_modules/compat"]["os"] =
        serde_json::json!(["darwin", "linux"]);
    let after = GraphSnapshot::from_bytes(value.to_string().as_bytes()).unwrap();
    let changes = before.compare(&after);
    assert!(changes.added_instances.is_empty());
    assert!(changes.removed_instances.is_empty());
    assert_eq!(changes.changed_selections, 0);
}
