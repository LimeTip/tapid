use serde::Deserialize;
use std::{fs, path::Path};
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use tapid_lockfile::Lockfile;
use tapid_manifest::{DependencyKind, PackageManifest, Workspace};
use tapid_registry_client::{HttpsTransport, JsrRegistry, NpmRegistry};
use tapid_resolver::Requirement;

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
        if requirement.starts_with("workspace:") {
            return Err(format!(
                "unsupported workspace dependency reference: {}@{}",
                mutation.name, requirement
            ));
        }
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

pub(crate) fn resolve_workspace(
    project_dir: &Path,
    selector: Option<&str>,
) -> Result<(std::path::PathBuf, PackageManifest), String> {
    if selector.is_some_and(|value| value.starts_with("workspace:")) {
        return Err(
            "workspace protocol references are not implemented; refusing registry fallback"
                .to_owned(),
        );
    }
    let workspace = Workspace::discover(project_dir)?;
    let manifest = workspace.select(selector)?.clone();
    for dependencies in [
        manifest.dependencies(),
        manifest.dev_dependencies(),
        manifest.optional_dependencies(),
        manifest.peer_dependencies(),
    ] {
        if let Some((name, requirement)) = dependencies
            .iter()
            .find(|(_, requirement)| requirement.starts_with("workspace:"))
        {
            return Err(format!(
                "unsupported workspace dependency reference: {name}@{requirement}; workspace linking is not implemented"
            ));
        }
    }
    let manifest_path = workspace.select_path(selector)?.to_path_buf();
    Ok((
        manifest_path.parent().unwrap_or(project_dir).to_path_buf(),
        manifest,
    ))
}

pub(crate) fn format_outdated_entry(entry: &OutdatedEntry) -> String {
    format!(
        "{} [{}] declared={} locked={} compatible={} available={}{}",
        entry.identity,
        entry.kind,
        entry.declared,
        entry.locked.as_deref().unwrap_or("unlocked"),
        entry.newest_compatible.as_deref().unwrap_or("unavailable"),
        entry.newest_available.as_deref().unwrap_or("unavailable"),
        entry
            .diagnostic
            .as_deref()
            .map(|value| format!(" diagnostic={value}"))
            .unwrap_or_default()
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutdatedEntry {
    pub(crate) identity: String,
    pub(crate) kind: String,
    pub(crate) declared: String,
    pub(crate) locked: Option<String>,
    pub(crate) newest_compatible: Option<String>,
    pub(crate) newest_available: Option<String>,
    pub(crate) diagnostic: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    packages: Vec<FixturePackage>,
}
#[derive(Debug, Deserialize)]
struct FixturePackage {
    registry: String,
    name: String,
    version: String,
}

fn kind_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Dependencies => "dependencies",
        DependencyKind::DevDependencies => "devDependencies",
        DependencyKind::OptionalDependencies => "optionalDependencies",
        DependencyKind::PeerDependencies => "peerDependencies",
    }
}

fn direct_dependencies(manifest: &PackageManifest) -> Vec<(String, String, DependencyKind)> {
    [
        (manifest.dependencies(), DependencyKind::Dependencies),
        (manifest.dev_dependencies(), DependencyKind::DevDependencies),
        (
            manifest.optional_dependencies(),
            DependencyKind::OptionalDependencies,
        ),
        (
            manifest.peer_dependencies(),
            DependencyKind::PeerDependencies,
        ),
    ]
    .into_iter()
    .flat_map(|(map, kind)| {
        map.iter()
            .map(move |(name, requirement)| (name.clone(), requirement.clone(), kind))
    })
    .collect()
}

fn versions_from_fixture(
    path: &Path,
    origin: &RegistryOrigin,
    name: &PackageName,
) -> Result<Vec<PackageVersion>, String> {
    let fixture: Fixture = serde_json::from_str(
        &fs::read_to_string(path)
            .map_err(|error| format!("cannot read registry fixture: {error}"))?,
    )
    .map_err(|error| format!("invalid registry fixture: {error}"))?;
    fixture
        .packages
        .into_iter()
        .filter(|package| {
            package.registry == origin.to_string() && package.name == name.to_string()
        })
        .map(|package| {
            package
                .version
                .parse()
                .map_err(|error: tapid_core::DomainError| error.to_string())
        })
        .collect()
}

fn versions_from_registry(
    transport: &HttpsTransport,
    origin: &RegistryOrigin,
    name: &PackageName,
) -> Result<Vec<PackageVersion>, String> {
    let artifacts = if origin.to_string() == "https://jsr.io" {
        JsrRegistry::new(transport, origin.clone())
            .fetch(&name.to_string())
            .map_err(|error| error.to_string())?
    } else if origin.to_string() == "https://registry.npmjs.org" {
        NpmRegistry::new(transport, origin.clone())
            .fetch(&name.to_string())
            .map_err(|error| error.to_string())?
    } else {
        return Err(format!("unsupported registry origin {origin}"));
    };
    Ok(artifacts
        .into_iter()
        .map(|artifact| artifact.identity.version)
        .collect())
}

pub(crate) fn outdated_report(
    project_dir: &Path,
    workspace_selector: Option<&str>,
    registry_fixture: Option<&Path>,
) -> Result<Vec<OutdatedEntry>, String> {
    let (project_dir, manifest) = resolve_workspace(project_dir, workspace_selector)?;
    let project_dir = project_dir.as_path();
    let lock = Lockfile::from_json(
        &fs::read_to_string(project_dir.join("tapid.lock"))
            .map_err(|error| format!("cannot read lockfile: {error}"))?,
    )
    .map_err(|error| format!("invalid lockfile: {error}"))?;
    let locked = lock
        .packages_typed()
        .map_err(|error| format!("invalid lockfile package identity: {error}"))?;
    let transport = if registry_fixture.is_none() {
        Some(
            HttpsTransport::standard()
                .map_err(|error| format!("cannot initialize registry transport: {error}"))?,
        )
    } else {
        None
    };
    let mut entries = Vec::new();
    for (identity, declared, kind) in direct_dependencies(&manifest) {
        let (origin, package_name) = crate::online::dep_parts(&identity)?;
        let locked_version = locked
            .iter()
            .filter(|(key, _)| key.registry == origin && key.name == package_name)
            .map(|(key, _)| key.version.clone())
            .max();
        let versions = match registry_fixture {
            Some(path) => versions_from_fixture(path, &origin, &package_name),
            None => versions_from_registry(
                transport.as_ref().expect("transport"),
                &origin,
                &package_name,
            ),
        };
        let requirement = declared.parse::<Requirement>().ok();
        let (newest_compatible, newest_available, diagnostic) = match versions {
            Ok(mut versions) => {
                versions.sort();
                let available = versions.last().cloned();
                let compatible = requirement.as_ref().and_then(|requirement| {
                    versions
                        .iter()
                        .filter(|version| requirement.matches(version))
                        .max()
                        .cloned()
                });
                let diagnostic = if versions.is_empty() {
                    Some("registry metadata returned no versions".to_owned())
                } else if requirement.is_none() {
                    Some(format!("unsupported declared requirement: {declared}"))
                } else {
                    None
                };
                (compatible, available, diagnostic)
            }
            Err(error) => (
                None,
                None,
                Some(format!("registry metadata unavailable: {error}")),
            ),
        };
        entries.push(OutdatedEntry {
            identity,
            kind: kind_name(kind).to_owned(),
            declared,
            locked: locked_version.map(|version| version.to_string()),
            newest_compatible: newest_compatible.map(|version| version.to_string()),
            newest_available: newest_available.map(|version| version.to_string()),
            diagnostic,
        });
    }
    Ok(entries)
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
    fn rejects_workspace_protocol_as_a_registry_requirement() {
        let mutation = DependencyMutation {
            name: "local-pkg".into(),
            requirement: Some("workspace:*".into()),
            kind: DependencyKind::Dependencies,
        };
        let error = plan_add(&manifest(), &[mutation]).unwrap_err();
        assert!(error.contains("workspace dependency reference"));
    }

    #[test]
    fn update_preserves_declared_ranges_unless_latest_is_explicit() {
        let manifest = manifest().with_dependency("foo", "^1.2.3").unwrap();
        let update = plan_update(&manifest, &["foo".into()], false).unwrap();
        assert_eq!(update.mutations[0].requirement.as_deref(), Some("^1.2.3"));
        let latest = plan_update(&manifest, &["foo".into()], true).unwrap();
        assert_eq!(latest.mutations[0].requirement.as_deref(), Some("*"));
    }
}
