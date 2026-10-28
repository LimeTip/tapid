use super::*;

#[test]
fn pruning_required_optional_diamond_is_exact_and_order_independent() {
    let source = "https://registry.npmjs.org";
    let graph = resolve_graph(
        &[dep(source, "required", "1"), dep(source, "optional", "1")],
        &[registry(
            source,
            vec![
                package("required", "1.0.0", &[("shared", "1")]),
                package("optional", "1.0.0", &[("shared", "1")]),
                package("shared", "1.0.0", &[]),
            ],
        )],
        ResolutionOptions::default(),
    )
    .unwrap();
    let failed = graph
        .selected
        .iter()
        .find(|id| id.name.as_str() == "shared")
        .unwrap()
        .clone();
    for reverse in [false, true] {
        let mut graph = graph.clone();
        for declaration in &mut graph.declarations {
            if declaration.parent.is_none() && declaration.name.as_str() == "optional" {
                declaration.kind = DeclarationKind::Optional;
            }
        }
        if reverse {
            graph.declarations.reverse();
        }
        assert!(matches!(
            graph.prune_unavailable(&BTreeSet::from([failed.clone()])),
            Err(PruneError::RequiredDeclaration(_))
        ));
    }
}

#[test]
fn pruning_nested_branch_preserves_shared_descendant_and_aliases() {
    let source = "https://registry.npmjs.org";
    let mut graph = resolve_graph(
        &[
            dep(source, "addon", "npm:optional@1"),
            dep(source, "keep", "npm:shared@1"),
        ],
        &[registry(
            source,
            vec![
                package("optional", "1.0.0", &[("broken", "1"), ("shared", "1")]),
                package("broken", "1.0.0", &[]),
                package("shared", "1.0.0", &[]),
            ],
        )],
        ResolutionOptions::default(),
    )
    .unwrap();
    graph
        .declarations
        .iter_mut()
        .find(|edge| edge.parent.is_none() && edge.name.as_str() == "addon")
        .unwrap()
        .kind = DeclarationKind::Optional;
    let broken = graph
        .selected
        .iter()
        .find(|id| id.name.as_str() == "broken")
        .unwrap()
        .clone();
    let result = graph.prune_unavailable(&BTreeSet::from([broken])).unwrap();
    assert_eq!(
        result
            .resolution
            .selected
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["shared"]
    );
    assert_eq!(result.cuts.len(), 1);
    assert_eq!(result.cuts[0].name.as_str(), "addon");
    assert_eq!(result.cuts[0].requirement.raw, "npm:optional@1");
    assert_eq!(
        result
            .removed
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["broken", "optional"]
    );
    assert_eq!(
        result
            .resolution
            .root_bindings
            .keys()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["keep"]
    );
    assert_eq!(
        result
            .resolution
            .prune_unavailable(&BTreeSet::new())
            .unwrap()
            .resolution,
        result.resolution
    );
}

#[test]
fn pruning_keeps_same_name_and_version_from_other_registry() {
    let public = "https://registry.npmjs.org";
    let private = "https://private.example";
    let mut graph = resolve_graph(
        &[
            dep(public, "public", "npm:shared@1"),
            dep(private, "private", "npm:shared@1"),
        ],
        &[
            registry(public, vec![package("shared", "1.0.0", &[])]),
            registry(private, vec![package("shared", "1.0.0", &[])]),
        ],
        ResolutionOptions::default(),
    )
    .unwrap();
    graph
        .declarations
        .iter_mut()
        .find(|edge| edge.name.as_str() == "public")
        .unwrap()
        .kind = DeclarationKind::Optional;
    let failed = graph.root_bindings[&(public.parse().unwrap(), "public".parse().unwrap())].clone();
    let result = graph.prune_unavailable(&BTreeSet::from([failed])).unwrap();
    assert_eq!(result.resolution.selected.len(), 1);
    assert_eq!(
        result.resolution.selected[0].registry,
        private.parse::<PackageSource>().unwrap()
    );
}

#[test]
fn pruning_revalidates_required_and_optional_peer_providers() {
    let source = "https://registry.npmjs.org";
    for optional in [false, true] {
        let mut consumer = package("consumer", "1.0.0", &[]);
        consumer
            .peer_dependencies
            .insert("provider".parse().unwrap(), req("1"));
        if optional {
            consumer
                .optional_peer_dependencies
                .insert("provider".parse().unwrap());
        }
        let mut graph = resolve_graph(
            &[dep(source, "consumer", "1"), dep(source, "provider", "1")],
            &[registry(
                source,
                vec![consumer, package("provider", "1.0.0", &[])],
            )],
            ResolutionOptions::default(),
        )
        .unwrap();
        graph
            .declarations
            .iter_mut()
            .find(|edge| edge.parent.is_none() && edge.name.as_str() == "provider")
            .unwrap()
            .kind = DeclarationKind::Optional;
        let failed = graph
            .selected
            .iter()
            .find(|id| id.name.as_str() == "provider")
            .unwrap()
            .clone();
        let result = graph.prune_unavailable(&BTreeSet::from([failed]));
        if optional {
            let result = result.unwrap();
            assert_eq!(result.resolution.selected.len(), 1);
            assert!(
                result
                    .resolution
                    .peer_contexts
                    .values()
                    .all(|context| context.entries().is_empty())
            );
        } else {
            assert!(matches!(result, Err(PruneError::PeerDeclaration(_))));
        }
    }
}

#[test]
fn pruning_cycles_is_monotonic_and_cuts_only_retained_declarations() {
    let source = "https://registry.npmjs.org";
    let mut graph = resolve_graph(
        &[dep(source, "a", "1"), dep(source, "keep", "1")],
        &[registry(
            source,
            vec![
                package("a", "1.0.0", &[("b", "1")]),
                package("b", "1.0.0", &[("a", "1")]),
                package("keep", "1.0.0", &[]),
            ],
        )],
        ResolutionOptions::default(),
    )
    .unwrap();
    graph
        .declarations
        .iter_mut()
        .find(|edge| edge.parent.is_none() && edge.name.as_str() == "a")
        .unwrap()
        .kind = DeclarationKind::Optional;
    let failed = graph
        .selected
        .iter()
        .find(|id| id.name.as_str() == "b")
        .unwrap()
        .clone();
    let result = graph.prune_unavailable(&BTreeSet::from([failed])).unwrap();
    assert_eq!(result.removed.len(), 2);
    assert_eq!(result.cuts.len(), 1);
    assert_eq!(result.resolution.selected[0].name.as_str(), "keep");
}

#[test]
fn pruning_rejects_missing_provenance_and_unknown_failures() {
    let source = "https://registry.npmjs.org";
    let graph = resolve_graph(
        &[dep(source, "a", "1")],
        &[registry(
            source,
            vec![
                package("a", "1.0.0", &[("b", "1")]),
                package("b", "1.0.0", &[]),
            ],
        )],
        ResolutionOptions::default(),
    )
    .unwrap();
    let mut incomplete = graph.clone();
    incomplete.declarations.pop();
    assert_eq!(
        incomplete.prune_unavailable(&BTreeSet::new()),
        Err(PruneError::InvalidGraph)
    );
    let mut unknown = graph.selected[0].clone();
    unknown.version = "9.0.0".parse().unwrap();
    assert_eq!(
        graph.prune_unavailable(&BTreeSet::from([unknown])),
        Err(PruneError::InvalidGraph)
    );
}

#[test]
fn pruning_optional_child_discards_only_unreachable_descendants() {
    let source = "https://registry.npmjs.org";
    let mut graph = resolve_graph(
        &[dep(source, "app", "1")],
        &[registry(
            source,
            vec![
                package("app", "1.0.0", &[("addon", "1"), ("shared", "1")]),
                package(
                    "addon",
                    "1.0.0",
                    &[("broken", "1"), ("leaf", "1"), ("shared", "1")],
                ),
                package("broken", "1.0.0", &[]),
                package("leaf", "1.0.0", &[]),
                package("shared", "1.0.0", &[]),
            ],
        )],
        ResolutionOptions::default(),
    )
    .unwrap();
    graph
        .declarations
        .iter_mut()
        .find(|edge| {
            edge.parent
                .as_ref()
                .is_some_and(|id| id.name.as_str() == "app")
                && edge.name.as_str() == "addon"
        })
        .unwrap()
        .kind = DeclarationKind::Optional;
    let broken = graph
        .selected
        .iter()
        .find(|id| id.name.as_str() == "broken")
        .unwrap()
        .clone();
    let failures = BTreeSet::from([broken]);
    let expected = graph.prune_unavailable(&failures).unwrap();
    assert_eq!(
        expected
            .resolution
            .selected
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["app", "shared"]
    );
    assert_eq!(
        expected
            .removed
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["addon", "broken", "leaf"]
    );
    assert_eq!(expected.cuts.len(), 1);
    assert_eq!(
        expected.cuts[0].parent.as_ref().unwrap().name.as_str(),
        "app"
    );
    graph.declarations.reverse();
    graph.selected.reverse();
    graph.dependencies.reverse();
    graph.roots.reverse();
    assert_eq!(graph.prune_unavailable(&failures).unwrap(), expected);
}

#[test]
fn pruning_rejects_conflicting_edge_kinds_before_accepting_omissions() {
    let source = "https://registry.npmjs.org";
    let mut graph = resolve_graph(
        &[dep(source, "app", "1")],
        &[registry(source, vec![package("app", "1.0.0", &[])])],
        ResolutionOptions::default(),
    )
    .unwrap();
    let mut conflicting = graph.declarations[0].clone();
    conflicting.kind = DeclarationKind::Optional;
    graph.declarations.push(conflicting);
    assert_eq!(
        graph.prune_unavailable(&BTreeSet::new()),
        Err(PruneError::InvalidGraph)
    );
}

#[test]
fn pruning_many_optional_parents_preserves_one_required_shared_instance() {
    let source = "https://registry.npmjs.org";
    let mut packages = vec![
        package("shared", "1.0.0", &[]),
        package("broken", "1.0.0", &[]),
    ];
    let mut roots = vec![dep(source, "shared", "1")];
    for index in 0..256 {
        let name = format!("addon-{index:03}");
        packages.push(package(&name, "1.0.0", &[("shared", "1"), ("broken", "1")]));
        roots.push(dep(source, &name, "1"));
    }
    let mut graph = resolve_graph(
        &roots,
        &[registry(source, packages)],
        ResolutionOptions::default(),
    )
    .unwrap();
    for edge in &mut graph.declarations {
        if edge.parent.is_none() && edge.name.as_str().starts_with("addon-") {
            edge.kind = DeclarationKind::Optional;
        }
    }
    let broken = graph
        .selected
        .iter()
        .find(|id| id.name.as_str() == "broken")
        .unwrap()
        .clone();
    let result = graph.prune_unavailable(&BTreeSet::from([broken])).unwrap();
    assert_eq!(result.resolution.selected.len(), 1);
    assert_eq!(result.resolution.selected[0].name.as_str(), "shared");
    assert_eq!(result.cuts.len(), 256);
    assert_eq!(result.removed.len(), 257);
}

#[test]
fn pruning_rejects_unreachable_selected_instances() {
    let source = "https://registry.npmjs.org";
    let mut graph = resolve_graph(
        &[dep(source, "app", "1")],
        &[registry(source, vec![package("app", "1.0.0", &[])])],
        ResolutionOptions::default(),
    )
    .unwrap();
    let mut orphan = graph.selected[0].clone();
    orphan.name = "orphan".parse().unwrap();
    graph.selected.push(orphan.clone());
    graph.peer_contexts.insert(orphan, PeerContext::default());
    assert_eq!(
        graph.prune_unavailable(&BTreeSet::new()),
        Err(PruneError::InvalidGraph)
    );
}
