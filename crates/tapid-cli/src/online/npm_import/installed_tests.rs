use super::*;

#[test]
fn imported_observation_retains_nested_edges_and_requires_existing_receipts() {
    let home = tapid_test_support::TempHome::new("imported-observation").unwrap();
    let source = tapid_test_support::TempProject::new("imported-reference").unwrap();
    source
        .write("package.json", br#"{"name":"tool","version":"1.0.0"}"#)
        .unwrap();
    let store = Store::new(home.path().join("store"));
    let digest: ArtifactDigest = canonical_tree_digest(source.path())
        .unwrap()
        .parse()
        .unwrap();
    let mut tx = store.transaction();
    tx.stage_verified_tree(&digest, source.path()).unwrap();
    tx.publish().unwrap().commit().unwrap();
    let manifest = r#"{"name":"root","version":"1.0.0","dependencies":{"tool":"1.0.0"}}"#;
    let integrity = format!("sha512-{}", STANDARD.encode([0; 64]));
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":{"name":"root","version":"1.0.0","dependencies":{"tool":"1.0.0"}},
        "node_modules/tool":{"version":"1.0.0","resolved":"https://registry.npmjs.org/tool/-/tool-1.0.0.tgz","integrity":integrity,"dependencies":{"dep":"2.0.0"}},
        "node_modules/tool/node_modules/dep":{"version":"2.0.0","resolved":"https://registry.npmjs.org/dep/-/dep-2.0.0.tgz","integrity":integrity}
    }});
    let mut imported = ImportedNpmLockfile::import(
        &npm.to_string(),
        manifest,
        &format!("sha256-{}", "a".repeat(64)),
    )
    .unwrap();
    let guard = store.existing_read_guard().unwrap();
    let config = crate::registry::RegistryConfig::default();
    assert!(observe_imported_artifacts(&imported, &guard, &config).is_err());
    let graph = imported.graph().unwrap();
    imported
        .record_verified_trees(
            graph
                .packages
                .keys()
                .map(|path| (path.clone(), digest.to_string()))
                .collect(),
        )
        .unwrap();
    imported
        .record_verified_artifacts(
            graph
                .packages
                .iter()
                .map(|(path, p)| {
                    (
                        path.clone(),
                        tapid_lockfile::ImportedNpmArtifactReceipt {
                            tree_digest: digest.to_string(),
                            resolved: p.resolved.clone(),
                            integrity: p.integrity.to_string(),
                        },
                    )
                })
                .collect(),
        )
        .unwrap();
    let before = imported.to_json().unwrap();
    // One immutable tree cannot authenticate two different package identities.
    assert!(observe_imported_artifacts(&imported, &guard, &config).is_err());
    assert_eq!(imported.to_json().unwrap(), before);
    drop(guard);
    source
        .write("package.json", br#"{"name":"dep","version":"2.0.0"}"#)
        .unwrap();
    let dep_digest: ArtifactDigest = canonical_tree_digest(source.path())
        .unwrap()
        .parse()
        .unwrap();
    let mut tx = store.transaction();
    tx.stage_verified_tree(&dep_digest, source.path()).unwrap();
    tx.publish().unwrap().commit().unwrap();
    imported
        .record_verified_trees(BTreeMap::from([(
            "node_modules/tool/node_modules/dep".into(),
            dep_digest.to_string(),
        )]))
        .unwrap();
    imported
        .record_verified_artifacts(BTreeMap::from([(
            "node_modules/tool/node_modules/dep".into(),
            tapid_lockfile::ImportedNpmArtifactReceipt {
                tree_digest: dep_digest.to_string(),
                resolved: graph.packages["node_modules/tool/node_modules/dep"]
                    .resolved
                    .clone(),
                integrity: integrity.clone(),
            },
        )]))
        .unwrap();
    let before = imported.to_json().unwrap();
    let entries = || {
        fs::read_dir(store.root())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<BTreeSet<_>>()
    };
    let before_entries = entries();
    let guard = store.existing_read_guard().unwrap();
    let observed = observe_imported_artifacts(&imported, &guard, &config).unwrap();
    assert_eq!(
        observed
            .placements
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["node_modules/tool", "node_modules/tool/node_modules/dep"]
    );
    assert_eq!(observed.input.root_dependencies.len(), 1);
    assert_eq!(observed.input.root_dependencies[0].name.as_str(), "tool");
    assert_eq!(observed.input.dependency_edges.len(), 1);
    let edge = &observed.input.dependency_edges[0];
    assert_eq!(edge.parent, observed.placements["node_modules/tool"]);
    assert_eq!(edge.dependency.name.as_str(), "dep");
    assert_eq!(
        edge.dependency.child,
        observed.placements["node_modules/tool/node_modules/dep"]
    );
    assert_eq!(observed.lock.packages().len(), 2);
    let project = tapid_test_support::TempProject::new("imported-live-layout").unwrap();
    let mut plan = tapid_linker::plan_materialization(
        tapid_linker::ManagedRoot::new(project.path()).unwrap(),
        tapid_linker::MaterializationInput {
            instances: observed.input.instances.clone(),
        },
    )
    .unwrap();
    // Imported paths are authoritative even where native hoisting would differ.
    plan.activation.steps = observed
        .placements
        .iter()
        .map(|(path, key)| {
            let entry = plan
                .entries
                .iter()
                .find(|entry| {
                    entry.instance == key.id
                        && entry.peer_context == key.peer_context.to_string()
                        && entry.platform_context == key.platform_context.to_string()
                })
                .unwrap();
            tapid_linker::ActivationStep {
                kind: if cfg!(windows) {
                    tapid_linker::LinkKind::Junction
                } else {
                    tapid_linker::LinkKind::Symlink
                },
                source: entry.target.clone(),
                target: project.path().join(path),
            }
        })
        .collect();
    let trees = observed
        .input
        .instances
        .iter()
        .map(|instance| (instance.id.to_string(), instance.tree.root.clone()))
        .collect();
    crate::filesystem::tree::materialize_stage(
        project.path(),
        &plan,
        &observed.input.instances,
        &trees,
        false,
    )
    .unwrap();
    let verify = || {
        crate::filesystem::tree::verification::verify_materialized_layout(
            project.path(),
            project.path(),
            &plan,
            &observed.input.instances,
            &WorkspaceLinkPlan::default(),
        )
    };
    verify().unwrap();
    fs::rename(
        project.path().join("node_modules/tool/node_modules/dep"),
        project.path().join("node_modules/dep"),
    )
    .unwrap();
    assert!(verify().is_err());

    assert_eq!(imported.to_json().unwrap(), before);
    assert_eq!(entries(), before_entries);
    fs::remove_dir_all(guard.verified_tree_path(&dep_digest).unwrap()).unwrap();
    assert!(observe_imported_artifacts(&imported, &guard, &config).is_err());
    assert_eq!(entries(), before_entries);
}
