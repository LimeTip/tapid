use super::*;
use std::{collections::BTreeSet, io::Read};

enum ExpectedEntry {
    Directory,
    File(PathBuf),
    Bytes(Vec<u8>),
    Link(PathBuf),
}

/// Compare the whole live dependency namespace with authenticated references
/// and exact materialization outputs. The caller owns project coordination and
/// retained store guards; this operation creates no snapshots or other state.
/// Workspace bytes are mutable and are deliberately not archive-authenticated.
/// Manifest/lock binding checks and launch authorization belong to the caller.
#[allow(dead_code)] // Awaiting installed-command selection; exercised through materialization fixtures.
pub(crate) fn verify_materialized_layout(
    project: &Path,
    workspace_root: &Path,
    plan: &tapid_linker::MaterializationPlan,
    instances: &[PackageInstance],
    workspace_links: &tapid_linker::WorkspaceLinkPlan,
) -> Result<(), String> {
    if project != plan.managed_root.path {
        return Err("materialized layout root does not match project".into());
    }
    let root = project.join("node_modules");
    let mut expected = BTreeMap::from([(root.clone(), ExpectedEntry::Directory)]);
    let references = instances
        .iter()
        .map(|instance| (instance.tree.root.clone(), instance.tree.digest.clone()))
        .collect::<BTreeMap<_, _>>();
    for instance in instances {
        if references.get(&instance.tree.root) != Some(&instance.tree.digest) {
            return Err("conflicting reference tree identities".into());
        }
    }
    verify_references(&references)?;
    for step in &plan.activation.steps {
        let entry = plan
            .entries
            .iter()
            .find(|entry| entry.target == step.source)
            .ok_or("materialized activation source missing")?;
        if !references.contains_key(&entry.source) {
            return Err("materialized reference missing".into());
        }
        add_parents(&mut expected, &root, &step.target)?;
        collect_reference(
            &package_content_root(&entry.source)?,
            &step.target,
            &mut expected,
            true,
        )?;
    }
    // Validate all reference-backed entries before the shim planner reads live
    // manifests and targets. Executable-bit changes are checked after planning.
    for (path, entry) in &expected {
        verify_entry(path, entry)?;
    }
    let canonical_project = fs::canonicalize(workspace_root).map_err(|e| e.to_string())?;
    if !fs::canonicalize(project)
        .map_err(|e| e.to_string())?
        .starts_with(&canonical_project)
    {
        return Err("materialized project escapes workspace root".into());
    }
    for link in &workspace_links.links {
        let source = fs::canonicalize(&link.source).map_err(|e| e.to_string())?;
        if source == canonical_project || !source.starts_with(&canonical_project) {
            return Err("workspace link escapes project".into());
        }
        let manifest =
            fs::symlink_metadata(source.join("package.json")).map_err(|e| e.to_string())?;
        if !regular_entry(&manifest) || !manifest.is_file() {
            return Err("workspace manifest is not a regular file".into());
        }
        let target = root.join(link.name.as_str().split('/').collect::<PathBuf>());
        if target != link.target {
            return Err("workspace link placement differs from plan".into());
        }
        add_parents(&mut expected, &root, &target)?;
        #[cfg(unix)]
        let destination = relative_path(
            target.parent().ok_or("workspace link has no parent")?,
            &source,
        );
        #[cfg(not(unix))]
        let destination = source;
        insert_entry(
            &mut expected,
            target.clone(),
            ExpectedEntry::Link(destination),
        )?;
        verify_entry(&target, expected.get(&target).unwrap())?;
    }
    let shims = package_shim_plan(project, plan, workspace_links, &canonical_project)?;
    let mut executable_targets = BTreeSet::new();
    for entry in shims.entries {
        let parent = entry.target.parent().ok_or("shim has no parent")?;
        match entry.strategy {
            tapid_linker::ShimStrategy::UnixSymlink => {
                add_parents(&mut expected, &root, &entry.target)?;
                insert_entry(
                    &mut expected,
                    entry.target.clone(),
                    ExpectedEntry::Link(relative_path(parent, &entry.source)),
                )?;
                executable_targets.insert(entry.source);
            }
            tapid_linker::ShimStrategy::WindowsCmdAndPowerShell => {
                for (extension, contents) in [
                    (
                        "cmd",
                        cmd_shim_contents(parent, &entry.source, entry.interpreter),
                    ),
                    (
                        "ps1",
                        powershell_shim_contents(parent, &entry.source, entry.interpreter),
                    ),
                ] {
                    let target = entry.target.with_extension(extension);
                    add_parents(&mut expected, &root, &target)?;
                    insert_entry(
                        &mut expected,
                        target,
                        ExpectedEntry::Bytes(contents.into_bytes()),
                    )?;
                }
            }
        }
    }
    if verify_namespace(&root, &expected, &executable_targets)? != expected.len() {
        return Err("missing materialized entries".into());
    }
    verify_references(&references)
}

fn verify_references(references: &BTreeMap<PathBuf, String>) -> Result<(), String> {
    for (path, digest) in references {
        if tapid_archive::canonical_tree_digest(path).map_err(|e| e.to_string())? != *digest {
            return Err(format!(
                "reference tree content changed: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn insert_entry(
    expected: &mut BTreeMap<PathBuf, ExpectedEntry>,
    path: PathBuf,
    entry: ExpectedEntry,
) -> Result<(), String> {
    if let Some(previous) = expected.get(&path) {
        if matches!(
            (previous, &entry),
            (ExpectedEntry::Directory, ExpectedEntry::Directory)
        ) {
            return Ok(());
        }
        return Err(format!(
            "overlapping materialized entries: {}",
            path.display()
        ));
    }
    expected.insert(path, entry);
    Ok(())
}

fn add_parents(
    expected: &mut BTreeMap<PathBuf, ExpectedEntry>,
    root: &Path,
    path: &Path,
) -> Result<(), String> {
    if path == root
        || !path.starts_with(root)
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("materialized path escapes dependency root".into());
    }
    let mut parent = path.parent();
    while let Some(path) = parent {
        insert_entry(expected, path.to_path_buf(), ExpectedEntry::Directory)?;
        if path == root {
            break;
        }
        parent = path.parent();
    }
    Ok(())
}

fn collect_reference(
    source: &Path,
    target: &Path,
    expected: &mut BTreeMap<PathBuf, ExpectedEntry>,
    package_root: bool,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if !regular_entry(&metadata) || !metadata.is_dir() {
        return Err("reference package root is not a directory".into());
    }
    insert_entry(expected, target.to_path_buf(), ExpectedEntry::Directory)?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if package_root && entry.file_name() == ".tapid-executable-modes" {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if !regular_entry(&metadata) {
            return Err("redirected reference entry".into());
        }
        let target = target.join(entry.file_name());
        if metadata.is_dir() {
            collect_reference(&entry.path(), &target, expected, false)?;
        } else if metadata.is_file() {
            insert_entry(expected, target, ExpectedEntry::File(entry.path()))?;
        } else {
            return Err("special reference entry".into());
        }
    }
    Ok(())
}

fn regular_entry(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    !metadata.file_type().is_symlink()
}

fn verify_entry(path: &Path, entry: &ExpectedEntry) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("missing materialized entry {}: {e}", path.display()))?;
    let valid_type = match entry {
        ExpectedEntry::Directory => regular_entry(&metadata) && metadata.is_dir(),
        ExpectedEntry::File(_) | ExpectedEntry::Bytes(_) => {
            regular_entry(&metadata) && metadata.is_file()
        }
        ExpectedEntry::Link(_) => metadata.file_type().is_symlink(),
    };
    if !valid_type {
        return Err(format!(
            "materialized entry type differs: {}",
            path.display()
        ));
    }
    let content_matches = match entry {
        ExpectedEntry::Directory => true,
        ExpectedEntry::Link(target) => fs::read_link(path).map_err(|e| e.to_string())? == *target,
        ExpectedEntry::Bytes(contents) => {
            metadata.len() == contents.len() as u64
                && fs::read(path).map_err(|e| e.to_string())? == *contents
        }
        ExpectedEntry::File(source) => files_match(source, path)?,
    };
    if !content_matches {
        return Err(format!("materialized content differs: {}", path.display()));
    }
    Ok(())
}

fn files_match(source: &Path, target: &Path) -> Result<bool, String> {
    let mut source = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut target = fs::File::open(target).map_err(|e| e.to_string())?;
    let length = source.metadata().map_err(|e| e.to_string())?.len();
    if target.metadata().map_err(|e| e.to_string())?.len() != length {
        return Ok(false);
    }
    let mut left = [0; 65536];
    let mut right = [0; 65536];
    let mut remaining = length;
    while remaining > 0 {
        let count = remaining.min(left.len() as u64) as usize;
        source
            .read_exact(&mut left[..count])
            .map_err(|e| e.to_string())?;
        target
            .read_exact(&mut right[..count])
            .map_err(|e| e.to_string())?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(source.read(&mut left[..1]).map_err(|e| e.to_string())? == 0
        && target.read(&mut right[..1]).map_err(|e| e.to_string())? == 0)
}

fn verify_namespace(
    path: &Path,
    expected: &BTreeMap<PathBuf, ExpectedEntry>,
    executable_targets: &BTreeSet<PathBuf>,
) -> Result<usize, String> {
    let entry = expected
        .get(path)
        .ok_or_else(|| format!("unexpected materialized entry: {}", path.display()))?;
    verify_entry(path, entry)?;
    #[cfg(unix)]
    if let ExpectedEntry::File(source) = entry {
        use std::os::unix::fs::PermissionsExt;
        let source_mode = fs::metadata(source)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        let target_mode = fs::symlink_metadata(path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        let expected_executable = source_mode & 0o111 != 0 || executable_targets.contains(path);
        if target_mode & 0o7000 != 0 || (target_mode & 0o111 != 0) != expected_executable {
            return Err(format!(
                "materialized executable mode differs: {}",
                path.display()
            ));
        }
    }
    let mut count = 1;
    if matches!(entry, ExpectedEntry::Directory) {
        for child in fs::read_dir(path).map_err(|e| e.to_string())? {
            let child = child.map_err(|e| e.to_string())?.path();
            count += verify_namespace(&child, expected, executable_targets)?;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_core::{PackageInstanceId, PeerContext, PlatformContext};
    use tapid_linker::{InstanceKey, LayoutInput, VerifiedTreeReference, WorkspaceLinkPlan};
    use tapid_test_support::TempProject;

    #[test]
    fn materialized_verification_rejects_tampered_content_and_unplanned_overlays() {
        let source = TempProject::new("materialized-reference").unwrap();
        source
            .write(
                "package/package.json",
                br#"{"name":"tool","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        source
            .write("package/cli.js", b"console.log('tool');\n")
            .unwrap();
        source
            .write("package/lib/runtime.js", b"module.exports = 1;\n")
            .unwrap();
        let project = TempProject::new("materialized-live").unwrap();
        let digest = tapid_archive::canonical_tree_digest(source.path()).unwrap();
        let instance = PackageInstance {
            id: PackageInstanceId::new(
                "https://registry.example".parse().unwrap(),
                "tool".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            ),
            peer_context: PeerContext::default(),
            platform_context: PlatformContext::new(None, None, None).unwrap(),
            tree: VerifiedTreeReference::new(&digest, source.path()).unwrap(),
        };
        let plan = tapid_linker::plan_layout(
            ManagedRoot::new(project.path()).unwrap(),
            LayoutInput {
                instances: vec![instance.clone()],
                root_dependencies: vec![InstanceKey::from(&instance)],
                dependency_edges: vec![],
            },
            crate::application::replay::current_platform(),
        )
        .unwrap();
        super::super::materialize_stage(
            project.path(),
            &plan,
            std::slice::from_ref(&instance),
            &BTreeMap::from([("tool".into(), source.path().to_path_buf())]),
            false,
        )
        .unwrap();
        let verify = || {
            verify_materialized_layout(
                project.path(),
                project.path(),
                &plan,
                std::slice::from_ref(&instance),
                &WorkspaceLinkPlan::default(),
            )
        };
        verify().unwrap();
        let runtime = project.path().join("node_modules/tool/lib/runtime.js");
        fs::write(&runtime, b"module.exports = 2;\n").unwrap();
        assert!(verify().unwrap_err().contains("content"));
        fs::write(&runtime, b"module.exports = 1;\n").unwrap();
        let overlay = project
            .path()
            .join("node_modules/tool/node_modules/foreign/index.js");
        fs::create_dir_all(overlay.parent().unwrap()).unwrap();
        fs::write(&overlay, b"injected").unwrap();
        assert!(verify().unwrap_err().contains("unexpected"));
        fs::remove_dir_all(project.path().join("node_modules/tool/node_modules")).unwrap();
        verify().unwrap();
        let shim = project.path().join(if cfg!(windows) {
            "node_modules/.bin/tool.cmd"
        } else {
            "node_modules/.bin/tool"
        });
        fs::remove_file(&shim).unwrap();
        assert!(verify().unwrap_err().contains("missing"));
        #[cfg(unix)]
        std::os::unix::fs::symlink("../tool/cli.js", &shim).unwrap();
        #[cfg(windows)]
        fs::write(
            &shim,
            cmd_shim_contents(
                shim.parent().unwrap(),
                &project.path().join("node_modules/tool/cli.js"),
                tapid_linker::ShimInterpreter::Direct,
            ),
        )
        .unwrap();
        verify().unwrap();
        #[cfg(unix)]
        {
            fs::remove_file(&runtime).unwrap();
            std::os::unix::fs::symlink(source.path().join("package/lib/runtime.js"), &runtime)
                .unwrap();
            assert!(verify().unwrap_err().contains("type"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn materialized_verification_checks_workspace_links_without_freezing_workspace_bytes() {
        let project = TempProject::new("verified-workspace").unwrap();
        project
            .write(
                "packages/member/package.json",
                br#"{"name":"member","version":"1.0.0","bin":{"member":"cli.js"}}"#,
            )
            .unwrap();
        project
            .write("packages/member/cli.js", b"console.log('mutable');\n")
            .unwrap();
        let member = fs::canonicalize(project.path().join("packages/member")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(member.join("cli.js"), fs::Permissions::from_mode(0o755)).unwrap();
        let root = fs::canonicalize(project.path()).unwrap();
        let plan = tapid_linker::plan_layout(
            ManagedRoot::new(&root).unwrap(),
            LayoutInput {
                instances: vec![],
                root_dependencies: vec![],
                dependency_edges: vec![],
            },
            tapid_linker::Platform::Unix,
        )
        .unwrap();
        let links = WorkspaceLinkPlan {
            links: vec![tapid_linker::WorkspaceLink {
                name: "member".parse().unwrap(),
                version: "1.0.0".parse().unwrap(),
                source: member.clone(),
                target: root.join("node_modules/member"),
            }],
        };
        let stage = root.join(".stage");
        super::super::materialize_stage_with_workspace_links(
            &stage,
            &plan,
            &[],
            &BTreeMap::new(),
            false,
            &links,
            &root,
            |_, _| {},
        )
        .unwrap();
        fs::rename(stage.join("node_modules"), root.join("node_modules")).unwrap();
        let verify = || verify_materialized_layout(&root, &root, &plan, &[], &links);
        verify().unwrap();
        fs::write(member.join("cli.js"), b"console.log('edited');\n").unwrap();
        verify().unwrap();
        let shim = root.join("node_modules/.bin/member");
        fs::remove_file(&shim).unwrap();
        std::os::unix::fs::symlink("../../packages/member/cli.js", &shim).unwrap();
        assert!(verify().unwrap_err().contains("content"));
        fs::remove_file(&shim).unwrap();
        std::os::unix::fs::symlink("../member/cli.js", &shim).unwrap();
        let link = root.join("node_modules/member");
        fs::remove_file(&link).unwrap();
        let outside = TempProject::new("foreign-workspace").unwrap();
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(verify().unwrap_err().contains("content"));
    }

    #[test]
    fn materialized_verification_preserves_aliases_and_exact_nested_placements() {
        use tapid_linker::{NamedDependency, NamedDependencyEdge, NamedLayoutInput};
        let project = TempProject::new("verified-alias-layout").unwrap();
        let parent_source = TempProject::new("verified-parent-reference").unwrap();
        parent_source
            .write("package.json", br#"{"name":"parent","version":"1.0.0"}"#)
            .unwrap();
        let child_source = TempProject::new("verified-child-reference").unwrap();
        child_source
            .write("package.json", br#"{"name":"child","version":"1.0.0"}"#)
            .unwrap();
        child_source
            .write("runtime.js", b"module.exports = 'child';\n")
            .unwrap();
        let instance = |name: &str, source: &Path| PackageInstance {
            id: PackageInstanceId::new(
                "https://registry.example".parse().unwrap(),
                name.parse().unwrap(),
                "1.0.0".parse().unwrap(),
            ),
            peer_context: PeerContext::default(),
            platform_context: PlatformContext::new(None, None, None).unwrap(),
            tree: VerifiedTreeReference::new(
                &tapid_archive::canonical_tree_digest(source).unwrap(),
                source,
            )
            .unwrap(),
        };
        let parent = instance("parent", parent_source.path());
        let child = instance("child", child_source.path());
        let instances = vec![parent.clone(), child.clone()];
        let plan = tapid_linker::plan_named_layout(
            ManagedRoot::new(project.path()).unwrap(),
            NamedLayoutInput {
                instances: instances.clone(),
                root_dependencies: vec![
                    NamedDependency {
                        name: "parent-alias".parse().unwrap(),
                        child: InstanceKey::from(&parent),
                    },
                    NamedDependency {
                        name: "child-alias".parse().unwrap(),
                        child: InstanceKey::from(&child),
                    },
                    NamedDependency {
                        name: "nested-child".parse().unwrap(),
                        child: InstanceKey::from(&parent),
                    },
                ],
                dependency_edges: vec![NamedDependencyEdge {
                    parent: InstanceKey::from(&parent),
                    dependency: NamedDependency {
                        name: "nested-child".parse().unwrap(),
                        child: InstanceKey::from(&child),
                    },
                }],
            },
            crate::application::replay::current_platform(),
        )
        .unwrap();
        super::super::materialize_stage(
            project.path(),
            &plan,
            &instances,
            &BTreeMap::from([
                ("parent".into(), parent_source.path().to_path_buf()),
                ("child".into(), child_source.path().to_path_buf()),
            ]),
            false,
        )
        .unwrap();
        let verify = || {
            verify_materialized_layout(
                project.path(),
                project.path(),
                &plan,
                &instances,
                &WorkspaceLinkPlan::default(),
            )
        };
        verify().unwrap();
        let nested = project
            .path()
            .join("node_modules/parent-alias/node_modules/nested-child/runtime.js");
        assert!(nested.is_file());
        fs::write(nested, b"module.exports = 'forged';\n").unwrap();
        assert!(verify().unwrap_err().contains("content"));
    }

    #[cfg(unix)]
    #[test]
    fn materialized_member_layout_uses_workspace_root_for_sibling_links() {
        use std::os::unix::fs::PermissionsExt;
        let project = TempProject::new("verified-member-layout").unwrap();
        project
            .write(
                "packages/tool/package.json",
                br#"{"name":"tool","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
        project
            .write("packages/tool/cli.js", b"console.log('tool');\n")
            .unwrap();
        let root = fs::canonicalize(project.path()).unwrap();
        let tool = root.join("packages/tool");
        fs::set_permissions(tool.join("cli.js"), fs::Permissions::from_mode(0o755)).unwrap();
        let member = root.join("packages/consumer");
        fs::create_dir(&member).unwrap();
        let plan = tapid_linker::plan_layout(
            ManagedRoot::new(&member).unwrap(),
            LayoutInput {
                instances: vec![],
                root_dependencies: vec![],
                dependency_edges: vec![],
            },
            tapid_linker::Platform::Unix,
        )
        .unwrap();
        let links = WorkspaceLinkPlan {
            links: vec![tapid_linker::WorkspaceLink {
                name: "tool".parse().unwrap(),
                version: "1.0.0".parse().unwrap(),
                source: tool,
                target: member.join("node_modules/tool"),
            }],
        };
        let stage = member.join(".stage");
        super::super::materialize_stage_with_workspace_links(
            &stage,
            &plan,
            &[],
            &BTreeMap::new(),
            false,
            &links,
            &root,
            |_, _| {},
        )
        .unwrap();
        fs::rename(stage.join("node_modules"), member.join("node_modules")).unwrap();
        verify_materialized_layout(&member, &root, &plan, &[], &links).unwrap();
        assert!(verify_materialized_layout(&member, &member, &plan, &[], &links).is_err());
    }
}
