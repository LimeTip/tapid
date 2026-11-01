use std::collections::BTreeMap;
use tapid_core::{PackageName, RegistryOrigin};
use tapid_manifest::{DependencyKind, PackageManifest};
use tapid_resolver::{
    DeclarationTarget, dependency_parts, interpret_workspace_member, interpret_workspace_roots,
    legacy_root_requirements, root_membership, workspace_requirement,
};

fn route(name: &str) -> Result<RegistryOrigin, String> {
    if name.starts_with("@private/") {
        "https://packages.example.test"
            .parse()
            .map_err(|error| format!("{error}"))
    } else {
        dependency_parts(name).map(|(registry, _)| registry)
    }
}

#[test]
fn typed_member_declarations_preserve_overlap_spelling_aliases_and_exact_routes() {
    let manifest = PackageManifest::parse(
        r#"{
        "name":"member", "version":"1.0.0",
        "dependencies":{"alias":"npm:@private/actual@^1"},
        "optionalDependencies":{"alias":"npm:@private/actual@1.2.0"}
    }"#,
    )
    .unwrap();
    let declarations = interpret_workspace_member(&manifest, &BTreeMap::new(), route).unwrap();
    assert_eq!(declarations.len(), 2);
    assert_eq!(
        declarations[0].declaration.kind,
        DependencyKind::Dependencies
    );
    assert_eq!(
        declarations[1].declaration.kind,
        DependencyKind::OptionalDependencies
    );
    for declaration in &declarations {
        assert_eq!(declaration.declaration.name, "alias");
        assert_eq!(declaration.local_name.as_str(), "alias");
        assert_eq!(
            declaration.declared_requirement.raw,
            declaration.declaration.specification
        );
        let DeclarationTarget::Registry { registry, package } = &declaration.target else {
            panic!("registry target expected");
        };
        assert_eq!(registry.as_str(), "https://packages.example.test");
        assert_eq!(package.as_str(), "@private/actual");
    }
    let membership = root_membership(&manifest, dependency_parts).unwrap();
    assert_eq!(membership.required, membership.optional);
}

#[test]
fn legacy_projection_keeps_declared_identity_and_all_requirements() {
    let manifest = PackageManifest::parse(
        r#"{
        "name":"root", "version":"1.0.0",
        "dependencies":{"alias":"npm:@private/actual@^1"},
        "optionalDependencies":{"alias":"npm:@private/actual@1.2.0"},
        "overrides":{"alias":"2"}
    }"#,
    )
    .unwrap();
    let requirements = legacy_root_requirements(&manifest, dependency_parts).unwrap();
    let identity = dependency_parts("alias").unwrap();
    assert_eq!(requirements.len(), 1);
    assert_eq!(requirements[&identity].len(), 2);
    assert!(
        requirements[&identity]
            .iter()
            .all(|requirement| requirement.matches(&"1.2.0".parse().unwrap()))
    );
}

#[test]
fn workspace_facts_select_local_edges_and_peers_without_registry_fallback() {
    let manifest = PackageManifest::parse(
        r#"{
        "name":"member", "version":"1.0.0",
        "dependencies":{"local":"workspace:^"},
        "peerDependencies":{"local":"workspace:~"}
    }"#,
    )
    .unwrap();
    let facts = BTreeMap::from([("local".to_owned(), "1.2.3".parse().unwrap())]);
    let declarations = interpret_workspace_member(&manifest, &facts, |_| {
        panic!("local declarations must not route")
    })
    .unwrap();
    assert_eq!(declarations.len(), 2);
    assert_eq!(
        declarations[0].declaration.kind,
        DependencyKind::PeerDependencies
    );
    assert!(
        declarations
            .iter()
            .all(|declaration| declaration.target == DeclarationTarget::Workspace)
    );
    assert!(
        interpret_workspace_member(&manifest, &BTreeMap::new(), route)
            .unwrap_err()
            .contains("refusing registry fallback")
    );
    for raw in ["workspace:*", "workspace:^", "workspace:~", "^1"] {
        assert!(
            workspace_requirement(raw, &facts["local"])
                .unwrap()
                .matches(&facts["local"])
        );
    }
    assert!(workspace_requirement("workspace:1.2.3", &facts["local"]).is_err());
}

#[test]
fn effective_member_overrides_do_not_replace_declared_requirements() {
    let manifest = PackageManifest::parse(
        r#"{
        "name":"root", "version":"1.0.0", "overrides":{"actual":"2"}
    }"#,
    )
    .unwrap();
    let member = PackageManifest::parse(
        r#"{
        "name":"member", "version":"1.0.0", "dependencies":{"alias":"npm:actual@1"}
    }"#,
    )
    .unwrap();
    let declarations = interpret_workspace_member(&member, &BTreeMap::new(), route).unwrap();
    let dependencies = declarations
        .iter()
        .map(|declaration| {
            let DeclarationTarget::Registry { registry, .. } = &declaration.target else {
                panic!("registry target expected");
            };
            tapid_resolver::Dependency::new(
                registry.clone(),
                declaration.local_name.clone(),
                declaration.requirement.clone(),
            )
        })
        .collect::<Vec<_>>();
    let interpreted =
        interpret_workspace_roots(&manifest, &BTreeMap::new(), &dependencies, route).unwrap();
    assert_eq!(interpreted.member_requirements[0].raw, "2");
    assert_eq!(declarations[0].declared_requirement.raw, "npm:actual@1");
    assert_eq!(
        interpreted.roots[0].name,
        "alias".parse::<PackageName>().unwrap()
    );
}
