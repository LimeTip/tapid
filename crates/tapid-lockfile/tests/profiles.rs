use serde_json::{Value, json};
use tapid_core::{PeerContext, PlatformContext};
use tapid_lockfile::{InstallProfile, LockfilePackageKey, NativeSelection};

fn key(name: &str) -> String {
    LockfilePackageKey::new(
        "https://registry.npmjs.org".parse().unwrap(),
        name.parse().unwrap(),
        "1.0.0".parse().unwrap(),
        &PeerContext::default(),
        &PlatformContext::new(None, None, None).unwrap(),
    )
    .to_string()
}

fn package(edges: Value, providers: Value) -> Value {
    json!({
        "sourceEvidence": {"state": "pending", "artifactUrl": "https://registry.npmjs.org/pkg.tgz",
            "artifactIntegrity": format!("sha512-{}==", "A".repeat(86))},
        "dependencies": edges, "peerProviders": providers
    })
}

#[test]
fn replay_requires_the_complete_manifest_snapshot() {
    let root = format!("sha256-{}", "a".repeat(64));
    let member = format!("sha256-{}", "b".repeat(64));
    let members = std::collections::BTreeMap::from([("packages/member".into(), member.clone())]);
    let selection = NativeSelection::new(
        "0".into(),
        "0".into(),
        root.clone(),
        members.clone(),
        vec![],
        Default::default(),
    )
    .unwrap();
    let before = selection.to_json().unwrap();
    selection.validate_replay(&root, &members).unwrap();
    assert!(selection.validate_replay(&member, &members).is_err());
    assert!(
        selection
            .validate_replay(&root, &Default::default())
            .is_err()
    );
    let changed = std::collections::BTreeMap::from([("packages/member".into(), root.clone())]);
    assert!(selection.validate_replay(&root, &changed).is_err());
    let added = std::collections::BTreeMap::from([
        ("packages/member".into(), member.clone()),
        ("packages/other".into(), member),
    ]);
    assert!(selection.validate_replay(&root, &added).is_err());
    assert_eq!(selection.to_json().unwrap(), before);
}

#[test]
fn production_closure_retains_shared_dependencies_and_exact_peer_provider() {
    let (dev, shared, provider, tool) = (key("dev"), key("shared"), key("provider"), key("tool"));
    let app = LockfilePackageKey::new(
        "https://registry.npmjs.org".parse().unwrap(),
        "app".parse().unwrap(),
        "1.0.0".parse().unwrap(),
        &PeerContext::default().with("provider".parse().unwrap(), "1.0.0".parse().unwrap()),
        &PlatformContext::new(None, None, None).unwrap(),
    )
    .to_string();
    let wire = json!({
        "lockfileVersion": 11, "resolverVersion": "0", "linkerVersion": "0",
        "rootManifestDigest": format!("sha256-{}", "a".repeat(64)),
        "workspaceManifestDigests": {},
        "declarations": [
            {"workspace": null, "name": "app-alias", "kind": "required", "target": app},
            {"workspace": null, "name": "dev", "kind": "dev", "target": dev},
            {"workspace": null, "name": "provider", "kind": "dev", "target": provider}
        ],
        "packages": {
            app.clone(): package(json!({"shared": {"kind": "required", "target": shared}}), json!({"provider": provider})),
            dev.clone(): package(json!({"shared": {"kind": "required", "target": shared}, "tool": {"kind": "required", "target": tool}}), json!({})),
            shared.clone(): package(json!({}), json!({})),
            provider.clone(): package(json!({"provider-dep": {"kind": "required", "target": key("provider-dep")}}), json!({})),
            key("provider-dep"): package(json!({}), json!({})),
            tool.clone(): package(json!({}), json!({}))
        }
    });
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    let before = selection.to_json().unwrap();
    assert_eq!(
        selection
            .materialized_declarations(InstallProfile::production())
            .map(|edge| edge.name.as_str())
            .collect::<Vec<_>>(),
        vec!["app-alias"]
    );
    assert_eq!(
        selection
            .materialized_dependencies(&app, InstallProfile::production())
            .unwrap()
            .collect::<std::collections::BTreeMap<_, _>>(),
        [("shared", shared.as_str()), ("provider", provider.as_str())]
            .into_iter()
            .collect()
    );
    assert_eq!(
        selection
            .materialized_keys(InstallProfile::production())
            .unwrap(),
        [app, shared, provider, key("provider-dep")]
            .into_iter()
            .collect()
    );
    assert_eq!(
        selection
            .materialized_keys(InstallProfile::default())
            .unwrap()
            .len(),
        6
    );
    assert_eq!(selection.to_json().unwrap(), before);
}

#[test]
fn rejects_missing_or_replaced_exact_peer_provider_even_when_consumer_is_omitted() {
    let provider = key("provider");
    let consumer = LockfilePackageKey::new(
        "https://registry.npmjs.org".parse().unwrap(),
        "consumer".parse().unwrap(),
        "1.0.0".parse().unwrap(),
        &PeerContext::default().with("provider".parse().unwrap(), "1.0.0".parse().unwrap()),
        &PlatformContext::new(None, None, None).unwrap(),
    )
    .to_string();
    let mut wire = json!({
        "lockfileVersion": 11, "resolverVersion": "0", "linkerVersion": "0",
        "rootManifestDigest": format!("sha256-{}", "a".repeat(64)),
        "workspaceManifestDigests": {},
        "declarations": [{"workspace": null, "name": "consumer", "kind": "dev", "target": consumer}],
        "packages": {
            consumer.clone(): package(json!({}), json!({})),
            provider.clone(): package(json!({}), json!({}))
        }
    });
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
    wire["packages"][&consumer]["peerProviders"] = json!({"provider": provider});
    assert!(NativeSelection::from_json(&wire.to_string()).is_ok());
    wire["packages"][&consumer]["peerProviders"] = json!({"provider": key("consumer")});
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
}

fn single_selection() -> Value {
    json!({
        "lockfileVersion": 11, "resolverVersion": "0", "linkerVersion": "0",
        "rootManifestDigest": format!("sha256-{}", "a".repeat(64)),
        "workspaceManifestDigests": {},
        "declarations": [{"workspace": null, "name": "app", "kind": "required", "target": key("app")}],
        "packages": {key("app"): package(json!({}), json!({}))}
    })
}

#[test]
fn rejects_duplicate_selection_json_fields_instead_of_replacing_edges() {
    let wire = single_selection().to_string().replace(
        "\"dependencies\":{}",
        "\"dependencies\":{},\"dependencies\":{}",
    );
    assert!(NativeSelection::from_json(&wire).is_err());
    let wire = single_selection().to_string().replace(
        "\"packages\":{",
        &format!(
            "\"packages\":{{{}:{},",
            serde_json::to_string(&key("app")).unwrap(),
            package(json!({}), json!({}))
        ),
    );
    assert!(NativeSelection::from_json(&wire).is_err());
}

#[test]
fn verified_selection_reuses_legacy_derived_output_validation() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    let integrity = format!("sha512-{}==", "A".repeat(86));
    let digest = format!("sha256-{}", "a".repeat(64));
    let evidence = LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "app",
        "1.0.0",
        &integrity,
        &digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let mut wire = single_selection();
    wire["packages"][key("app")]["sourceEvidence"] =
        json!({"state": "verified", "package": evidence});
    assert!(NativeSelection::from_json(&wire.to_string()).is_ok());
    wire["packages"][key("app")]["sourceEvidence"]["package"]["derivedHooks"] = json!([{
        "hook": "install", "key": digest, "treeDigest": digest, "attestation": "forged"
    }]);
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
}

#[test]
fn workspace_declarations_share_one_profile_and_traverse_local_package_edges() {
    use tapid_lockfile::{LocalWorkspaceSource, LockfilePackageSource};
    let source = LocalWorkspaceSource::new("packages/member", "member", "1.0.0").unwrap();
    let member = LockfilePackageKey::workspace(source.clone()).to_string();
    let mut wire = single_selection();
    wire["workspaceManifestDigests"] =
        json!({"packages/member": format!("sha256-{}", "b".repeat(64))});
    wire["declarations"].as_array_mut().unwrap().extend([
        json!({"workspace": null, "name": "member", "kind": "required", "target": member}),
        json!({"workspace": "packages/member", "name": "tool", "kind": "dev", "target": key("tool")})
    ]);
    wire["packages"][&member] = json!({
        "sourceEvidence": {"state": "workspace", "source": source},
        "dependencies": {"shared-alias": {"kind": "required", "target": key("shared")}},
        "peerProviders": {}
    });
    for name in ["tool", "shared"] {
        wire["packages"][key(name)] = package(json!({}), json!({}));
    }
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    assert_eq!(
        selection
            .materialized_keys(InstallProfile::production())
            .unwrap(),
        [key("app"), member.clone(), key("shared")]
            .into_iter()
            .collect()
    );
    assert_eq!(
        selection
            .materialized_keys(InstallProfile::default())
            .unwrap()
            .len(),
        4
    );
    let parsed: LockfilePackageKey = member.parse().unwrap();
    assert!(matches!(parsed.source, LockfilePackageSource::Workspace(_)));
    wire["workspaceManifestDigests"] = json!({});
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
}

#[test]
fn optional_omission_retains_required_alternate_paths_and_terminates_on_cycles() {
    let mut wire = single_selection();
    wire["packages"][key("app")]["dependencies"] = json!({
        "optional-alias": {"kind": "optional", "target": key("optional")},
        "shared": {"kind": "required", "target": key("shared")}
    });
    wire["packages"][key("optional")] = package(
        json!({"shared": {"kind": "required", "target": key("shared")}}),
        json!({}),
    );
    wire["packages"][key("shared")] = package(
        json!({"app": {"kind": "required", "target": key("app")}}),
        json!({}),
    );
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    assert_eq!(
        selection
            .materialized_keys(InstallProfile {
                include_dev: false,
                include_optional: false
            })
            .unwrap(),
        [key("app"), key("shared")].into_iter().collect()
    );
    assert_eq!(
        selection
            .materialized_keys(InstallProfile::default())
            .unwrap()
            .len(),
        3
    );
    wire["packages"][key("optional")]["dependencies"]["missing"] =
        json!({"kind": "required", "target": key("missing")});
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
}

#[test]
fn materialized_bindings_do_not_restore_omitted_aliases_to_retained_targets() {
    let mut wire = single_selection();
    wire["packages"][key("app")]["dependencies"] = json!({
        "optional-alias": {"kind": "optional", "target": key("shared")},
        "required-alias": {"kind": "required", "target": key("shared")}
    });
    wire["packages"][key("shared")] = package(json!({}), json!({}));
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    let profile = InstallProfile {
        include_dev: false,
        include_optional: false,
    };
    assert_eq!(
        selection.materialized_keys(profile).unwrap(),
        [key("app"), key("shared")].into_iter().collect()
    );
    let target = key("shared");
    assert_eq!(
        selection
            .materialized_dependencies(&key("app"), profile)
            .unwrap()
            .collect::<Vec<_>>(),
        vec![("required-alias", target.as_str())]
    );
}

#[test]
fn pending_evidence_cannot_smuggle_tree_receipts_or_untyped_edges() {
    let wire = single_selection();
    assert!(NativeSelection::from_json(&wire.to_string()).is_ok());
    for field in [
        "treeDigest",
        "unpackedDigest",
        "derivedHooks",
        "registryIntegrityDeclared",
    ] {
        let mut invalid = wire.clone();
        invalid["packages"][key("app")]["sourceEvidence"][field] = json!("forged");
        assert!(
            NativeSelection::from_json(&invalid.to_string()).is_err(),
            "{field}"
        );
    }
    let mut invalid = wire.clone();
    invalid["packages"][key("app")]["dependencies"] = json!({"app": {"target": key("app")}});
    assert!(NativeSelection::from_json(&invalid.to_string()).is_err());
    invalid["packages"][key("app")]["dependencies"] =
        json!({"app": {"kind": "dev", "target": key("app")}});
    assert!(NativeSelection::from_json(&invalid.to_string()).is_err());
    let mut invalid = wire.clone();
    invalid["packages"][key("app")]["sourceEvidence"]["artifactUrl"] = json!("file:///tmp/app.tgz");
    assert!(NativeSelection::from_json(&invalid.to_string()).is_err());
    let mut invalid = wire.clone();
    invalid["packages"][key("app")]["sourceEvidence"]["artifactIntegrity"] =
        json!("sha512-invalid");
    assert!(NativeSelection::from_json(&invalid.to_string()).is_err());
}

#[test]
fn schema_eleven_is_isolated_from_legacy_verified_lock_readers() {
    use tapid_lockfile::Lockfile;
    let wire = single_selection();
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    assert!(Lockfile::from_json(&selection.to_json().unwrap()).is_err());
    let legacy = Lockfile::new(&format!("sha256-{}", "a".repeat(64))).unwrap();
    assert!(NativeSelection::from_json(&legacy.to_json().unwrap()).is_err());
    let mut invalid = wire;
    invalid["lockfileVersion"] = json!(10);
    assert!(NativeSelection::from_json(&invalid.to_string()).is_err());
}

#[test]
fn rejects_unresolved_optional_override_but_retains_independent_dev_path() {
    let mut wire = single_selection();
    wire["declarations"].as_array_mut().unwrap().push(json!({
        "workspace": null, "name": "app", "kind": "optional", "target": key("app")
    }));
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
    wire["declarations"][0]["kind"] = json!("dev");
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    assert!(
        selection
            .materialized_keys(InstallProfile {
                include_dev: false,
                include_optional: false
            })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        selection
            .materialized_keys(InstallProfile {
                include_dev: true,
                include_optional: false
            })
            .unwrap(),
        [key("app")].into_iter().collect()
    );
}

#[test]
fn copied_selections_require_existing_verified_evidence() {
    use tapid_lockfile::{LockedPackage, RegistryIntegrityProvenance};
    let digest = format!("sha256-{}", "a".repeat(64));
    let source = format!("file:vendor/app.tgz#{digest}");
    let evidence = LockedPackage::new_with_provenance(
        &source,
        "app",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &digest,
        RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    let copied = evidence.key();
    let mut wire = single_selection();
    wire["declarations"][0]["target"] = json!(copied);
    wire["packages"] = json!({copied.clone(): package(json!({}), json!({}))});
    assert!(NativeSelection::from_json(&wire.to_string()).is_err());
    wire["packages"][&copied]["sourceEvidence"] = json!({"state": "verified", "package": evidence});
    let selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    assert_eq!(
        selection
            .materialized_keys(InstallProfile::production())
            .unwrap(),
        [copied].into_iter().collect()
    );
    let round_trip = NativeSelection::from_json(&selection.to_json().unwrap()).unwrap();
    assert_eq!(selection.to_json().unwrap(), round_trip.to_json().unwrap());
}

#[test]
fn recording_verified_source_preserves_selection_and_rejects_changed_pins_atomically() {
    let tool = key("tool");
    let key = key("pkg");
    let wire = json!({
        "lockfileVersion":11,"resolverVersion":"0","linkerVersion":"0",
        "rootManifestDigest":format!("sha256-{}", "a".repeat(64)),
        "workspaceManifestDigests":{},"declarations":[],
        "packages":{
            key.clone():package(json!({"tool":{"kind":"optional","target":tool}}),json!({})),
            tool.clone():package(json!({}),json!({}))
        }
    });
    let mut selection = NativeSelection::from_json(&wire.to_string()).unwrap();
    let mut source = tapid_lockfile::LockedPackage::new_with_provenance(
        "https://registry.npmjs.org",
        "pkg",
        "1.0.0",
        &format!("sha512-{}==", "A".repeat(86)),
        &format!("sha256-{}", "b".repeat(64)),
        tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
    )
    .unwrap();
    source
        .set_artifact_url("https://registry.npmjs.org/changed.tgz")
        .unwrap();
    let before = selection.to_json().unwrap();
    assert!(
        selection
            .record_verified_sources(std::collections::BTreeMap::from([(
                key.clone(),
                source.clone()
            )]))
            .is_err()
    );
    assert_eq!(selection.to_json().unwrap(), before);
    source
        .set_artifact_url("https://registry.npmjs.org/pkg.tgz")
        .unwrap();
    // The materialized lifecycle graph can lack optional bindings and contain
    // provider bindings. Neither may overwrite the complete typed edge map.
    source
        .add_alias_dependency("extra-provider", &tool)
        .unwrap();
    assert!(
        selection
            .record_verified_sources(std::collections::BTreeMap::from([
                (key.clone(), source.clone()),
                (tool.clone(), source.clone()),
            ]))
            .is_err()
    );
    assert_eq!(selection.to_json().unwrap(), before);
    selection
        .record_verified_sources(std::collections::BTreeMap::from([(key.clone(), source)]))
        .unwrap();
    let after: Value = serde_json::from_str(&selection.to_json().unwrap()).unwrap();
    assert_eq!(
        after["packages"][&key]["sourceEvidence"]["state"],
        "verified"
    );
    assert_eq!(after["declarations"], wire["declarations"]);
    assert_eq!(after["packages"][&tool], wire["packages"][&tool]);
    let tapid_lockfile::NativeSourceEvidence::Verified { package } =
        &selection.packages()[&key].source_evidence
    else {
        panic!("verified source missing")
    };
    assert_eq!(
        package.dependencies(),
        &std::collections::BTreeMap::from([("tool".into(), tool)])
    );
    assert_eq!(
        after["packages"][&key]["dependencies"],
        wire["packages"][&key]["dependencies"]
    );
}
