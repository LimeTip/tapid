use super::outcome::{ErrorKind, OperationalError};
use super::replay::replay_input_with_tree_source;
use std::{collections::BTreeMap, path::PathBuf};
use tapid_core::PackageInstanceId;
use tapid_linker::{InstanceKey, NamedLayoutInput, VerifiedTreeReference};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;

mod live;
mod project;
mod selection;
#[cfg(test)]
use live::bind_live_command;

/// Reconstruct locked instances from existing authenticated store evidence.
/// The caller must retain `guard` while consuming these paths and separately
/// validate project observations, workspace topology and the materialized layout.
/// This is evidence preparation, never launch authorization or lifecycle approval.
#[allow(dead_code)] // Installed-command composition is not yet exposed by the CLI.
pub(crate) fn installed_input(
    lock: &Lockfile,
    manifest: &PackageManifest,
    guard: &tapid_store::StoreReadGuard,
    registry_config: &crate::registry::RegistryConfig,
) -> Result<(NamedLayoutInput, BTreeMap<String, PathBuf>), OperationalError> {
    let mut final_trees = BTreeMap::new();
    let (mut input, mut trees) = replay_input_with_tree_source(
        lock,
        manifest,
        registry_config,
        |_, _| {},
        |key, package| {
            let digest = |value: &str| {
                value
                    .parse::<tapid_core::ArtifactDigest>()
                    .map_err(|error| OperationalError::from_source(ErrorKind::Lockfile, error))
            };
            let source = guard.verified_tree_path(&digest(package.tree_digest())?)?;
            if let Some(output) = package.derived_hooks().last() {
                let path = guard.verified_lifecycle_tree_path(
                    &digest(output.key())?,
                    &digest(output.tree_digest())?,
                    output.attestation(),
                )?;
                final_trees.insert(
                    InstanceKey {
                        id: PackageInstanceId::from_source(
                            key.source
                                .package_source()
                                .ok_or("workspace artifact in installed input")?,
                            key.name.clone(),
                            key.version.clone(),
                        ),
                        peer_context: key.peer_context()?,
                        platform_context: key.platform_context()?,
                    },
                    (
                        key.to_string(),
                        VerifiedTreeReference::new(output.tree_digest(), path)
                            .map_err(|error| error.to_string())?,
                    ),
                );
            }
            Ok(source)
        },
    )?;
    for instance in &mut input.instances {
        if let Some((key, tree)) = final_trees.remove(&InstanceKey::from(&*instance)) {
            trees.insert(key, tree.root.clone());
            instance.tree = tree;
        }
    }
    Ok((input, trees))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_store::Store;

    #[test]
    fn installed_copied_input_uses_locked_tree_and_fails_on_missing_or_changed_evidence() {
        use tapid_test_support::{TempHome, TempProject};
        let home = TempHome::new("installed-copied-input").unwrap();
        let source = TempProject::new("installed-copied-source").unwrap();
        source
            .write(
                "package.json",
                br#"{"name":"tool","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        source.write("cli.js", b"console.log('locked');\n").unwrap();
        let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let store = Store::new(home.path().join("store"));
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&digest.parse().unwrap(), source.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            &format!("file:vendor/tool.tgz#{digest}"), "tool", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", &digest,
            tapid_lockfile::RegistryIntegrityProvenance::LocallyComputed,
        ).unwrap();
        let key = package.key();
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
        lock.set_root_bindings(BTreeMap::from([("local".into(), key.clone())]))
            .unwrap();
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"local":"file:vendor/tool.tgz"}}"#,
        )
        .unwrap();
        source
            .write("cli.js", b"console.log('mutable source');\n")
            .unwrap();
        let guard = store.existing_read_guard().unwrap();
        let inspect = || {
            installed_input(
                &lock,
                &manifest,
                &guard,
                &crate::registry::RegistryConfig::default(),
            )
        };
        let (input, trees) = inspect().unwrap();
        let project = TempProject::new("installed-copied-live").unwrap();
        let plan = tapid_linker::plan_named_layout(
            tapid_linker::ManagedRoot::new(project.path()).unwrap(),
            input.clone(),
            crate::application::replay::current_platform(),
        )
        .unwrap();
        crate::filesystem::tree::materialize_stage(
            project.path(),
            &plan,
            &input.instances,
            &trees,
            false,
        )
        .unwrap();
        let select_live = || {
            bind_live_command(
                project.path(),
                project.path(),
                &input,
                &plan,
                &tapid_linker::WorkspaceLinkPlan::default(),
                selection::select_manifest_command(
                    &input,
                    &manifest,
                    Some("local"),
                    "tool",
                    crate::application::replay::current_platform(),
                )
                .unwrap(),
            )
        };
        assert!(
            select_live().is_err(),
            "a matching layout does not establish managed ownership"
        );
        project
            .write(".tapid-managed", b"tapid-managed-v1\n")
            .unwrap();
        let selected = select_live().unwrap();
        assert_eq!(
            selected.entry,
            project.path().join("node_modules/local/cli.js")
        );
        assert_eq!(selected.selection.owner.id.name.as_str(), "tool");
        let mut foreign_plan = plan.clone();
        foreign_plan.entries[0].instance.name = "foreign".parse().unwrap();
        assert!(
            bind_live_command(
                project.path(),
                project.path(),
                &input,
                &foreign_plan,
                &tapid_linker::WorkspaceLinkPlan::default(),
                selection::select_manifest_command(
                    &input,
                    &manifest,
                    Some("local"),
                    "tool",
                    crate::application::replay::current_platform(),
                )
                .unwrap(),
            )
            .is_err()
        );
        std::fs::write(project.path().join("node_modules/local/cli.js"), b"hostile").unwrap();
        assert!(select_live().is_err());
        assert_eq!(input.root_dependencies[0].name.as_str(), "local");
        assert_eq!(
            input.instances[0].id.registry.as_str(),
            format!("file:vendor/tool.tgz#{digest}")
        );
        let tree = &trees[&key];
        assert_eq!(
            std::fs::read(tree.join("cli.js")).unwrap(),
            b"console.log('locked');\n"
        );
        std::fs::write(tree.join("cli.js"), b"modified store").unwrap();
        assert!(inspect().is_err());
        std::fs::remove_dir_all(tree).unwrap();
        assert!(inspect().is_err());
        assert!(
            !tree.exists(),
            "inspection must not repair missing evidence"
        );
    }

    #[test]
    fn installed_input_preserves_alias_and_authenticates_derived_tree_without_snapshots() {
        use tapid_test_support::{TempHome, TempProject};
        let home = TempHome::new("installed-input").unwrap();
        let source = TempProject::new("installed-source").unwrap();
        source
            .write("package.json", br#"{"name":"tool","version":"1.0.0"}"#)
            .unwrap();
        let output = TempProject::new("installed-output").unwrap();
        output
            .write(
                "package.json",
                br#"{"name":"tool","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        output
            .write("cli.js", b"console.log('generated');\n")
            .unwrap();
        let original = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let generated = tapid_archive::canonical_tree_digest(output.path()).unwrap();
        let store = Store::new(home.path().join("store"));
        let mut transaction = store.transaction();
        transaction
            .stage_verified_tree(&original.parse().unwrap(), source.path())
            .unwrap();
        transaction
            .stage_verified_tree(&generated.parse().unwrap(), output.path())
            .unwrap();
        transaction.publish().unwrap().commit().unwrap();
        let recipe = format!("sha256-{}", "b".repeat(64));
        let tag = store
            .attest_lifecycle_output(&recipe.parse().unwrap(), &generated.parse().unwrap())
            .unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_provenance(
            "https://registry.npmjs.org", "tool", "1.0.0",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", &original,
            tapid_lockfile::RegistryIntegrityProvenance::LocallyComputed,
        ).unwrap();
        let key = package.key();
        let mut lock = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
        lock.set_root_bindings(BTreeMap::from([("alias".into(), key.clone())]))
            .unwrap();
        lock.set_derived_hooks(
            &key,
            vec![
                tapid_lockfile::DerivedHookOutput::new("postinstall", &recipe, &generated, &tag)
                    .unwrap(),
            ],
        )
        .unwrap();
        let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"alias":"npm:tool@1.0.0"}}"#,
        )
        .unwrap();
        let entries = || {
            let mut paths = std::fs::read_dir(store.root())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>();
            paths.sort();
            paths
        };
        let before = entries();
        let guard = store.existing_read_guard().unwrap();
        let (input, trees) = installed_input(
            &lock,
            &manifest,
            &guard,
            &crate::registry::RegistryConfig::default(),
        )
        .unwrap();
        assert_eq!(input.root_dependencies[0].name.as_str(), "alias");
        assert_eq!(input.instances[0].id.name.as_str(), "tool");
        assert_eq!(input.instances[0].tree.digest, generated);
        assert_eq!(trees[&key], input.instances[0].tree.root);
        assert_eq!(entries(), before);
        let project = TempProject::new("installed-derived-layout").unwrap();
        let instances = input.instances.clone();
        let plan = tapid_linker::plan_named_layout(
            tapid_linker::ManagedRoot::new(project.path()).unwrap(),
            input,
            crate::application::replay::current_platform(),
        )
        .unwrap();
        crate::filesystem::tree::materialize_stage(
            project.path(),
            &plan,
            &instances,
            &trees,
            false,
        )
        .unwrap();
        crate::filesystem::tree::verification::verify_materialized_layout(
            project.path(),
            project.path(),
            &plan,
            &instances,
            &tapid_linker::WorkspaceLinkPlan::default(),
        )
        .unwrap();
        assert!(project.path().join("node_modules/alias/cli.js").is_file());
        let forged = format!("hmac-sha256-{}", "0".repeat(64));
        lock.set_derived_hooks(
            &key,
            vec![
                tapid_lockfile::DerivedHookOutput::new("postinstall", &recipe, &generated, &forged)
                    .unwrap(),
            ],
        )
        .unwrap();
        assert!(
            installed_input(
                &lock,
                &manifest,
                &guard,
                &crate::registry::RegistryConfig::default()
            )
            .is_err()
        );
        assert_eq!(entries(), before);
    }
}
