use super::*;
use tapid_test_support::{TempHome, TempProject};

#[test]
fn imported_alias_and_member_layouts_reject_relocation_and_changed_receipts() {
    let project = TempProject::new("exec-imported-members").unwrap();
    let source = TempProject::new("exec-imported-members-source").unwrap();
    let home = TempHome::new("exec-imported-members-home").unwrap();
    let root = serde_json::json!({"name":"root","version":"1.0.0","workspaces":["packages/*"],"dependencies":{"alias":"npm:actual@1.0.0","member":"^1"}}).to_string();
    let member =
        serde_json::json!({"name":"member","version":"1.0.0","dependencies":{"other":"2.0.0"}})
            .to_string();
    project.write("package.json", root.as_bytes()).unwrap();
    project
        .write("packages/member/package.json", member.as_bytes())
        .unwrap();
    let integrity = "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
    let npm = serde_json::json!({"lockfileVersion":3,"packages":{
        "":serde_json::from_str::<serde_json::Value>(&root).unwrap(),
        "packages/member":serde_json::from_str::<serde_json::Value>(&member).unwrap(),
        "node_modules/member":{"resolved":"packages/member","link":true},
        "node_modules/alias":{"name":"actual","version":"1.0.0","resolved":"https://registry.npmjs.org/actual/-/actual-1.0.0.tgz","integrity":integrity},
        "packages/member/node_modules/other":{"version":"2.0.0","resolved":"https://registry.npmjs.org/other/-/other-2.0.0.tgz","integrity":integrity}
    }});
    let mut imported = tapid_lockfile::ImportedNpmLockfile::import(
        &npm.to_string(),
        &root,
        &crate::filesystem::atomic::digest_bytes(root.as_bytes()),
    )
    .unwrap();
    let store = tapid_store::Store::new(home.path().join("store"));
    let mut digests = BTreeMap::new();
    for (path, name, version) in [
        ("node_modules/alias", "actual", "1.0.0"),
        ("packages/member/node_modules/other", "other", "2.0.0"),
    ] {
        source
            .write(
                "package.json",
                serde_json::json!({"name":name,"version":version,"bin":{"tool":"cli.js"}})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        source
            .write("cli.js", b"console.log('literal');\n")
            .unwrap();
        let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let mut tx = store.transaction();
        tx.stage_verified_tree(&digest.parse().unwrap(), source.path())
            .unwrap();
        tx.publish().unwrap().commit().unwrap();
        digests.insert(path.to_string(), digest);
    }
    imported.record_verified_trees(digests.clone()).unwrap();
    imported
        .record_verified_artifacts(
            imported
                .graph()
                .unwrap()
                .packages
                .iter()
                .map(|(path, package)| {
                    (
                        path.clone(),
                        tapid_lockfile::ImportedNpmArtifactReceipt {
                            tree_digest: digests[path].clone(),
                            resolved: package.resolved.clone(),
                            integrity: package.integrity.to_string(),
                        },
                    )
                })
                .collect(),
        )
        .unwrap();
    imported
        .bind_workspace_manifests(&BTreeMap::from([(
            "packages/member".into(),
            (
                member.clone(),
                crate::filesystem::atomic::digest_bytes(member.as_bytes()),
            ),
        )]))
        .unwrap();
    let bytes = imported.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    drop(ActivationLock::acquire(project.path()).unwrap());
    let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
    let observed =
        crate::online::observe_imported_artifacts(&imported, &evidence.store, &evidence.config)
            .unwrap();
    let local = crate::online::workspace_materialization_from_discovered(
        &evidence.root,
        &evidence.config,
        None,
        &evidence.workspace,
    )
    .unwrap();
    let (input, layouts) = crate::online::imported_layout(
        &evidence.root,
        local.links,
        &imported.graph().unwrap(),
        &observed.input,
        &observed.placements,
    )
    .unwrap();
    assert_eq!(input.root_dependencies[0].name.as_str(), "alias");
    assert!(
        layouts
            .members
            .contains_key(&evidence.root.join("packages/member"))
    );
    for (root, input, links) in std::iter::once((&evidence.root, &input, &layouts.links)).chain(
        layouts
            .members
            .iter()
            .map(|(root, layout)| (root, &layout.input, &layout.links)),
    ) {
        let plan = tapid_linker::plan_workspace_layout(
            tapid_linker::ManagedRoot::new(root).unwrap(),
            input.clone(),
            links,
            crate::application::replay::current_platform(),
        )
        .unwrap();
        let trees = input
            .instances
            .iter()
            .map(|instance| (instance.id.to_string(), instance.tree.root.clone()))
            .collect();
        let stage = root.join(".fixture-import-stage");
        crate::filesystem::tree::materialize_stage_with_workspace_links(
            &stage,
            &plan,
            &input.instances,
            &trees,
            false,
            links,
            &evidence.root,
            |_, _| {},
        )
        .unwrap();
        std::fs::rename(stage.join("node_modules"), root.join("node_modules")).unwrap();
        std::fs::remove_dir(stage).unwrap();
        std::fs::write(root.join(".tapid-managed"), b"tapid-managed-v1\n").unwrap();
    }
    evidence.verify_imported_layout().unwrap();
    assert!(
        !evidence
            .root
            .join("packages/member/.tapid-activation.lock")
            .exists()
    );
    std::fs::rename(
        evidence.root.join("packages/member/node_modules/other"),
        evidence.root.join("node_modules/other"),
    )
    .unwrap();
    assert!(evidence.verify_imported_layout().is_err());
    std::fs::rename(
        evidence.root.join("node_modules/other"),
        evidence.root.join("packages/member/node_modules/other"),
    )
    .unwrap();
    evidence.verify_imported_layout().unwrap();
    project
        .write("packages/member/node_modules/other/cli.js", b"modified")
        .unwrap();
    assert!(evidence.verify_imported_layout().is_err());
    assert_eq!(
        std::fs::read(evidence.root.join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
    drop(evidence);
    project
        .write(
            "packages/member/package.json",
            br#"{"name":"member","version":"2.0.0"}"#,
        )
        .unwrap();
    assert!(ProjectEvidence::observe(project.path(), &store).is_err());
}

#[test]
fn imported_project_observation_validates_bound_manifests_without_rewriting_lock() {
    let project = TempProject::new("exec-imported-project").unwrap();
    let home = TempHome::new("exec-imported-project-home").unwrap();
    let manifest = r#"{"name":"root","version":"1.0.0"}"#;
    project.write("package.json", manifest.as_bytes()).unwrap();
    let npm = r#"{"lockfileVersion":3,"packages":{"":{"name":"root","version":"1.0.0"}}}"#;
    let imported = tapid_lockfile::ImportedNpmLockfile::import(
        npm,
        manifest,
        &crate::filesystem::atomic::digest_bytes(manifest.as_bytes()),
    )
    .unwrap();
    let bytes = imported.to_json().unwrap();
    project.write("tapid.lock", bytes.as_bytes()).unwrap();
    let store = tapid_store::Store::new(home.path().join("store"));
    drop(store.read_guard().unwrap());
    drop(ActivationLock::acquire(project.path()).unwrap());
    let evidence = ProjectEvidence::observe(project.path(), &store).unwrap();
    evidence.revalidate().unwrap();
    assert!(evidence.verify_imported_layout().is_err());
    let input = tapid_linker::NamedLayoutInput {
        instances: vec![],
        root_dependencies: vec![],
        dependency_edges: vec![],
    };
    let plan = tapid_linker::plan_named_layout(
        tapid_linker::ManagedRoot::new(&evidence.root).unwrap(),
        input,
        crate::application::replay::current_platform(),
    )
    .unwrap();
    crate::filesystem::tree::materialize_stage(&evidence.root, &plan, &[], &BTreeMap::new(), false)
        .unwrap();
    project
        .write(".tapid-managed", b"tapid-managed-v1\n")
        .unwrap();
    evidence.verify_imported_layout().unwrap();
    assert!(evidence.verify_native_layout().is_err());
    project
        .write("node_modules/foreign.js", b"untracked")
        .unwrap();
    assert!(evidence.verify_imported_layout().is_err());
    assert_eq!(
        std::fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
    drop(evidence);
    project
        .write("package.json", br#"{"name":"changed","version":"1.0.0"}"#)
        .unwrap();
    assert!(ProjectEvidence::observe(project.path(), &store).is_err());
    assert_eq!(
        std::fs::read(project.path().join("tapid.lock")).unwrap(),
        bytes.as_bytes()
    );
}
