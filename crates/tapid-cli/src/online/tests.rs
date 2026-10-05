use super::*;

#[test]
fn optional_alias_fetches_the_actual_package_and_retains_the_local_edge() {
    let mut parent = named_record("parent", "1.0.0", &[]);
    parent
        .optional_dependencies
        .insert("h3-v2".into(), "npm:h3@2.0.1-rc.20".into());
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "parent".parse().unwrap(),
        "1".parse().unwrap(),
    )];
    let mut fetched = Vec::new();
    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        fetched.push(name.to_string());
        match name.as_str() {
            "parent" => Ok(vec![parent.clone()]),
            "h3" => Ok(vec![named_record("h3", "2.0.1-rc.20", &[])]),
            _ => panic!("must fetch actual alias identity: {name}"),
        }
    })
    .unwrap();
    assert_eq!(fetched, ["parent", "h3"]);
    assert_eq!(resolution.dependencies[0].dependency.as_str(), "h3-v2");
    assert_eq!(resolution.dependencies[0].child.name.as_str(), "h3");
}

#[test]
fn jsr_alias_metadata_never_falls_back_to_a_jsr_package_of_the_same_name() {
    let mut package = named_record("@scope/parent", "1.0.0", &[("local", "npm:@actual/pkg@1")]);
    package.registry = JSR.parse().unwrap();
    let error = normalize_record(&package)
        .err()
        .expect("npm source cannot be interpreted as JSR");
    assert!(error.contains("refusing JSR registry fallback"));
}

#[test]
fn overriding_an_alias_range_preserves_its_actual_identity() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "parent".parse().unwrap(),
        "1".parse().unwrap(),
    )];
    let overrides = BTreeMap::from([("local".parse().unwrap(), "2".parse().unwrap())]);
    let (resolution, _) =
        resolve_with_overrides(&roots, &overrides, |_, name| match name.as_str() {
            "parent" => Ok(vec![named_record(
                "parent",
                "1.0.0",
                &[("local", "npm:actual@1")],
            )]),
            "actual" => Ok(vec![
                named_record("actual", "1.0.0", &[]),
                named_record("actual", "2.0.0", &[]),
            ]),
            _ => panic!("unexpected alias target {name}"),
        })
        .unwrap();
    assert_eq!(resolution.dependencies[0].dependency.as_str(), "local");
    assert_eq!(resolution.dependencies[0].child.name.as_str(), "actual");
    assert_eq!(
        resolution.dependencies[0].child.version.to_string(),
        "2.0.0"
    );
}
#[test]
fn custom_private_origins_use_the_npm_metadata_protocol() {
    let registry: RegistryOrigin = "https://127.0.0.1:9".parse().unwrap();
    let transport =
        HttpsTransport::authenticated_metadata([registry.to_string()], std::iter::empty()).unwrap();
    let config = crate::registry::RegistryConfig::from_toml(
        "[registries.default]\nurl='https://127.0.0.1:9'\n",
    )
    .unwrap();
    let error = remote_records(
        &transport,
        &config,
        &registry,
        &"private-package".parse().unwrap(),
        false,
    )
    .err()
    .unwrap();
    assert!(
        error.to_string().contains("cannot fetch metadata"),
        "{error}"
    );
    assert!(
        !error.to_string().contains("unsupported registry origin"),
        "{error}"
    );
}

#[test]
fn private_package_resolves_unscoped_transitive_dependency_from_public_npm() {
    let config = crate::registry::RegistryConfig::from_toml(
        r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
    )
    .unwrap();
    let (private_origin, package_name) = config.identity_for_spec("@acme/widget").unwrap();
    let root = Dependency::new(private_origin.clone(), package_name, "*".parse().unwrap());
    let mut private_package = named_record("@acme/widget", "1.0.0", &[("left-pad", "^1")]);
    private_package.registry = private_origin.clone();
    let public_origin: RegistryOrigin = NPM.parse().unwrap();
    let mut requests = Vec::new();

    let result = resolve_with_fetch_routed(
        &[root],
        |parent, dependency| config.registry_for_dependency(parent, dependency),
        |registry, name| {
            requests.push((registry.to_string(), name.to_string()));
            match name.to_string().as_str() {
                "@acme/widget" => Ok(vec![private_package.clone()]),
                "left-pad" => Ok(vec![named_record("left-pad", "1.3.0", &[])]),
                other => panic!("unexpected metadata request for {other}"),
            }
        },
    );

    let (resolution, _) = match result {
        Ok(resolution) => resolution,
        Err(error) => panic!("mixed-origin resolution failed: {error}"),
    };
    let child = resolution
        .selected
        .iter()
        .find(|package| package.name.to_string() == "left-pad")
        .expect("public transitive dependency must be selected");
    assert_eq!(child.registry, public_origin);
    assert!(requests.contains(&(NPM.to_owned(), "left-pad".to_owned())));
}

#[test]
fn public_package_resolves_scoped_transitive_dependency_from_private_registry() {
    let config = crate::registry::RegistryConfig::from_toml(
        r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
    )
    .unwrap();
    let private_origin = config.origin_for("@acme/helper").unwrap();
    let root = Dependency::new(
        NPM.parse().unwrap(),
        "public-app".parse().unwrap(),
        "*".parse().unwrap(),
    );
    let public_package = named_record("public-app", "1.0.0", &[("@acme/helper", "^1")]);
    let mut private_package = named_record("@acme/helper", "1.2.0", &[]);
    private_package.registry = private_origin.clone();
    let mut requests = Vec::new();

    let result = resolve_with_fetch_routed(
        &[root],
        |parent, dependency| config.registry_for_dependency(parent, dependency),
        |registry, name| {
            requests.push((registry.to_string(), name.to_string()));
            match name.to_string().as_str() {
                "public-app" => Ok(vec![public_package.clone()]),
                "@acme/helper" => Ok(vec![private_package.clone()]),
                other => panic!("unexpected metadata request for {other}"),
            }
        },
    );

    let (resolution, _) = match result {
        Ok(resolution) => resolution,
        Err(error) => panic!("mixed-origin resolution failed: {error}"),
    };
    let child = resolution
        .selected
        .iter()
        .find(|package| package.name.to_string() == "@acme/helper")
        .expect("private scoped dependency must be selected");
    assert_eq!(child.registry, private_origin);
    assert!(requests.contains(&(private_origin.to_string(), "@acme/helper".to_owned())));
}

#[test]
fn optional_dependency_uses_its_configured_registry_route() {
    let config = crate::registry::RegistryConfig::from_toml(
        r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
    )
    .unwrap();
    let private_origin = config.origin_for("@acme/feature").unwrap();
    let root = Dependency::new(
        NPM.parse().unwrap(),
        "public-app".parse().unwrap(),
        "*".parse().unwrap(),
    );
    let mut public_package = named_record("public-app", "1.0.0", &[]);
    public_package
        .optional_dependencies
        .insert("@acme/feature".into(), "^1".into());
    let mut private_feature = named_record("@acme/feature", "1.1.0", &[]);
    private_feature.registry = private_origin.clone();

    let result = resolve_with_fetch_routed(
        &[root],
        |parent, dependency| config.registry_for_dependency(parent, dependency),
        |_, name| match name.to_string().as_str() {
            "public-app" => Ok(vec![public_package.clone()]),
            "@acme/feature" => Ok(vec![private_feature.clone()]),
            other => panic!("unexpected metadata request for {other}"),
        },
    );
    let (resolution, _) = match result {
        Ok(resolution) => resolution,
        Err(error) => panic!("optional dependency resolution failed: {error}"),
    };

    assert!(resolution.selected.iter().any(|package| {
        package.name.to_string() == "@acme/feature" && package.registry == private_origin
    }));
}

#[test]
fn peer_provider_uses_the_configured_registry_route() {
    let config = crate::registry::RegistryConfig::from_toml(
        r#"[registries.'@acme']
url='https://packages.acme.example'
"#,
    )
    .unwrap();
    let private_origin = config.origin_for("@acme/host").unwrap();
    let roots = vec![
        Dependency::new(
            NPM.parse().unwrap(),
            "plugin".parse().unwrap(),
            "*".parse().unwrap(),
        ),
        Dependency::new(
            private_origin.clone(),
            "@acme/host".parse().unwrap(),
            "*".parse().unwrap(),
        ),
    ];
    let mut plugin = named_record("plugin", "1.0.0", &[]);
    plugin
        .peer_dependencies
        .insert("@acme/host".into(), "^1".into());
    let mut host = named_record("@acme/host", "1.2.0", &[]);
    host.registry = private_origin.clone();

    let result = resolve_with_fetch_routed(
        &roots,
        |parent, dependency| config.registry_for_dependency(parent, dependency),
        |_, name| match name.to_string().as_str() {
            "plugin" => Ok(vec![plugin.clone()]),
            "@acme/host" => Ok(vec![host.clone()]),
            other => panic!("unexpected metadata request for {other}"),
        },
    );

    let (resolution, _) = match result {
        Ok(resolution) => resolution,
        Err(error) => panic!("cross-origin peer resolution failed: {error}"),
    };
    assert!(resolution.selected.iter().any(|package| {
        package.name.to_string() == "@acme/host" && package.registry == private_origin
    }));
}

#[test]
fn same_origin_authenticated_and_public_routes_use_separate_transports() {
    let config = crate::registry::RegistryConfig::from_toml(
        "[registries.'@acme']\nurl='https://registry.npmjs.org'\ntoken-env='ACME_TOKEN'\n",
    )
    .unwrap();
    let authenticated = config
        .route_with_env("@acme/private", |_| Some("fixture-only-token".into()))
        .unwrap();
    let public = config.route("left-pad").unwrap();
    assert_eq!(authenticated.origin, public.origin);
    assert!(authenticated.token.is_some());
    assert!(public.token.is_none());
    assert_ne!(authenticated.policy, public.policy);

    for artifact in [false, true] {
        let mut cache = BTreeMap::new();
        let authenticated_transport = transport_for_route(
            &mut cache,
            authenticated.clone(),
            &[NPM.to_owned()],
            artifact,
        )
        .unwrap() as *const HttpsTransport;
        let public_transport =
            transport_for_route(&mut cache, public.clone(), &[NPM.to_owned()], artifact).unwrap()
                as *const HttpsTransport;

        assert_ne!(authenticated_transport, public_transport);
        assert_eq!(cache.len(), 2);
    }
}

#[test]
fn artifact_progress_is_emitted_at_bounded_completion_checkpoints() {
    let checkpoints = (1..=625)
        .filter(|completed| artifact_progress_checkpoint(*completed, 625))
        .collect::<Vec<_>>();

    assert_eq!(checkpoints.first(), Some(&1));
    assert_eq!(checkpoints.last(), Some(&625));
    assert!(checkpoints.len() <= 14);
}

#[test]
fn manifest_overrides_rejects_version_qualified_selectors() {
    let manifest = PackageManifest::parse(
        r#"{"name":"root","version":"1.0.0","overrides":{"typescript@*":"$typescript"}}"#,
    )
    .unwrap();

    let error = manifest_overrides(&manifest).unwrap_err();
    assert!(error.contains("unsupported override selector"));
    assert!(error.contains("typescript@*"));
}

#[test]
fn manifest_roots_rejects_conflicting_direct_dependency_override() {
    let manifest = PackageManifest::parse(
            r#"{"name":"root","version":"1.0.0","dependencies":{"postcss":"8.4.31"},"overrides":{"postcss":"8.5.28"}}"#,
        )
        .unwrap();

    let error = manifest_roots(&manifest).unwrap_err();
    assert!(error.contains("unsupported direct dependency override"));
    assert!(error.contains("postcss"));
}

#[test]
fn root_override_replaces_a_transitive_dependency_requirement() {
    let root = Dependency::new(
        NPM.parse().unwrap(),
        "next".parse().unwrap(),
        "1.0.0".parse().unwrap(),
    );
    let overrides = BTreeMap::from([("postcss".parse().unwrap(), "8.5.28".parse().unwrap())]);
    let (resolution, records) = resolve_with_overrides(&[root], &overrides, |_, name| {
        match name.to_string().as_str() {
            "next" => Ok(vec![named_record(
                "next",
                "1.0.0",
                &[("postcss", "8.4.31")],
            )]),
            "postcss" => Ok(vec![
                named_record("postcss", "8.4.31", &[]),
                named_record("postcss", "8.5.28", &[]),
            ]),
            _ => panic!("unexpected metadata request for {name}"),
        }
    })
    .unwrap();

    assert!(resolution.selected.iter().any(|package| {
        package.name.to_string() == "postcss" && package.version.to_string() == "8.5.28"
    }));
    assert!(!resolution.selected.iter().any(|package| {
        package.name.to_string() == "postcss" && package.version.to_string() == "8.4.31"
    }));
    assert_eq!(
        records[&(NPM.into(), "next".into(), "1.0.0".into())].dependencies["postcss"],
        "8.5.28"
    );
}

#[test]
fn wide_required_frontier_rebuilds_metadata_only_once_per_wave() {
    RESOLVER_METADATA_BUILD_COUNT.set(0);
    let registry: RegistryOrigin = NPM.parse().unwrap();
    let roots = (0..64)
        .map(|index| {
            Dependency::new(
                registry.clone(),
                format!("pkg-{index}").parse().unwrap(),
                "1.0.0".parse().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut fetched = Vec::new();

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        fetched.push(name.to_string());
        Ok(vec![named_record(&name.to_string(), "1.0.0", &[])])
    })
    .unwrap();

    assert_eq!(resolution.selected.len(), 64);
    assert_eq!(fetched.len(), 64);
    assert_eq!(RESOLVER_METADATA_BUILD_COUNT.get(), 2);
}

#[test]
fn deep_required_frontier_normalizes_each_version_once() {
    RESOLVER_METADATA_VERSION_VISITS.set(0);
    let registry: RegistryOrigin = NPM.parse().unwrap();
    let root = Dependency::new(registry, "pkg-0".parse().unwrap(), "1.0.0".parse().unwrap());

    let (resolution, _) = resolve_with_fetch(&[root], |_, name| {
        let index = name.to_string()[4..].parse::<usize>().unwrap();
        let mut package = named_record(&name.to_string(), "1.0.0", &[]);
        if index < 31 {
            package
                .dependencies
                .insert(format!("pkg-{}", index + 1), "1.0.0".into());
        }
        Ok(vec![package])
    })
    .unwrap();

    assert_eq!(resolution.selected.len(), 32);
    assert_eq!(RESOLVER_METADATA_VERSION_VISITS.get(), 32);
}

#[test]
fn one_packument_updates_parent_metadata_once_per_version() {
    RESOLVER_METADATA_PARENT_VISITS.set(0);
    let registry: RegistryOrigin = NPM.parse().unwrap();
    let root = Dependency::new(registry, "large".parse().unwrap(), "*".parse().unwrap());
    let version_count = 256;

    let (resolution, _) = resolve_with_fetch(&[root], |_, name| {
        assert_eq!(name.to_string(), "large");
        Ok((0..version_count)
            .map(|patch| named_record("large", &format!("1.0.{patch}"), &[]))
            .collect())
    })
    .unwrap();

    assert_eq!(resolution.selected[0].version.to_string(), "1.0.255");
    assert!(
        RESOLVER_METADATA_PARENT_VISITS.get() <= version_count * 2,
        "one packument caused {} parent metadata visits",
        RESOLVER_METADATA_PARENT_VISITS.get()
    );
}

#[test]
fn metadata_progress_is_emitted_at_bounded_checkpoints() {
    let checkpoints = (1..=612)
        .filter(|fetches| metadata_progress_checkpoint(*fetches))
        .collect::<Vec<_>>();

    assert_eq!(checkpoints.first(), Some(&1));
    assert_eq!(checkpoints.last(), Some(&600));
    assert!(checkpoints.len() <= 13);
}

#[test]
fn padded_and_unpadded_sha512_inputs_canonicalize_and_verify() {
    let bytes = b"archive bytes";
    let padded = integrity(bytes);
    let unpadded_text = padded.to_string().trim_end_matches('=').to_owned();
    let unpadded: PackageIntegrity = unpadded_text.parse().unwrap();

    assert_ne!(unpadded_text, padded.to_string());
    assert_eq!(unpadded.to_string(), padded.to_string());
    let different = integrity(b"different bytes");
    assert!(integrity_matches(&padded, &integrity(bytes)));
    assert!(integrity_matches(&unpadded, &integrity(bytes)));
    assert!(!integrity_matches(&padded, &different));
    assert!(!integrity_matches(&unpadded, &different));
}

#[test]
fn explicit_registry_prefixes_are_mapped_safely() {
    let (r, n) = dep_parts("jsr:@std/path").unwrap();
    assert_eq!(r.to_string(), JSR);
    assert_eq!(n.to_string(), "@std/path");
    let (r, n) = dep_parts("npm:foo").unwrap();
    assert_eq!(r.to_string(), NPM);
    assert_eq!(n.to_string(), "foo");
}

fn record(version: &str, dependency_requirement: Option<&str>) -> PackageRecord {
    named_record(
        "framer-motion",
        version,
        &dependency_requirement
            .map(|requirement| vec![("popmotion", requirement)])
            .unwrap_or_default(),
    )
}

fn named_record(name: &str, version: &str, dependencies: &[(&str, &str)]) -> PackageRecord {
    PackageRecord {
        registry: NPM.parse().unwrap(),
        name: name.parse().unwrap(),
        version: version.parse().unwrap(),
        integrity: None,
        artifact: format!("https://registry.npmjs.org/{name}/-/{version}.tgz"),
        dependencies: dependencies
            .iter()
            .map(|(name, requirement)| ((*name).into(), (*requirement).into()))
            .collect(),
        peer_dependencies: BTreeMap::new(),
        optional_peer_dependencies: BTreeSet::new(),
        optional_dependencies: BTreeMap::new(),
        platform: PackagePlatform::unrestricted(),
        fixture: false,
    }
}

#[test]
fn fixture_metadata_preserves_peer_dependencies_separately() {
    let fixture: Fixture = serde_json::from_str(
            r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"plugin","version":"1.0.0","artifact":"base64:AA==","dependencies":{"runtime":"^1.0.0"},"peerDependencies":{"host":"^2.0.0"},"optionalPeerDependencies":["host"]}]}"#,
        )
        .unwrap();

    assert_eq!(fixture.packages[0].dependencies["runtime"], "^1.0.0");
    assert_eq!(fixture.packages[0].peer_dependencies["host"], "^2.0.0");
    assert!(
        fixture.packages[0]
            .optional_peer_dependencies
            .contains("host")
    );
    assert!(!fixture.packages[0].dependencies.contains_key("host"));
}

#[test]
fn peer_metadata_is_preserved_separately_for_peer_context_resolution() {
    let mut record = named_record("plugin", "1.0.0", &[("runtime", "^1.0.0")]);
    record
        .peer_dependencies
        .insert("host".into(), "^1.0.0".into());

    let normalized = normalize_record(&record).unwrap();
    assert_eq!(normalized.metadata.dependencies.len(), 1);
    assert_eq!(
        normalized.metadata.peer_dependencies[&"host".parse().unwrap()].raw,
        "^1.0.0"
    );
}

#[test]
fn normalization_preserves_optional_peer_markers() {
    let mut record = named_record("plugin", "1.0.0", &[]);
    record
        .peer_dependencies
        .insert("host".into(), "^2.0.0".into());
    record.optional_peer_dependencies.insert("host".into());

    let normalized = normalize_record(&record).unwrap();
    assert!(
        normalized
            .metadata
            .optional_peer_dependencies
            .contains(&"host".parse().unwrap())
    );
}

#[test]
fn normalization_rejects_optional_marker_for_undeclared_peer() {
    let mut record = named_record("plugin", "1.0.0", &[]);
    record.optional_peer_dependencies.insert("host".into());

    let error = match normalize_record(&record) {
        Err(error) => error,
        Ok(_) => panic!("optional marker for an undeclared peer was accepted"),
    };
    assert!(error.contains("optional peer metadata refers to undeclared peer host"));
}

#[test]
fn malformed_peer_requirement_fails_without_flattening() {
    let mut record = named_record("plugin", "1.0.0", &[("runtime", "^1.0.0")]);
    record
        .peer_dependencies
        .insert("host".into(), "not-a-range".into());

    let error = match normalize_record(&record) {
        Ok(_) => panic!("malformed peer metadata was accepted"),
        Err(error) => error,
    };
    assert!(error.contains("peer dependency host has unsupported requirement"));
    assert!(!error.contains("ordinary"));
}

#[test]
fn malformed_peer_name_fails_closed() {
    let mut record = named_record("plugin", "1.0.0", &[]);
    record
        .peer_dependencies
        .insert("../host".into(), "^1.0.0".into());

    let error = match normalize_record(&record) {
        Ok(_) => panic!("malformed peer metadata was accepted"),
        Err(error) => error,
    };
    assert!(error.contains("peer dependency ../host has an unsupported name"));
}

#[test]
fn selected_platform_constraints_produce_an_exact_lockfile_context() {
    let platform = PackagePlatform {
        os: vec!["darwin".into()],
        cpu: vec!["arm64".into()],
        libc: Vec::new(),
    };

    let context = selected_platform_context_for("macos", "aarch64", None, &platform).unwrap();

    assert_eq!(context.os.as_deref(), Some("darwin"));
    assert_eq!(context.cpu.as_deref(), Some("arm64"));
    assert_eq!(context.libc, None);
}

#[test]
fn libc_constraints_follow_linux_only_npm_semantics() {
    let positive = PackagePlatform {
        os: Vec::new(),
        cpu: Vec::new(),
        libc: vec!["glibc".into()],
    };
    let exclusion_only = PackagePlatform {
        os: Vec::new(),
        cpu: Vec::new(),
        libc: vec!["!musl".into()],
    };

    assert!(platform_matches_for("macos", "aarch64", None, &positive));
    assert!(!platform_matches_for("linux", "x86_64", None, &positive));
    assert!(platform_matches_for(
        "linux",
        "x86_64",
        None,
        &exclusion_only
    ));
    assert_eq!(
        selected_platform_context_for("macos", "aarch64", None, &positive)
            .unwrap()
            .libc,
        None
    );
}

#[test]
fn incompatible_package_versions_are_not_usable() {
    let mut package = named_record("native", "1.0.0", &[]);
    package.platform.os = vec!["definitely-not-this-platform".into()];

    assert!(usable_versions(vec![package]).is_empty());
}

#[test]
fn unsupported_historical_dependencies_do_not_hide_usable_versions() {
    let versions = usable_versions(vec![
        record("2.9.5", Some("git+https://example.test/popmotion.git")),
        record("11.18.2", None),
    ]);

    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version.to_string(), "11.18.2");
}

#[test]
fn empty_dependency_ranges_exclude_only_affected_versions() {
    let versions = usable_versions(vec![record("3.0.1", Some("")), record("4.0.5", None)]);

    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version.to_string(), "4.0.5");
}

#[test]
fn production_normalization_rejects_empty_registry_dependency_ranges() {
    let package = record("3.0.1", Some(""));
    let error = match normalize_record(&package) {
        Ok(_) => panic!("empty registry dependency ranges must be rejected"),
        Err(error) => error,
    };
    assert!(error.contains("dependency"));
    assert!(error.contains("empty requirement"));
}

#[test]
fn all_unsupported_versions_remain_unavailable_to_the_resolver() {
    let versions = usable_versions(vec![record(
        "2.9.5",
        Some("git+https://example.test/popmotion.git"),
    )]);

    assert!(versions.is_empty());
}

#[test]
fn resolution_error_reports_discarded_version_and_requirement() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "framer-motion".parse().unwrap(),
        "*".parse().unwrap(),
    )];
    let result = resolve_with_fetch(&roots, |_, _| {
        Ok(vec![record(
            "2.9.5",
            Some("git+https://example.test/popmotion.git"),
        )])
    });
    let error = match result {
        Ok(_) => panic!("unsupported versions must not resolve"),
        Err(error) => error,
    };

    assert!(error.contains("discarded version 2.9.5"), "{error}");
    assert!(
        error.contains("git+https://example.test/popmotion.git"),
        "{error}"
    );
}

#[test]
fn selected_bare_major_dependency_range_remains_usable() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "app".parse().unwrap(),
        "*".parse().unwrap(),
    )];

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        Ok(match name.to_string().as_str() {
            "app" => vec![named_record("app", "1.0.0", &[("inherits", "2")])],
            "inherits" => vec![
                named_record("inherits", "1.0.0", &[]),
                named_record("inherits", "2.0.0", &[]),
                named_record("inherits", "2.0.4", &[]),
                named_record("inherits", "3.0.0", &[]),
            ],
            other => panic!("unexpected metadata fetch for {other}"),
        })
    })
    .unwrap();

    let inherits = resolution
        .selected
        .iter()
        .find(|package| package.name.to_string() == "inherits")
        .expect("inherits must be selected");
    assert_eq!(inherits.version.to_string(), "2.0.4");
}

#[test]
fn selected_npm_or_and_prerelease_ranges_remain_usable() {
    let versions = usable_versions(vec![named_record(
        "eslint-plugin-react",
        "7.37.5",
        &[
            ("jsx-ast-utils", "^2.4.1 || ^3.0.0"),
            ("resolve", "^2.0.0-next.5"),
        ],
    )]);

    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version.to_string(), "7.37.5");
}

#[test]
fn unavailable_optional_requirement_does_not_fail_resolution() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "app".parse().unwrap(),
        "*".parse().unwrap(),
    )];

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        Ok(match name.to_string().as_str() {
            "app" => {
                let mut package = named_record("app", "1.0.0", &[]);
                package
                    .optional_dependencies
                    .insert("native".into(), "2.0.0".into());
                vec![package]
            }
            "native" => vec![named_record("native", "1.0.0", &[])],
            other => panic!("unexpected metadata fetch for {other}"),
        })
    })
    .unwrap();

    assert_eq!(resolution.selected.len(), 1);
    assert_eq!(resolution.selected[0].name.to_string(), "app");
}

#[test]
fn unusable_optional_candidate_does_not_become_a_required_edge() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "app".parse().unwrap(),
        "*".parse().unwrap(),
    )];

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        Ok(match name.to_string().as_str() {
            "app" => {
                let mut package = named_record("app", "1.0.0", &[]);
                package
                    .optional_dependencies
                    .insert("native".into(), "1.0.0".into());
                vec![package]
            }
            "native" => vec![named_record(
                "native",
                "1.0.0",
                &[("historical", "git+https://example.test/repo.git")],
            )],
            other => panic!("unexpected metadata fetch for {other}"),
        })
    })
    .unwrap();

    assert_eq!(resolution.selected.len(), 1);
    assert_eq!(resolution.selected[0].name.to_string(), "app");
}

#[test]
fn incremental_resolution_fetches_and_selects_compatible_optional_dependencies() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "app".parse().unwrap(),
        "*".parse().unwrap(),
    )];
    let mut fetched = Vec::new();

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        fetched.push(name.to_string());
        Ok(match name.to_string().as_str() {
            "app" => {
                let mut package = named_record("app", "1.0.0", &[]);
                package
                    .optional_dependencies
                    .insert("native".into(), "1.0.0".into());
                vec![package]
            }
            "native" => vec![named_record("native", "1.0.0", &[])],
            other => panic!("unexpected metadata fetch for {other}"),
        })
    })
    .unwrap();

    assert_eq!(fetched, vec!["app", "native"]);
    assert!(
        resolution
            .selected
            .iter()
            .any(|id| id.name.to_string() == "native")
    );
    assert!(
        resolution
            .dependencies
            .iter()
            .any(|edge| edge.dependency.to_string() == "native")
    );
}

#[test]
fn incremental_resolution_fetches_only_the_selected_versions_dependencies() {
    let roots = vec![Dependency::new(
        NPM.parse().unwrap(),
        "app".parse().unwrap(),
        "^2.0.0".parse().unwrap(),
    )];
    let mut fetched = Vec::new();

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        fetched.push(name.to_string());
        Ok(match name.to_string().as_str() {
            "app" => vec![
                named_record("app", "1.0.0", &[("historical", "*")]),
                named_record("app", "2.0.0", &[("selected", "*")]),
            ],
            "selected" => vec![named_record("selected", "1.0.0", &[])],
            other => panic!("unexpected metadata fetch for {other}"),
        })
    })
    .unwrap();

    assert_eq!(fetched, vec!["app", "selected"]);
    assert_eq!(
        resolution
            .selected
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![
            "https://registry.npmjs.org:app@2.0.0",
            "https://registry.npmjs.org:selected@1.0.0",
        ]
    );
}

#[test]
fn incremental_resolution_fetches_metadata_before_reporting_constraint_conflicts() {
    let roots = vec![
        Dependency::new(
            NPM.parse().unwrap(),
            "shared".parse().unwrap(),
            "^0.4.0".parse().unwrap(),
        ),
        Dependency::new(
            NPM.parse().unwrap(),
            "shared".parse().unwrap(),
            "^0.4.2".parse().unwrap(),
        ),
    ];
    let mut fetches = 0;

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        fetches += 1;
        assert_eq!(name.to_string(), "shared");
        Ok(vec![
            named_record("shared", "0.4.0", &[]),
            named_record("shared", "0.4.3", &[]),
        ])
    })
    .unwrap();

    assert_eq!(fetches, 1);
    assert_eq!(
        resolution.selected[0].to_string(),
        "https://registry.npmjs.org:shared@0.4.3"
    );
}

#[test]
fn incremental_resolution_fetches_one_packument_for_multiple_selected_versions() {
    let roots = vec![
        Dependency::new(
            NPM.parse().unwrap(),
            "a".parse().unwrap(),
            "*".parse().unwrap(),
        ),
        Dependency::new(
            NPM.parse().unwrap(),
            "b".parse().unwrap(),
            "*".parse().unwrap(),
        ),
    ];
    let mut fetched = Vec::new();

    let (resolution, _) = resolve_with_fetch(&roots, |_, name| {
        fetched.push(name.to_string());
        Ok(match name.to_string().as_str() {
            "a" => vec![named_record("a", "1.0.0", &[("debug", "^3.0.0")])],
            "b" => vec![named_record("b", "1.0.0", &[("debug", "^4.0.0")])],
            "debug" => vec![
                named_record("debug", "3.2.7", &[]),
                named_record("debug", "4.3.7", &[]),
            ],
            other => panic!("unexpected metadata fetch for {other}"),
        })
    })
    .unwrap();

    assert_eq!(fetched, vec!["a", "b", "debug"]);
    assert_eq!(
        resolution
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
