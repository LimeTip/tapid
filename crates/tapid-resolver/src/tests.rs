use super::*;
use proptest::prelude::*;
fn req(s: &str) -> Requirement {
    s.parse().unwrap()
}

#[test]
fn npm_aliases_validate_actual_names_and_ranges() {
    let local: PackageName = "local".parse().unwrap();
    for (spec, target) in [
        ("npm:h3@2.0.1-rc.20", "h3"),
        ("npm:@scope/pkg@^1", "@scope/pkg"),
        ("npm:@scope/pkg", "@scope/pkg"),
    ] {
        let requirement = req(spec);
        assert_eq!(requirement.raw, spec);
        assert_eq!(requirement.package_name(&local).as_str(), target);
        assert!(requirement.is_alias());
    }
    assert!(req("npm:h3@2.0.1-rc.20").matches(&"2.0.1-rc.20".parse().unwrap()));
    assert!(!req("npm:h3@2.0.1-rc.20").matches(&"2.0.1".parse().unwrap()));
    for spec in [
        "npm:",
        "npm:pkg@",
        "npm:pkg@latest",
        "npm:../pkg@1",
        "npm:@scope@1",
        "npm:pkg@npm:other@1",
        "npm:https://example.test/pkg@1",
        "npm:pkg@workspace:*",
    ] {
        assert!(spec.parse::<Requirement>().is_err(), "{spec}");
    }
}

#[test]
fn aliases_select_distinct_root_versions_and_route_transitives_by_actual_name() {
    let public = "https://registry.npmjs.org";
    let private = "https://packages.example";
    let metadata = vec![
        registry(
            public,
            vec![
                package("h3", "1.0.0", &[]),
                package("h3", "2.0.0", &[]),
                package("parent", "1.0.0", &[("local", "npm:@actual/pkg@^1")]),
            ],
        ),
        registry(private, vec![package("@actual/pkg", "1.2.0", &[])]),
    ];
    let resolution = resolve_graph_with_routing(
        &[
            dep(public, "first", "npm:h3@1"),
            dep(public, "second", "npm:h3@2"),
            dep(public, "parent", "1"),
        ],
        &metadata,
        Default::default(),
        |parent, name| {
            Ok(if name.as_str() == "@actual/pkg" {
                private.parse().unwrap()
            } else {
                parent.clone()
            })
        },
    )
    .unwrap();
    assert_eq!(resolution.selected.len(), 4);
    let bindings = resolution
        .root_bindings
        .iter()
        .map(|((_, name), id)| (name.as_str(), id.version.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(bindings["first"], "1.0.0");
    assert_eq!(bindings["second"], "2.0.0");
    let edge = &resolution.dependencies[0];
    assert_eq!(edge.dependency.as_str(), "local");
    assert_eq!(edge.child.name.as_str(), "@actual/pkg");
    assert_eq!(edge.child.registry.as_str(), private);
}

#[test]
fn validated_requirement_reuses_its_parsed_form_when_matching_candidates() {
    reset_requirement_base_parse_count();
    let requirement = req("^1.2.3");
    let parses_after_validation = requirement_base_parse_count();
    assert!(parses_after_validation > 0);

    for version in ["1.2.3", "1.9.0", "2.0.0"] {
        assert_eq!(
            requirement.matches(&version.parse().unwrap()),
            version != "2.0.0"
        );
    }

    assert_eq!(
        requirement_base_parse_count(),
        parses_after_validation,
        "candidate matching must not repeatedly parse the validated requirement"
    );
}

proptest! {
    #[test]
    fn generated_exact_requirements_trim_and_match_their_version(
        major in 0u64..1000, minor in 0u64..1000, patch in 0u64..1000,
    ) {
        let version_text = format!("{major}.{minor}.{patch}");
        let version: PackageVersion = version_text.parse().unwrap();
        let requirement: Requirement = format!("  ={version_text}  ").parse().unwrap();
        prop_assert_eq!(&requirement.raw, &format!("={version_text}"));
        prop_assert!(requirement.matches(&version));
    }
}

fn dep(registry: &str, name: &str, range: &str) -> Dependency {
    Dependency::new(registry.parse().unwrap(), name.parse().unwrap(), req(range))
}
fn package(name: &str, version: &str, dependencies: &[(&str, &str)]) -> PackageVersionMetadata {
    PackageVersionMetadata {
        name: name.parse().unwrap(),
        version: version.parse().unwrap(),
        dependencies: dependencies
            .iter()
            .map(|(n, r)| (n.parse().unwrap(), req(r)))
            .collect(),
        peer_dependencies: BTreeMap::new(),
        optional_peer_dependencies: BTreeSet::new(),
    }
}
fn registry(url: &str, packages: Vec<PackageVersionMetadata>) -> RegistryMetadata {
    RegistryMetadata::normalize(url.parse().unwrap(), packages).unwrap()
}

#[test]
fn exact_prerelease_selects_only_the_matching_candidate() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "2.0.0-rc.23", &[]),
            package("foo", "2.0.0-rc.24", &[]),
            package("foo", "2.0.0", &[]),
        ],
    );
    let r = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "2.0.0-rc.24")],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        r.selected[0].to_string(),
        "https://registry.npmjs.org:foo@2.0.0-rc.24"
    );
}

#[test]
fn prerelease_caret_selects_matching_prereleases_and_stable_release() {
    let prerelease_only = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "2.0.0-next.4", &[]),
            package("foo", "2.0.0-next.6", &[]),
            package("foo", "2.1.0-next.1", &[]),
        ],
    );
    let selected = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^2.0.0-next.5")],
        &[prerelease_only],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        selected.selected[0].to_string(),
        "https://registry.npmjs.org:foo@2.0.0-next.6"
    );

    let with_stable = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "2.0.0-next.6", &[]),
            package("foo", "2.0.0", &[]),
        ],
    );
    let selected = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^2.0.0-next.5")],
        &[with_stable],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        selected.selected[0].to_string(),
        "https://registry.npmjs.org:foo@2.0.0"
    );
}

#[test]
fn stable_ranges_do_not_select_prerelease_candidates() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![package("foo", "2.0.0-rc.24", &[])],
    );
    let error = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "*")],
        &[m],
        Default::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ResolveError::MissingCandidate { .. }));
}

#[test]
fn npm_or_ranges_select_the_highest_matching_alternative() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "2.4.1", &[]),
            package("foo", "2.9.0", &[]),
            package("foo", "3.1.0", &[]),
            package("foo", "4.0.0", &[]),
        ],
    );
    let selected = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^2.4.1 || ^3.0.0")],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        selected.selected[0].to_string(),
        "https://registry.npmjs.org:foo@3.1.0"
    );
}

#[test]
fn malformed_or_ranges_are_rejected() {
    for requirement in ["|| ^1.0.0", "^1.0.0 ||", "^1.0.0 || || ^2.0.0"] {
        assert!(matches!(
            requirement.parse::<Requirement>(),
            Err(ResolveError::UnsupportedRange(_))
        ));
    }
}

#[test]
fn npm_wildcard_and_hyphen_ranges_match_bounds() {
    let any: Requirement = "x".parse().unwrap();
    assert!(any.matches(&"1.2.3".parse().unwrap()));
    assert!(!any.matches(&"1.2.3-beta.1".parse().unwrap()));

    let hyphen: Requirement = "1.2.0 - 2.0.0".parse().unwrap();
    assert!(!hyphen.matches(&"1.1.9".parse().unwrap()));
    assert!(hyphen.matches(&"1.2.0".parse().unwrap()));
    assert!(hyphen.matches(&"2.0.0".parse().unwrap()));
    assert!(!hyphen.matches(&"2.0.1".parse().unwrap()));
}

#[test]
fn npm_x_ranges_match_major_minor_and_all_wildcards() {
    for (range, yes, no) in [
        ("1.x", "1.9.0", "2.0.0"),
        ("1.2.*", "1.2.9", "1.3.0"),
        ("*.*", "9.0.0", "1.0.0-beta.1"),
    ] {
        let requirement: Requirement = range.parse().unwrap();
        assert!(requirement.matches(&yes.parse().unwrap()), "{range}");
        assert!(!requirement.matches(&no.parse().unwrap()), "{range}");
    }
}

#[test]
fn exact_and_caret_are_deterministic() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "1.1.0", &[]),
            package("foo", "1.9.0", &[]),
            package("foo", "2.0.0", &[]),
        ],
    );
    let r = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^1.0.0")],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        r.selected[0].to_string(),
        "https://registry.npmjs.org:foo@1.9.0"
    );
}

#[test]
fn npm_comparison_intersections_support_spaced_and_compact_operators() {
    for text in [">= 2.1.2 < 3", ">=2.1.2 <3"] {
        let requirement: Requirement = text.parse().unwrap();
        assert!(!requirement.matches(&"2.1.1".parse().unwrap()), "{text}");
        assert!(requirement.matches(&"2.1.2".parse().unwrap()), "{text}");
        assert!(requirement.matches(&"2.9.9".parse().unwrap()), "{text}");
        assert!(!requirement.matches(&"3.0.0".parse().unwrap()), "{text}");
        assert!(
            !requirement.matches(&"2.2.0-beta.1".parse().unwrap()),
            "{text}"
        );
    }
}

#[test]
fn partial_comparison_bounds_follow_npm_x_range_semantics() {
    for (text, matching, rejected) in [
        (">2", "3.0.0", "2.9.9"),
        ("<=2", "2.9.9", "3.0.0"),
        (">2.1", "2.2.0", "2.1.99"),
        ("<=2.1", "2.1.99", "2.2.0"),
    ] {
        let requirement: Requirement = text.parse().unwrap();
        assert!(requirement.matches(&matching.parse().unwrap()), "{text}");
        assert!(!requirement.matches(&rejected.parse().unwrap()), "{text}");
    }

    for text in ["<3 3.0.0-beta.1", "<=2 3.0.0-beta.1"] {
        let requirement: Requirement = text.parse().unwrap();
        assert!(
            !requirement.matches(&"3.0.0-beta.1".parse().unwrap()),
            "{text} must preserve npm's -0 partial upper bound"
        );
    }
}

#[test]
fn partial_range_intersection_allows_explicit_matching_prerelease() {
    let below_major_floor: Requirement = "2 2.0.0-beta.1".parse().unwrap();
    assert!(!below_major_floor.matches(&"2.0.0-beta.1".parse().unwrap()));

    let major: Requirement = "2 2.1.0-beta.1".parse().unwrap();
    assert!(major.matches(&"2.1.0-beta.1".parse().unwrap()));
    assert!(!major.matches(&"2.1.0-beta.2".parse().unwrap()));

    let below_minor_floor: Requirement = "2.1 2.1.0-beta.1".parse().unwrap();
    assert!(!below_minor_floor.matches(&"2.1.0-beta.1".parse().unwrap()));

    let minor: Requirement = "2.1 2.1.1-beta.1".parse().unwrap();
    assert!(minor.matches(&"2.1.1-beta.1".parse().unwrap()));
    assert!(!minor.matches(&"2.1.2-beta.1".parse().unwrap()));
}

#[test]
fn bare_major_range_selects_highest_matching_major() {
    let requirement = req("2");
    assert!(!requirement.matches(&"2.0.0-beta.1".parse().unwrap()));
    assert!(requirement.matches(&"2.0.0".parse().unwrap()));
    assert!(requirement.matches(&"2.9.9".parse().unwrap()));
    assert!(!requirement.matches(&"3.0.0".parse().unwrap()));
}

#[test]
fn bare_minor_range_selects_highest_matching_minor() {
    let requirement = req("2.1");
    assert!(!requirement.matches(&"2.1.0-beta.1".parse().unwrap()));
    assert!(requirement.matches(&"2.1.0".parse().unwrap()));
    assert!(requirement.matches(&"2.1.9".parse().unwrap()));
    assert!(!requirement.matches(&"2.2.0".parse().unwrap()));
}

#[test]
fn partial_tilde_ranges_follow_npm_semantics() {
    for text in ["~2", "~ 2"] {
        let requirement: Requirement = text.parse().unwrap();
        assert!(
            !requirement.matches(&"2.0.0-beta.1".parse().unwrap()),
            "{text}"
        );
        assert!(requirement.matches(&"2.0.0".parse().unwrap()), "{text}");
        assert!(requirement.matches(&"2.9.9".parse().unwrap()), "{text}");
        assert!(!requirement.matches(&"3.0.0".parse().unwrap()), "{text}");
    }

    for text in ["~2.1", "~ 2.1"] {
        let requirement: Requirement = text.parse().unwrap();
        assert!(
            !requirement.matches(&"2.1.0-beta.1".parse().unwrap()),
            "{text}"
        );
        assert!(requirement.matches(&"2.1.0".parse().unwrap()), "{text}");
        assert!(requirement.matches(&"2.1.99".parse().unwrap()), "{text}");
        assert!(!requirement.matches(&"2.2.0".parse().unwrap()), "{text}");
    }

    let explicit_prerelease: Requirement = "~2 2.1.0-beta.1".parse().unwrap();
    assert!(explicit_prerelease.matches(&"2.1.0-beta.1".parse().unwrap()));
    assert!(!explicit_prerelease.matches(&"2.1.0-beta.2".parse().unwrap()));
}

#[test]
fn partial_ranges_reject_noncanonical_components() {
    for requirement in ["02", "2.01", "2.", ".2", "2.1.0.0"] {
        assert!(matches!(
            requirement.parse::<Requirement>(),
            Err(ResolveError::UnsupportedRange(_))
        ));
    }
}

#[test]
fn major_only_caret_range_selects_highest_matching_major() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "3.0.0", &[]),
            package("foo", "3.9.0", &[]),
            package("foo", "4.0.0", &[]),
        ],
    );

    let r = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^3")],
        &[m],
        Default::default(),
    )
    .unwrap();

    assert_eq!(
        r.selected[0].to_string(),
        "https://registry.npmjs.org:foo@3.9.0"
    );
}

#[test]
fn zero_major_only_caret_range_uses_next_major_as_upper_bound() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "0.0.1", &[]),
            package("foo", "0.9.0", &[]),
            package("foo", "1.0.0", &[]),
        ],
    );

    let r = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^0")],
        &[m],
        Default::default(),
    )
    .unwrap();

    assert_eq!(
        r.selected[0].to_string(),
        "https://registry.npmjs.org:foo@0.9.0"
    );
}

#[test]
fn resolves_peer_requirements_from_root_providers_without_installing_peer_as_root() {
    let mut plugin = package("plugin", "1.0.0", &[]);
    plugin.peer_dependencies = BTreeMap::from([("react".parse().unwrap(), req("^18.0.0"))]);
    let metadata = registry(
        "https://registry.npmjs.org",
        vec![plugin, package("react", "18.2.0", &[])],
    );
    let result = resolve_graph(
        &[
            dep("https://registry.npmjs.org", "plugin", "1.0.0"),
            dep("https://registry.npmjs.org", "react", "^18.0.0"),
        ],
        &[metadata],
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.roots.len(), 2);
    assert_eq!(result.selected.len(), 2);
    let plugin_id = RegistryPackageId::new(
        "https://registry.npmjs.org".parse().unwrap(),
        "plugin".parse().unwrap(),
        "1.0.0".parse().unwrap(),
    );
    let expected_context = tapid_core::PeerContext::default()
        .with("react".parse().unwrap(), "18.2.0".parse().unwrap());
    assert_eq!(
        result.peer_contexts.get(&plugin_id),
        Some(&expected_context)
    );

    let mut missing = package("plugin", "1.0.0", &[]);
    missing.peer_dependencies = BTreeMap::from([("react".parse().unwrap(), req("^18.0.0"))]);
    let metadata = registry("https://registry.npmjs.org", vec![missing]);
    let error = resolve_graph(
        &[dep("https://registry.npmjs.org", "plugin", "1.0.0")],
        &[metadata],
        Default::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ResolveError::PeerDependency { .. }));
    assert!(error.to_string().contains("peer dependency unresolved"));
}

#[test]
fn optional_peer_dependency_allows_missing_direct_provider() {
    let mut plugin = package("plugin", "1.0.0", &[]);
    let peer: PackageName = "host".parse().unwrap();
    plugin.peer_dependencies = BTreeMap::from([(peer.clone(), req("^2.0.0"))]);
    plugin.optional_peer_dependencies = BTreeSet::from([peer]);
    let metadata = registry("https://registry.npmjs.org", vec![plugin]);
    let result = resolve_graph(
        &[dep("https://registry.npmjs.org", "plugin", "1.0.0")],
        &[metadata],
        Default::default(),
    )
    .unwrap();
    let plugin_id = RegistryPackageId::new(
        "https://registry.npmjs.org".parse().unwrap(),
        "plugin".parse().unwrap(),
        "1.0.0".parse().unwrap(),
    );
    assert_eq!(
        result.peer_contexts.get(&plugin_id),
        Some(&PeerContext::default())
    );
}

#[test]
fn optional_peer_dependency_binds_a_compatible_direct_provider() {
    let mut plugin = package("plugin", "1.0.0", &[]);
    let peer: PackageName = "host".parse().unwrap();
    plugin.peer_dependencies = BTreeMap::from([(peer.clone(), req("^2.0.0"))]);
    plugin.optional_peer_dependencies = BTreeSet::from([peer]);
    let metadata = registry(
        "https://registry.npmjs.org",
        vec![plugin, package("host", "2.4.0", &[])],
    );
    let result = resolve_graph(
        &[
            dep("https://registry.npmjs.org", "plugin", "1.0.0"),
            dep("https://registry.npmjs.org", "host", "^2.0.0"),
        ],
        &[metadata],
        Default::default(),
    )
    .unwrap();
    let plugin_id = RegistryPackageId::new(
        "https://registry.npmjs.org".parse().unwrap(),
        "plugin".parse().unwrap(),
        "1.0.0".parse().unwrap(),
    );
    let expected_context =
        PeerContext::default().with("host".parse().unwrap(), "2.4.0".parse().unwrap());
    assert_eq!(
        result.peer_contexts.get(&plugin_id),
        Some(&expected_context)
    );
}

#[test]
fn optional_peer_dependency_rejects_an_incompatible_direct_provider() {
    let mut plugin = package("plugin", "1.0.0", &[]);
    let peer: PackageName = "host".parse().unwrap();
    plugin.peer_dependencies = BTreeMap::from([(peer.clone(), req("^2.0.0"))]);
    plugin.optional_peer_dependencies = BTreeSet::from([peer]);
    let metadata = registry(
        "https://registry.npmjs.org",
        vec![plugin, package("host", "3.0.0", &[])],
    );
    let error = resolve_graph(
        &[
            dep("https://registry.npmjs.org", "plugin", "1.0.0"),
            dep("https://registry.npmjs.org", "host", "3.0.0"),
        ],
        &[metadata],
        Default::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ResolveError::PeerDependency { .. }));
}

#[test]
fn preserves_peer_requirements_separately_from_ordinary_dependencies() {
    let peer: PackageVersionMetadata = PackageVersionMetadata {
        name: "plugin".parse().unwrap(),
        version: "1.0.0".parse().unwrap(),
        dependencies: BTreeMap::from([("runtime".parse().unwrap(), req("^1.0.0"))]),
        peer_dependencies: BTreeMap::from([("react".parse().unwrap(), req("^18.0.0"))]),
        optional_peer_dependencies: BTreeSet::new(),
    };

    assert!(peer.dependencies.contains_key(&"runtime".parse().unwrap()));
    assert!(!peer.dependencies.contains_key(&"react".parse().unwrap()));
    assert_eq!(
        peer.peer_dependencies[&"react".parse().unwrap()].raw,
        "^18.0.0"
    );
}

#[test]
fn available_versions_use_semver_order_before_rendering() {
    let first = PackageVersionMetadata {
        name: "pkg".parse().unwrap(),
        version: "10.0.0".parse().unwrap(),
        dependencies: BTreeMap::new(),
        peer_dependencies: BTreeMap::new(),
        optional_peer_dependencies: BTreeSet::new(),
    };
    let second = PackageVersionMetadata {
        name: "pkg".parse().unwrap(),
        version: "2.0.0".parse().unwrap(),
        dependencies: BTreeMap::new(),
        peer_dependencies: BTreeMap::new(),
        optional_peer_dependencies: BTreeSet::new(),
    };

    assert_eq!(available(&[&first, &second]), vec!["2.0.0", "10.0.0"]);
}

#[test]
fn missing_metadata_is_reported_as_a_sorted_frontier() {
    let registry: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
    let app: PackageName = "app".parse().unwrap();
    let metadata = RegistryMetadata::normalize(
        registry.clone(),
        vec![PackageVersionMetadata {
            name: app.clone(),
            version: "1.0.0".parse().unwrap(),
            dependencies: BTreeMap::from([
                ("z-child".parse().unwrap(), req("1.0.0")),
                ("a-child".parse().unwrap(), req("1.0.0")),
            ]),
            peer_dependencies: BTreeMap::new(),
            optional_peer_dependencies: BTreeSet::new(),
        }],
    )
    .unwrap();

    let error = resolve_graph(
        &[Dependency::new(registry, app, req("1.0.0"))],
        &[metadata],
        ResolutionOptions::default(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        ResolveError::MissingMetadata {
            packages: vec![
                (
                    "https://registry.npmjs.org".to_owned(),
                    "a-child".to_owned()
                ),
                (
                    "https://registry.npmjs.org".to_owned(),
                    "z-child".to_owned()
                ),
            ],
        }
    );
}

#[test]
fn major_only_caret_range_rejects_leading_zeroes() {
    assert!(matches!(
        "^03".parse::<Requirement>(),
        Err(ResolveError::UnsupportedRange(_))
    ));
}

#[test]
fn npm_and_jsr_registries_remain_distinct() {
    let npm = registry(
        "https://registry.npmjs.org",
        vec![package("foo", "1.0.0", &[])],
    );
    let jsr = registry("https://jsr.io", vec![package("foo", "1.0.0", &[])]);
    let result = resolve_graph(
        &[
            dep("https://registry.npmjs.org", "foo", "1.0.0"),
            dep("https://jsr.io", "foo", "1.0.0"),
        ],
        &[npm, jsr],
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.selected[0].registry.to_string(), "https://jsr.io");
    assert_eq!(
        result.selected[1].registry.to_string(),
        "https://registry.npmjs.org"
    );
}

#[test]
fn shuffled_metadata_normalizes_and_tilde_is_supported() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "1.2.1", &[]),
            package("foo", "1.2.9", &[]),
            package("foo", "1.3.0", &[]),
        ],
    );
    let result = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "~1.2.0")],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.selected[0].version.to_string(), "1.2.9");
}

#[test]
fn transitive_dependencies_and_cycles_are_finite_and_sorted() {
    let m = registry(
        "https://jsr.io",
        vec![
            package("a", "1.0.0", &[("b", "1.0.0")]),
            package("b", "1.0.0", &[("a", "1.0.0")]),
        ],
    );
    let result = resolve_graph(
        &[dep("https://jsr.io", "a", "1.0.0")],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .selected
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["https://jsr.io:a@1.0.0", "https://jsr.io:b@1.0.0"]
    );
}

#[test]
fn different_parents_can_select_different_versions_of_one_dependency() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("a", "1.0.0", &[("debug", "^3.0.0")]),
            package("b", "1.0.0", &[("debug", "^4.0.0")]),
            package("debug", "3.2.7", &[]),
            package("debug", "4.3.7", &[]),
        ],
    );

    let result = resolve_graph(
        &[
            dep("https://registry.npmjs.org", "a", "*"),
            dep("https://registry.npmjs.org", "b", "*"),
        ],
        &[m],
        Default::default(),
    )
    .unwrap();

    assert_eq!(
        result
            .selected
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![
            "https://registry.npmjs.org:a@1.0.0",
            "https://registry.npmjs.org:b@1.0.0",
            "https://registry.npmjs.org:debug@3.2.7",
            "https://registry.npmjs.org:debug@4.3.7",
        ]
    );
}

#[test]
fn incompatible_constraints_are_structured_and_deterministic() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![package("foo", "1.0.0", &[]), package("foo", "2.0.0", &[])],
    );
    let result = resolve_graph(
        &[
            dep("https://registry.npmjs.org", "foo", "^1.0.0"),
            dep("https://registry.npmjs.org", "foo", "^2.0.0"),
        ],
        &[m],
        Default::default(),
    );
    assert!(
        matches!(result, Err(ResolveError::Conflict { requirements, .. }) if requirements == vec!["^1.0.0", "^2.0.0"])
    );
}

#[test]
fn npm_zero_major_caret_bounds_are_respected() {
    let m = registry(
        "https://registry.npmjs.org",
        vec![
            package("foo", "0.2.3", &[]),
            package("foo", "0.2.9", &[]),
            package("foo", "0.3.0", &[]),
        ],
    );
    let result = resolve_graph(
        &[dep("https://registry.npmjs.org", "foo", "^0.2.3")],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.selected[0].version.to_string(), "0.2.9");
}

#[test]
fn caret_ranges_at_integer_bounds_fail_closed_without_panicking() {
    let max = u64::MAX;
    let m = registry(
        "https://registry.npmjs.org",
        vec![package("foo", &format!("{max}.0.0"), &[])],
    );
    let result = resolve_graph(
        &[dep(
            "https://registry.npmjs.org",
            "foo",
            &format!("^{max}.0.0"),
        )],
        &[m],
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.selected[0].version.to_string(), format!("{max}.0.0"));
}

#[test]
fn zero_major_caret_ranges_at_integer_bounds_fail_closed_without_panicking() {
    let max = u64::MAX;
    for (version, requirement) in [
        (format!("0.{max}.0"), format!("^0.{max}.0")),
        (format!("0.0.{max}"), format!("^0.0.{max}")),
    ] {
        let m = registry(
            "https://registry.npmjs.org",
            vec![
                package("foo", &version, &[]),
                package(
                    "foo",
                    if requirement.starts_with("^0.") && requirement.contains(&format!(".{max}.")) {
                        "1.0.0"
                    } else {
                        "0.1.0"
                    },
                    &[],
                ),
            ],
        );
        let result = resolve_graph(
            &[dep("https://registry.npmjs.org", "foo", &requirement)],
            &[m],
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.selected[0].version.to_string(), version);
    }
}

#[test]
fn unsupported_ranges_and_modes_fail_closed() {
    assert!(matches!(
        "!=1.0.0".parse::<Requirement>(),
        Err(ResolveError::UnsupportedRange(_))
    ));
    assert!(matches!(
        resolve_graph(
            &[],
            &[],
            ResolutionOptions {
                offline: true,
                frozen: false
            }
        ),
        Err(ResolveError::UnsupportedMode(_))
    ));
    assert!(matches!(
        resolve_graph(
            &[],
            &[],
            ResolutionOptions {
                offline: false,
                frozen: true
            }
        ),
        Err(ResolveError::UnsupportedMode(_))
    ));
}
