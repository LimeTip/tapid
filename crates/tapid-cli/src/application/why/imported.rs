use super::*;
use crate::application::lifecycle::WorkspaceSelection;
use tapid_lockfile::ImportedNpmLockfile;

pub(super) fn explain(
    selection: &WorkspaceSelection,
    manifest_bytes: &[u8],
    lock_bytes: &str,
    requested: &tapid_core::PackageName,
) -> Result<WhyReport, OperationalError> {
    let invalid = |error| OperationalError::new(ErrorKind::Lockfile, error);
    let lock = ImportedNpmLockfile::from_json(lock_bytes).map_err(invalid)?;
    let manifest = std::str::from_utf8(manifest_bytes)
        .map_err(|error| OperationalError::new(ErrorKind::Manifest, error))?;
    let digest = digest_bytes(manifest_bytes);
    let facts = lock.placement_selection();
    if let Some(facts) = facts {
        lock.validate_replay_with_selection(manifest, &digest, &facts.target, facts.profile)
    } else {
        lock.validate_replay(manifest, &digest)
    }
    .map_err(invalid)?;
    let workspace = tapid_manifest::Workspace::discover_typed(&selection.root_dir)
        .map_err(OperationalError::from)?;
    let mut manifests = BTreeMap::new();
    for member in workspace.members() {
        let path = member.path();
        let relative = path
            .parent()
            .and_then(|p| p.strip_prefix(&selection.root_dir).ok())
            .ok_or_else(|| OperationalError::new(ErrorKind::Manifest, "workspace outside project"))?
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(path)
            .map_err(|error| OperationalError::from_source(ErrorKind::Manifest, error))?;
        manifests.insert(relative, (text.clone(), digest_bytes(text.as_bytes())));
    }
    lock.validate_workspace_manifests(&manifests)
        .map_err(invalid)?;
    let imported = lock.graph().map_err(invalid)?;
    let selected = if let Some(facts) = facts {
        imported.selected_paths(
            &facts.target.os,
            &facts.target.cpu,
            facts.target.libc.as_deref(),
        )
    } else {
        imported.selected_paths(
            std::env::consts::OS,
            std::env::consts::ARCH,
            crate::online::current_libc(),
        )
    }
    .map_err(invalid)?;
    let mut graph = WhyGraph {
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    for (path, package) in &imported.packages {
        if !selected.contains(path) {
            continue;
        }
        graph.nodes.insert(
            path.clone(),
            WhyNode {
                name: package.name.clone(),
                source_kind: "registry",
            },
        );
        let edges = package
            .dependencies
            .iter()
            .map(|(name, target)| Edge {
                name: name.clone(),
                target: target.clone(),
                kind: if package.optional_dependencies.contains(name) {
                    "optionalDependencies"
                } else {
                    "dependencies"
                },
            })
            .chain(package.peers.iter().map(|(name, target)| Edge {
                name: name.clone(),
                target: target.clone(),
                kind: if package.optional_peers.contains(name) {
                    "optionalPeerDependencies"
                } else {
                    "peerDependencies"
                },
            }))
            .collect();
        graph.edges.insert(path.clone(), edges);
    }
    for (path, member) in &imported.workspaces {
        let edge_kind = |name: &str| {
            if member["optionalDependencies"].get(name).is_some() {
                "optionalDependencies"
            } else if member["dependencies"].get(name).is_some() {
                "dependencies"
            } else if member["devDependencies"].get(name).is_some() {
                "devDependencies"
            } else if member["peerDependenciesMeta"][name]["optional"].as_bool() == Some(true) {
                "optionalPeerDependencies"
            } else {
                "peerDependencies"
            }
        };
        let name = member["name"]
            .as_str()
            .ok_or_else(|| OperationalError::new(ErrorKind::Lockfile, "workspace name missing"))?
            .parse()
            .map_err(|error| OperationalError::new(ErrorKind::Lockfile, error))?;
        graph.nodes.insert(
            path.clone(),
            WhyNode {
                name,
                source_kind: "workspace",
            },
        );
        let edges = imported
            .member_roots
            .get(path)
            .map(|roots| {
                roots
                    .dependencies
                    .iter()
                    .map(|(name, target)| Edge {
                        name: name.clone(),
                        target: target.clone(),
                        kind: edge_kind(name),
                    })
                    .collect()
            })
            .unwrap_or_default();
        graph.edges.insert(path.clone(), edges);
        for (name, target) in lock.workspace_bindings(path).map_err(invalid)? {
            let kind = edge_kind(&name);
            graph
                .edges
                .get_mut(path)
                .expect("indexed workspace")
                .push(Edge { name, target, kind });
        }
    }
    // Links retain their physical identity; they lead to the workspace source.
    for (path, target) in &imported.workspace_links {
        if let Some(node) = graph.nodes.get(target) {
            let name = node.name.to_string();
            graph.nodes.insert(
                path.clone(),
                WhyNode {
                    name: node.name.clone(),
                    source_kind: "workspace",
                },
            );
            graph.edges.insert(
                path.clone(),
                vec![Edge {
                    name,
                    target: target.clone(),
                    kind: "workspace",
                }],
            );
        }
    }
    let root_manifest = selection.root_dir.join("package.json");
    let member = if selection.manifest_path == root_manifest {
        None
    } else {
        Some(
            selection
                .manifest_path
                .parent()
                .unwrap()
                .strip_prefix(&selection.root_dir)
                .map_err(|error| OperationalError::new(ErrorKind::Manifest, error))?
                .to_string_lossy()
                .replace('\\', "/"),
        )
    };
    let mut roots = Vec::new();
    if let Some(member) = &member {
        roots.push(Root {
            key: member.clone(),
            name: selection.manifest.name().to_string(),
            kind: "workspace".into(),
        });
    } else {
        let kinds = direct_dependency_kinds(&selection.manifest);
        roots.extend(imported.roots.iter().filter_map(|(name, target)| {
            Some(Root {
                key: target.clone(),
                name: name.clone(),
                kind: kinds.get(name)?.clone(),
            })
        }));
        for (name, target) in lock.workspace_bindings("").map_err(invalid)? {
            roots.push(Root {
                key: target,
                kind: kinds
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| "dependency".into()),
                name,
            });
        }
        roots.extend(imported.workspaces.iter().map(|(path, member)| {
            Root {
                key: path.clone(),
                name: member["name"]
                    .as_str()
                    .expect("validated workspace name")
                    .to_owned(),
                kind: "workspace".into(),
            }
        }));
    }
    let mut state = SearchState::default();
    for root in roots {
        let Some(node) = graph.nodes.get(&root.key) else {
            continue;
        };
        let mut path = vec![WhyStep {
            key: root.key.clone(),
            via: Some(root.name),
            edge_kind: Some(root.kind),
            source_kind: node.source_kind,
        }];
        search_paths(
            &root.key,
            requested,
            &graph,
            &mut BTreeSet::from([root.key.clone()]),
            &mut path,
            &mut state,
            0,
        );
        if state.truncated {
            break;
        }
    }
    let mut omissions = Vec::new();
    if let Some(facts) = facts {
        for omission in &facts.omissions {
            if member.is_some() && !state.visited_nodes.contains(&omission.declaring_placement) {
                continue;
            }
            let local = omission
                .name
                .parse::<tapid_core::PackageName>()
                .map_err(|error| OperationalError::new(ErrorKind::Lockfile, error))?;
            let requirement = omission
                .requirement
                .parse::<tapid_resolver::Requirement>()
                .map_err(|error| OperationalError::new(ErrorKind::Lockfile, error))?;
            if &local != requested && requirement.package_name(&local) != requested {
                continue;
            }
            if omissions.len() == MAX_PATHS {
                state.truncated = true;
                break;
            }
            omissions.push(omission.clone());
        }
    }
    Ok(WhyReport {
        schema_version: 1,
        operation: "why",
        outcome: if state.truncated {
            "partial"
        } else if state.paths.is_empty() && omissions.is_empty() {
            "not_found"
        } else {
            "success"
        },
        effective_project: selection.root_dir.display().to_string(),
        changes: Vec::new(),
        warnings: Vec::new(),
        package: requested.to_string(),
        paths: state.paths,
        omissions: Vec::new(),
        imported_omissions: omissions,
        selection: facts.map(|facts| WhySelection {
            target: facts.target.clone(),
            profile: facts.profile,
        }),
        truncated: state.truncated,
    })
}
