use super::*;
use crate::tests::{dep, package, registry, req};
const PUBLIC: &str = "https://registry.npmjs.org";

#[test]
fn contextual_failure_identifies_scope_source_and_correction_with_bounded_output() {
    let error = resolve(
        vec![
            package("parent", "1.0.0", &[("host", "2"), ("plugin", "1")]),
            package("host", "2.0.0", &[]),
            peer_plugin(false, "1"),
        ],
        &[dep(PUBLIC, "parent", "1")],
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("6:parent6:plugin"), "{message}");
    assert!(message.contains(PUBLIC), "{message}");
    assert!(message.contains("host@1"), "{message}");
    assert!(message.contains("2.0.0"), "{message}");
    assert!(message.contains("correct the nearest"), "{message}");
    assert!(!message.contains("direct root"), "{message}");
}

#[test]
fn contextual_diagnostic_truncates_large_multibyte_requirements() {
    let mut plugin = peer_plugin(false, "1");
    plugin
        .peer_dependencies
        .get_mut(&"host".parse().unwrap())
        .unwrap()
        .raw = "é".repeat(10_000);
    let error = resolve(vec![plugin], &[dep(PUBLIC, "plugin", "1")]).unwrap_err();
    let message = error.to_string();
    assert!(message.len() <= 8192);
    assert!(message.ends_with(" [truncated]"));
    assert!(message.contains("plugin@1.0.0"));
}

#[test]
fn dependency_recurrence_rejects_a_changed_provider_even_at_the_same_version() {
    let mut a = package("a", "1.0.0", &[("b", "1")]);
    a.peer_dependencies
        .insert("host".parse().unwrap(), req("1"));
    let error = resolve(
        vec![
            a,
            package("b", "1.0.0", &[("a", "1"), ("host", "1")]),
            package("host", "1.0.0", &[]),
        ],
        &[dep(PUBLIC, "a", "1"), dep(PUBLIC, "host", "1")],
    )
    .unwrap_err();
    assert!(
        matches!(error, ResolveError::UnsupportedMode(message) if message.contains("changes exact peer-provider environment"))
    );
}

#[test]
fn dependency_cycle_with_peer_bearing_descendants_retains_their_provider() {
    let graph = resolve(
        vec![
            package("a", "1.0.0", &[("b", "1"), ("plugin", "1")]),
            package("b", "1.0.0", &[("a", "1")]),
            package("host", "1.0.0", &[]),
            peer_plugin(false, "1"),
        ],
        &[dep(PUBLIC, "a", "1"), dep(PUBLIC, "host", "1")],
    )
    .unwrap();
    let a = &graph.instances[&graph.roots[&(PUBLIC.parse().unwrap(), "a".parse().unwrap())]];
    let host = &graph.roots[&(PUBLIC.parse().unwrap(), "host".parse().unwrap())];
    assert_eq!(
        &child(&graph, a, "plugin").peers[&"host".parse().unwrap()],
        host
    );
    assert_eq!(graph.instances.len(), 4);
}
fn resolve(
    packages: Vec<PackageVersionMetadata>,
    roots: &[Dependency],
) -> Result<InstanceResolution, ResolveError> {
    resolve_instances_with_preferences(
        roots,
        &[registry(PUBLIC, packages)],
        Default::default(),
        |source, _| Ok(source.clone()),
        &ResolutionPreferences::default(),
    )
}
fn peer_plugin(optional: bool, requirement: &str) -> PackageVersionMetadata {
    let mut plugin = package("plugin", "1.0.0", &[]);
    plugin
        .peer_dependencies
        .insert("host".parse().unwrap(), req(requirement));
    if optional {
        plugin
            .optional_peer_dependencies
            .insert("host".parse().unwrap());
    }
    plugin
}
fn child<'a>(
    graph: &'a InstanceResolution,
    parent: &ResolvedInstance,
    name: &str,
) -> &'a ResolvedInstance {
    &graph.instances[&parent.dependencies[&name.parse().unwrap()]]
}
#[test]
fn distinct_placements_retain_exact_provider_references_and_aliases() {
    let graph = resolve(
        vec![
            package(
                "left",
                "1.0.0",
                &[("host", "1.1.0"), ("plug", "npm:plugin@1")],
            ),
            package(
                "right",
                "1.0.0",
                &[("host", "1.2.0"), ("plug", "npm:plugin@1")],
            ),
            package("host", "1.1.0", &[]),
            package("host", "1.2.0", &[]),
            peer_plugin(false, "^1"),
        ],
        &[dep(PUBLIC, "left", "1"), dep(PUBLIC, "right", "1")],
    )
    .unwrap();
    let left = &graph.instances[&graph.roots[&(PUBLIC.parse().unwrap(), "left".parse().unwrap())]];
    let right =
        &graph.instances[&graph.roots[&(PUBLIC.parse().unwrap(), "right".parse().unwrap())]];
    let a = child(&graph, left, "plug");
    let b = child(&graph, right, "plug");
    assert_eq!(a.artifact, b.artifact);
    assert_ne!(
        left.dependencies[&"plug".parse().unwrap()],
        right.dependencies[&"plug".parse().unwrap()]
    );
    assert_eq!(
        a.peers[&"host".parse().unwrap()],
        left.dependencies[&"host".parse().unwrap()]
    );
    assert_eq!(
        b.peers[&"host".parse().unwrap()],
        right.dependencies[&"host".parse().unwrap()]
    );
    assert_ne!(a.peers, b.peers);
}
#[test]
fn equal_version_providers_remain_distinct_when_their_own_contexts_differ() {
    let mut host = package("host", "1.0.0", &[]);
    host.peer_dependencies
        .insert("engine".parse().unwrap(), req("^1"));
    let graph = resolve(
        vec![
            package(
                "left",
                "1.0.0",
                &[("engine", "1.1.0"), ("host", "1"), ("plugin", "1")],
            ),
            package(
                "right",
                "1.0.0",
                &[("engine", "1.2.0"), ("host", "1"), ("plugin", "1")],
            ),
            package("engine", "1.1.0", &[]),
            package("engine", "1.2.0", &[]),
            host,
            peer_plugin(false, "1"),
        ],
        &[dep(PUBLIC, "left", "1"), dep(PUBLIC, "right", "1")],
    )
    .unwrap();
    let parents: Vec<_> = graph
        .roots
        .values()
        .map(|reference| &graph.instances[reference])
        .collect();
    let providers: Vec<_> = parents
        .iter()
        .map(|parent| &child(&graph, parent, "plugin").peers[&"host".parse().unwrap()])
        .collect();
    assert_ne!(providers[0], providers[1]);
    assert_eq!(
        graph.instances[providers[0]].artifact,
        graph.instances[providers[1]].artifact
    );
    assert_ne!(
        graph.instances[providers[0]].peers,
        graph.instances[providers[1]].peers
    );
}
#[test]
fn nearest_incompatible_provider_shadows_compatible_root_even_for_optional_peers() {
    for optional in [false, true] {
        let error = resolve(
            vec![
                package("parent", "1.0.0", &[("host", "2"), ("plugin", "1")]),
                package("host", "1.0.0", &[]),
                package("host", "2.0.0", &[]),
                peer_plugin(optional, "^1"),
            ],
            &[dep(PUBLIC, "parent", "1"), dep(PUBLIC, "host", "1")],
        )
        .unwrap_err();
        assert!(
            matches!(error, ResolveError::ContextualPeerDependency(failure) if failure.provider.as_ref().is_some_and(|provider| provider.version.to_string() == "2.0.0"))
        );
    }
}
#[test]
fn private_ordinary_dependency_is_not_a_peer_provider() {
    let mut plugin = peer_plugin(false, "1");
    plugin
        .dependencies
        .insert("host".parse().unwrap(), req("1"));
    let error = resolve(
        vec![plugin, package("host", "1.0.0", &[])],
        &[dep(PUBLIC, "plugin", "1")],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ResolveError::ContextualPeerDependency(failure) if failure.provider.is_none()
    ));
}
#[test]
fn optional_absence_does_not_invent_a_provider() {
    let graph = resolve(vec![peer_plugin(true, "1")], &[dep(PUBLIC, "plugin", "1")]).unwrap();
    assert_eq!(graph.instances.len(), 1);
    assert!(graph.instances.values().next().unwrap().peers.is_empty());
}
#[test]
fn sibling_mutual_peers_use_finite_preallocated_references() {
    let mut a = package("a", "1.0.0", &[]);
    a.peer_dependencies.insert("b".parse().unwrap(), req("1"));
    let mut b = package("b", "1.0.0", &[]);
    b.peer_dependencies.insert("a".parse().unwrap(), req("1"));
    let graph = resolve(vec![a, b], &[dep(PUBLIC, "a", "1"), dep(PUBLIC, "b", "1")]).unwrap();
    assert_eq!(graph.instances.len(), 2);
    for instance in graph.instances.values() {
        let peer = &graph.instances[instance.peers.values().next().unwrap()];
        assert_eq!(
            graph.instances[peer.peers.values().next().unwrap()].artifact,
            instance.artifact
        );
    }
}
#[test]
fn peerless_dependency_cycle_reuses_an_ancestor_reference() {
    let graph = resolve(
        vec![
            package("a", "1.0.0", &[("b", "1")]),
            package("b", "1.0.0", &[("a", "1")]),
        ],
        &[dep(PUBLIC, "a", "1")],
    )
    .unwrap();
    assert_eq!(graph.instances.len(), 2);
    let root = graph.roots.values().next().unwrap();
    assert_eq!(
        child(&graph, &graph.instances[root], "b").dependencies[&"a".parse().unwrap()],
        *root
    );
}
#[test]
fn selected_graph_is_independent_of_input_order() {
    let mut packages = vec![
        package("parent", "1.0.0", &[("host", "1"), ("plugin", "1")]),
        package("host", "1.0.0", &[]),
        peer_plugin(false, "1"),
    ];
    let mut roots = vec![dep(PUBLIC, "parent", "1"), dep(PUBLIC, "host", "1")];
    let first = resolve(packages.clone(), &roots).unwrap();
    packages.reverse();
    roots.reverse();
    assert_eq!(first, resolve(packages, &roots).unwrap());
}
#[test]
fn source_qualified_roots_never_share_references() {
    let private = "https://private.example";
    let graph = resolve_instances_with_preferences(
        &[dep(PUBLIC, "host", "1"), dep(private, "host", "1")],
        &[
            registry(PUBLIC, vec![package("host", "1.0.0", &[])]),
            registry(private, vec![package("host", "1.0.0", &[])]),
        ],
        Default::default(),
        |source, _| Ok(source.clone()),
        &ResolutionPreferences::default(),
    )
    .unwrap();
    assert_eq!(graph.instances.len(), 2);
    let references: Vec<_> = graph.roots.values().collect();
    assert_ne!(references[0], references[1]);
    assert_ne!(
        graph.instances[references[0]].artifact.registry,
        graph.instances[references[1]].artifact.registry
    );
}
#[test]
fn explicit_alias_peer_checks_actual_target_and_route() {
    let private = "https://private.example";
    let graph = resolve_instances_with_preferences(
        &[dep(PUBLIC, "parent", "1")],
        &[
            registry(
                PUBLIC,
                vec![
                    package(
                        "parent",
                        "1.0.0",
                        &[("host", "npm:actual@1"), ("plugin", "1")],
                    ),
                    peer_plugin(false, "npm:actual@1"),
                ],
            ),
            registry(private, vec![package("actual", "1.0.0", &[])]),
        ],
        Default::default(),
        |source, name| {
            Ok(if name.as_str() == "actual" {
                private.parse().unwrap()
            } else {
                source.clone()
            })
        },
        &ResolutionPreferences::default(),
    )
    .unwrap();
    let parent = &graph.instances[graph.roots.values().next().unwrap()];
    let plugin = child(&graph, parent, "plugin");
    assert_eq!(
        graph.instances[&plugin.peers[&"host".parse().unwrap()]]
            .artifact
            .registry
            .as_str(),
        private
    );
}
#[test]
fn wrong_origin_visible_optional_provider_is_rejected() {
    let private = "https://private.example";
    let error = resolve_instances_with_preferences(
        &[dep(PUBLIC, "plugin", "1"), dep(private, "host", "1")],
        &[
            registry(PUBLIC, vec![peer_plugin(true, "1")]),
            registry(private, vec![package("host", "1.0.0", &[])]),
        ],
        Default::default(),
        |source, _| Ok(source.clone()),
        &ResolutionPreferences::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ResolveError::ContextualPeerDependency(failure) if failure.provider.is_some()
    ));
}

#[test]
fn explicit_alias_peer_rejects_a_different_actual_target_even_at_a_matching_version() {
    for optional in [false, true] {
        let error = resolve(
            vec![
                package(
                    "parent",
                    "1.0.0",
                    &[("host", "npm:wrong@1"), ("plugin", "1")],
                ),
                package("wrong", "1.0.0", &[]),
                peer_plugin(optional, "npm:actual@1"),
            ],
            &[dep(PUBLIC, "parent", "1")],
        )
        .unwrap_err();
        assert!(
            matches!(error, ResolveError::ContextualPeerDependency(failure)
            if failure.provider.as_ref().is_some_and(|id| id.name.as_str() == "wrong"))
        );
    }
}

#[test]
fn ordinary_peer_uses_a_local_alias_without_requiring_the_actual_name_to_match() {
    let graph = resolve(
        vec![
            package(
                "parent",
                "1.0.0",
                &[("host", "npm:actual@1"), ("plugin", "1")],
            ),
            package("actual", "1.0.0", &[]),
            peer_plugin(false, "1"),
        ],
        &[dep(PUBLIC, "parent", "1")],
    )
    .unwrap();
    let parent = &graph.instances[graph.roots.values().next().unwrap()];
    let provider = &child(&graph, parent, "plugin").peers[&"host".parse().unwrap()];
    assert_eq!(graph.instances[provider].artifact.name.as_str(), "actual");
    assert_eq!(*provider, parent.dependencies[&"host".parse().unwrap()]);
}

#[test]
fn cycle_analysis_has_a_separate_bounded_work_budget() {
    let metadata = [registry(
        PUBLIC,
        vec![
            package("a", "1.0.0", &[("b", "1")]),
            package("b", "1.0.0", &[("a", "1")]),
        ],
    )];
    let selected = select_graph(
        &[dep(PUBLIC, "a", "1")],
        &metadata,
        Default::default(),
        &mut |source: &PackageSource, _: &PackageName| Ok(source.clone()),
        &ResolutionPreferences::default(),
    )
    .unwrap();
    let error = expand(
        selected,
        &mut |source: &PackageSource, _: &PackageName| Ok(source.clone()),
        Limits {
            edges: 3,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ResolveError::UnsupportedMode("contextual cycle analysis exceeds work limit")
    ));
}
#[test]
fn graph_limits_reject_before_returning_a_partial_graph() {
    let metadata = [registry(
        PUBLIC,
        vec![
            package("parent", "1.0.0", &[("child", "1")]),
            package("child", "1.0.0", &[]),
        ],
    )];
    let roots = [dep(PUBLIC, "parent", "1")];
    let selected = || {
        select_graph(
            &roots,
            &metadata,
            Default::default(),
            &mut |source: &PackageSource, _: &PackageName| Ok(source.clone()),
            &ResolutionPreferences::default(),
        )
        .unwrap()
    };
    for limits in [
        Limits {
            instances: 1,
            ..Limits::default()
        },
        Limits {
            edges: 1,
            ..Limits::default()
        },
        Limits {
            depth: 1,
            ..Limits::default()
        },
        Limits {
            bytes: 1,
            ..Limits::default()
        },
    ] {
        assert!(matches!(
            expand(
                selected(),
                &mut |source: &PackageSource, _: &PackageName| Ok(source.clone()),
                limits
            ),
            Err(ResolveError::UnsupportedMode(_))
        ));
    }
    assert!(
        expand(
            selected(),
            &mut |source: &PackageSource, _: &PackageName| Ok(source.clone()),
            Limits::default()
        )
        .is_ok()
    );
}
#[test]
fn incompatible_provider_diagnostic_names_the_routed_visible_identity() {
    let private = "https://z-private.example";
    let error = resolve_instances_with_preferences(
        &[
            dep(PUBLIC, "plugin", "1"),
            dep(PUBLIC, "host", "1"),
            dep(private, "host", "2"),
        ],
        &[
            registry(
                PUBLIC,
                vec![peer_plugin(false, "1"), package("host", "1.0.0", &[])],
            ),
            registry(private, vec![package("host", "2.0.0", &[])]),
        ],
        Default::default(),
        |source, name| {
            Ok(if name.as_str() == "host" {
                private.parse().unwrap()
            } else {
                source.clone()
            })
        },
        &ResolutionPreferences::default(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ResolveError::ContextualPeerDependency(failure) if failure.provider.as_ref().is_some_and(|provider| provider.registry.as_str() == private && provider.version.to_string() == "2.0.0"))
    );
}

#[test]
fn incompatible_alias_diagnostic_retains_the_selected_candidates_route() {
    let private = "https://z-private.example";
    let error = resolve_instances_with_preferences(
        &[
            dep(PUBLIC, "plugin", "1"),
            dep(PUBLIC, "host", "npm:actual@2"),
            dep(private, "host", "npm:other@1"),
        ],
        &[
            registry(
                PUBLIC,
                vec![peer_plugin(false, "1"), package("actual", "2.0.0", &[])],
            ),
            registry(private, vec![package("other", "1.0.0", &[])]),
        ],
        Default::default(),
        |_, name| {
            Ok(if name.as_str() == "other" {
                "https://another.example"
            } else {
                PUBLIC
            }
            .parse()
            .unwrap())
        },
        &ResolutionPreferences::default(),
    )
    .unwrap_err();
    let ResolveError::ContextualPeerDependency(failure) = error else {
        panic!("expected a contextual peer failure")
    };
    assert_eq!(failure.expected_source.as_str(), PUBLIC);
    assert_eq!(failure.provider.unwrap().name.as_str(), "actual");
}

#[test]
fn identical_consumer_artifacts_bind_distinct_aliased_registry_providers() {
    let private = "https://private.example";
    let graph = resolve_instances_with_preferences(
        &[dep(PUBLIC, "left", "1"), dep(PUBLIC, "right", "1")],
        &[
            registry(
                PUBLIC,
                vec![
                    package(
                        "left",
                        "1.0.0",
                        &[("host", "npm:public-host@1"), ("plugin", "1")],
                    ),
                    package(
                        "right",
                        "1.0.0",
                        &[("host", "npm:private-host@1"), ("plugin", "1")],
                    ),
                    package("public-host", "1.0.0", &[]),
                    peer_plugin(false, "1"),
                ],
            ),
            registry(private, vec![package("private-host", "1.0.0", &[])]),
        ],
        Default::default(),
        |_, name| {
            Ok(if name.as_str() == "private-host" {
                private
            } else {
                PUBLIC
            }
            .parse()
            .unwrap())
        },
        &ResolutionPreferences::default(),
    )
    .unwrap();
    let left = &graph.instances[&graph.roots[&(PUBLIC.parse().unwrap(), "left".parse().unwrap())]];
    let right =
        &graph.instances[&graph.roots[&(PUBLIC.parse().unwrap(), "right".parse().unwrap())]];
    let a = child(&graph, left, "plugin");
    let b = child(&graph, right, "plugin");
    assert_eq!(a.artifact, b.artifact);
    assert_eq!(
        graph.instances[&a.peers[&"host".parse().unwrap()]]
            .artifact
            .registry
            .as_str(),
        PUBLIC
    );
    assert_eq!(
        graph.instances[&b.peers[&"host".parse().unwrap()]]
            .artifact
            .registry
            .as_str(),
        private
    );
}
