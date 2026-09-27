use tapid_manifest::{DependencyKind, PackageManifest};

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleAction {
    Add,
    Remove,
    Update,
    Outdated,
    Prune,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DependencyMutation {
    pub(crate) name: String,
    pub(crate) requirement: Option<String>,
    pub(crate) kind: DependencyKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LifecyclePlan {
    pub(crate) action: LifecycleAction,
    pub(crate) manifest: PackageManifest,
    pub(crate) mutations: Vec<DependencyMutation>,
    pub(crate) diagnostics: Vec<String>,
}

pub(crate) fn plan_add(
    manifest: &PackageManifest,
    mutations: &[DependencyMutation],
) -> Result<LifecyclePlan, String> {
    let mut next = manifest.clone();
    for mutation in mutations {
        let requirement = mutation
            .requirement
            .as_deref()
            .ok_or_else(|| format!("add requires a requirement for '{}'", mutation.name))?;
        next = next
            .with_dependency_kind(mutation.kind, &mutation.name, requirement)
            .map_err(|error| format!("cannot add dependency '{}': {error}", mutation.name))?;
    }
    Ok(LifecyclePlan {
        action: LifecycleAction::Add,
        manifest: next,
        mutations: mutations.to_vec(),
        diagnostics: Vec::new(),
    })
}

pub(crate) fn plan_remove(
    manifest: &PackageManifest,
    names: &[String],
) -> Result<LifecyclePlan, String> {
    let mut next = manifest.clone();
    let mut mutations = Vec::new();
    for name in names {
        if name.starts_with("workspace:") {
            return Err(format!(
                "unsupported workspace dependency reference: {name}"
            ));
        }
        let kind = next.dependency_kind(name).ok_or_else(|| {
            format!("cannot remove '{name}': dependency is not declared in package.json")
        })?;
        next = next
            .without_dependency(name)
            .map_err(|error| format!("cannot remove dependency '{name}': {error}"))?;
        mutations.push(DependencyMutation {
            name: name.clone(),
            requirement: None,
            kind,
        });
    }
    Ok(LifecyclePlan {
        action: LifecycleAction::Remove,
        manifest: next,
        mutations,
        diagnostics: Vec::new(),
    })
}

pub(crate) fn plan_update(
    manifest: &PackageManifest,
    names: &[String],
    latest: bool,
) -> Result<LifecyclePlan, String> {
    let selected = if names.is_empty() {
        manifest
            .dependencies()
            .keys()
            .chain(manifest.dev_dependencies().keys())
            .chain(manifest.optional_dependencies().keys())
            .chain(manifest.peer_dependencies().keys())
            .cloned()
            .collect::<Vec<_>>()
    } else {
        names.to_vec()
    };
    let mut mutations = Vec::new();
    for name in selected {
        let kind = manifest
            .dependency_kind(&name)
            .ok_or_else(|| format!("cannot update '{name}': dependency is not declared"))?;
        let requirement = [
            manifest.dependencies(),
            manifest.dev_dependencies(),
            manifest.optional_dependencies(),
            manifest.peer_dependencies(),
        ]
        .iter()
        .find_map(|map| map.get(&name))
        .cloned()
        .unwrap();
        mutations.push(DependencyMutation {
            name,
            requirement: Some(if latest { "*".to_owned() } else { requirement }),
            kind,
        });
    }
    Ok(LifecyclePlan {
        action: LifecycleAction::Update,
        manifest: manifest.clone(),
        mutations,
        diagnostics: Vec::new(),
    })
}

pub(crate) fn parse_workspace_selector(selector: Option<&str>) -> Result<Option<&str>, String> {
    match selector {
        None => Ok(None),
        Some("") => Err("workspace selector cannot be empty".to_owned()),
        Some(value) if value.starts_with("workspace:") => Err(
            "workspace protocol references are not implemented; refusing registry fallback"
                .to_owned(),
        ),
        Some(value) => Err(format!(
            "workspace selection is not implemented: '{value}' (only the current package is supported)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PackageManifest {
        PackageManifest::new("app", "1.0.0", true).unwrap()
    }

    #[test]
    fn plans_add_into_each_dependency_kind_and_preserves_source_identity() {
        let mutations = [
            ("is-char", "^1", DependencyKind::Dependencies),
            ("npm:foo", "^2", DependencyKind::DevDependencies),
            (
                "jsr:@arvid/is-char",
                "^3",
                DependencyKind::OptionalDependencies,
            ),
            ("peer", ">=4", DependencyKind::PeerDependencies),
        ]
        .into_iter()
        .map(|(name, requirement, kind)| DependencyMutation {
            name: name.into(),
            requirement: Some(requirement.into()),
            kind,
        })
        .collect::<Vec<_>>();
        let plan = plan_add(&manifest(), &mutations).unwrap();
        assert_eq!(plan.manifest.dependencies()["is-char"], "^1");
        assert_eq!(plan.manifest.dev_dependencies()["npm:foo"], "^2");
        assert_eq!(
            plan.manifest.optional_dependencies()["jsr:@arvid/is-char"],
            "^3"
        );
        assert_eq!(plan.manifest.peer_dependencies()["peer"], ">=4");
    }

    #[test]
    fn plans_remove_without_touching_other_dependency_kinds() {
        let manifest = manifest()
            .with_dependency_kind(DependencyKind::Dependencies, "foo", "*")
            .unwrap()
            .with_dependency_kind(DependencyKind::PeerDependencies, "peer", "*")
            .unwrap();
        let plan = plan_remove(&manifest, &["foo".into()]).unwrap();
        assert!(!plan.manifest.dependencies().contains_key("foo"));
        assert!(plan.manifest.peer_dependencies().contains_key("peer"));
    }

    #[test]
    fn update_preserves_declared_ranges_unless_latest_is_explicit() {
        let manifest = manifest().with_dependency("foo", "^1.2.3").unwrap();
        let update = plan_update(&manifest, &["foo".into()], false).unwrap();
        assert_eq!(update.mutations[0].requirement.as_deref(), Some("^1.2.3"));
        let latest = plan_update(&manifest, &["foo".into()], true).unwrap();
        assert_eq!(latest.mutations[0].requirement.as_deref(), Some("*"));
    }

    #[test]
    fn workspace_selection_fails_closed() {
        assert!(parse_workspace_selector(Some("web")).is_err());
        assert!(parse_workspace_selector(Some("workspace:foo")).is_err());
    }
}
