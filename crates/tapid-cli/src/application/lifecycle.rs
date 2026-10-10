use super::outcome::{ErrorKind, OperationFailure, OperationOutcome, OperationalError, Warning};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use tapid_core::{PackageName, PackageVersion, RegistryOrigin};
use tapid_lockfile::{Lockfile, LockfilePackageKey, LockfilePackageSource};
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
) -> Result<LifecyclePlan, OperationalError> {
    let mut next = manifest.clone();
    for mutation in mutations {
        let requirement = mutation.requirement.as_deref().ok_or_else(|| {
            OperationalError::new(
                ErrorKind::InvalidRequest,
                format!("add requires a requirement for '{}'", mutation.name),
            )
        })?;
        next = next
            .with_dependency_kind(mutation.kind, &mutation.name, requirement)
            .map_err(|error| {
                OperationalError::from_source(ErrorKind::InvalidRequest, error)
                    .context(format!("cannot add dependency '{}'", mutation.name))
            })?;
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
) -> Result<LifecyclePlan, OperationalError> {
    let mut next = manifest.clone();
    let mut mutations = Vec::new();
    for name in names {
        if name.starts_with("workspace:") {
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                format!("unsupported workspace dependency reference: {name}"),
            ));
        }
        let kind = next.dependency_kind(name).ok_or_else(|| {
            OperationalError::new(
                ErrorKind::InvalidRequest,
                format!("cannot remove '{name}': dependency is not declared in package.json"),
            )
        })?;
        next = next.without_dependency(name).map_err(|error| {
            OperationalError::from_source(ErrorKind::InvalidRequest, error)
                .context(format!("cannot remove dependency '{name}'"))
        })?;
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
) -> Result<LifecyclePlan, OperationalError> {
    let selected = if names.is_empty() {
        manifest
            .dependencies()
            .keys()
            .chain(manifest.dev_dependencies().keys())
            .chain(manifest.optional_dependencies().keys())
            .chain(manifest.peer_dependencies().keys())
            .cloned()
            .collect::<BTreeSet<_>>()
    } else {
        names.iter().cloned().collect()
    };
    let mut next = manifest.clone();
    let mut mutations = Vec::new();
    for name in selected {
        let mut declared = false;
        for (kind, dependencies) in [
            (DependencyKind::Dependencies, manifest.dependencies()),
            (DependencyKind::DevDependencies, manifest.dev_dependencies()),
            (
                DependencyKind::OptionalDependencies,
                manifest.optional_dependencies(),
            ),
            (
                DependencyKind::PeerDependencies,
                manifest.peer_dependencies(),
            ),
        ] {
            let Some(requirement) = dependencies.get(&name) else {
                continue;
            };
            declared = true;
            let requirement = if latest && crate::online::copied_declaration(requirement)?.is_none()
            {
                let parsed = requirement.parse::<Requirement>().map_err(|error| {
                    OperationalError::from_source(ErrorKind::InvalidRequest, error)
                        .context(format!("invalid dependency '{name}'"))
                })?;
                if parsed.is_alias() {
                    let (_, declared) = crate::online::dep_parts(&name)?;
                    format!("npm:{}@*", parsed.package_name(&declared))
                } else {
                    "*".to_owned()
                }
            } else {
                requirement.clone()
            };
            if latest {
                next = next
                    .update_dependency_kind(kind, &name, &requirement)
                    .map_err(|error| {
                        OperationalError::from_source(ErrorKind::InvalidRequest, error)
                            .context(format!("cannot update dependency '{name}'"))
                    })?;
            }
            mutations.push(DependencyMutation {
                requirement: Some(requirement),
                name: name.clone(),
                kind,
            });
        }
        if !declared {
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                format!("cannot update '{name}': dependency is not declared"),
            ));
        }
    }
    Ok(LifecyclePlan {
        action: LifecycleAction::Update,
        manifest: next,
        mutations,
        diagnostics: Vec::new(),
    })
}

pub(crate) struct WorkspaceSelection {
    pub(crate) root_dir: PathBuf,
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest: PackageManifest,
}

pub(crate) fn resolve_workspace(
    project_dir: &Path,
    selector: Option<&str>,
) -> Result<WorkspaceSelection, OperationalError> {
    if selector.is_some_and(|value| value.starts_with("workspace:")) {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "workspace protocol references are not implemented; refusing registry fallback",
        ));
    }
    let project_dir = fs::canonicalize(project_dir).map_err(|error| {
        OperationalError::from_source(ErrorKind::Project, error)
            .context("cannot open project directory")
    })?;
    let workspace = Workspace::discover(&project_dir)
        .map_err(|error| OperationalError::new(ErrorKind::Project, error))?;
    let manifest = workspace
        .select(selector)
        .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?
        .clone();
    let manifest_path = workspace
        .select_path(selector)
        .map_err(|error| OperationalError::new(ErrorKind::InvalidRequest, error))?
        .to_path_buf();
    let root_dir = workspace
        .root_path()
        .parent()
        .ok_or_else(|| {
            OperationalError::new(
                ErrorKind::Project,
                "workspace root manifest has no parent directory",
            )
        })?
        .to_path_buf();
    Ok(WorkspaceSelection {
        root_dir,
        manifest_path,
        manifest,
    })
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
            .as_ref()
            .map(|value| format!(" diagnostic={value}"))
            .unwrap_or_default()
    )
}

#[derive(Debug)]
pub(crate) struct OutdatedEntry {
    pub(crate) identity: String,
    pub(crate) kind: String,
    pub(crate) declared: String,
    pub(crate) locked: Option<String>,
    pub(crate) newest_compatible: Option<String>,
    pub(crate) newest_available: Option<String>,
    pub(crate) diagnostic: Option<OperationalError>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    packages: Vec<FixturePackage>,
}
#[derive(Debug, Deserialize)]
struct FixturePackage {
    #[serde(default)]
    dist_tags: BTreeSet<String>,
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
) -> Result<Vec<(PackageVersion, BTreeSet<String>)>, OperationalError> {
    let fixture: Fixture = serde_json::from_str(&fs::read_to_string(path).map_err(|error| {
        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
            .context("cannot read registry fixture")
    })?)
    .map_err(|error| {
        OperationalError::from_source(ErrorKind::RegistryMetadata, error)
            .context("invalid registry fixture")
    })?;
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
                .map(|version| (version, package.dist_tags))
                .map_err(|error: tapid_core::DomainError| {
                    OperationalError::from_source(ErrorKind::RegistryMetadata, error)
                })
        })
        .collect()
}

fn versions_from_registry(
    transport: &HttpsTransport,
    origin: &RegistryOrigin,
    name: &PackageName,
) -> Result<Vec<(PackageVersion, BTreeSet<String>)>, OperationalError> {
    let artifacts = if origin.to_string() == "https://jsr.io" {
        JsrRegistry::new(transport, origin.clone())
            .fetch(&name.to_string())
            .map_err(OperationalError::from)?
    } else {
        NpmRegistry::new(transport, origin.clone())
            .fetch(&name.to_string())
            .map_err(OperationalError::from)?
    };
    Ok(artifacts
        .into_iter()
        .map(|artifact| (artifact.identity.version, artifact.dist_tags))
        .collect())
}

#[derive(Debug)]
pub(crate) struct OutdatedReport {
    pub(crate) entries: Vec<OutdatedEntry>,
    pub(crate) outcome: OperationOutcome,
}

pub(crate) fn outdated_report(
    project_dir: &Path,
    workspace_selector: Option<&str>,
    registry_fixture: Option<&Path>,
) -> Result<OutdatedReport, OperationFailure> {
    let mut outcome = OperationOutcome::unchanged(project_dir);
    match outdated_entries(
        project_dir,
        workspace_selector,
        registry_fixture,
        &mut outcome,
    ) {
        Ok(entries) => Ok(OutdatedReport { entries, outcome }),
        Err(error) => {
            if error.kind == ErrorKind::Recovery {
                outcome.state = super::outcome::ChangeState::RecoveryRequired;
            }
            Err(OperationFailure::new(error, outcome, None))
        }
    }
}

fn outdated_entries(
    project_dir: &Path,
    workspace_selector: Option<&str>,
    registry_fixture: Option<&Path>,
    outcome: &mut OperationOutcome,
) -> Result<Vec<OutdatedEntry>, OperationalError> {
    let selection = resolve_workspace(project_dir, workspace_selector)?;
    outcome.project_dir = selection.root_dir.clone();
    let project_dir = selection.root_dir.as_path();
    let mut manifest = selection.manifest;
    let registry_config = crate::registry::RegistryConfig::load(project_dir)
        .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?;
    let recovery_lock = if crate::filesystem::lifecycle_journal::has_pending(project_dir)
        .map_err(|error| OperationalError::new(ErrorKind::Recovery, error))?
    {
        Some(crate::filesystem::activation::ActivationLock::acquire(
            project_dir,
        )?)
    } else {
        None
    };
    if recovery_lock.is_some() {
        let recovered_manifest = fs::read_to_string(&selection.manifest_path).map_err(|error| {
            OperationalError::from_source(ErrorKind::Recovery, error)
                .context("cannot read recovered package manifest")
        })?;
        manifest = PackageManifest::parse(&recovered_manifest).map_err(|error| {
            OperationalError::new(ErrorKind::Manifest, error)
                .context("invalid recovered package manifest")
        })?;
        outcome.warnings.push(Warning::PreviousTransactionRecovered);
    }
    let lock_path = project_dir.join("tapid.lock");
    let bytes = fs::read_to_string(&lock_path).map_err(|error| {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            ErrorKind::LockfileMissing
        } else {
            ErrorKind::Lockfile
        };
        OperationalError::from_source(kind, error).context("cannot read lockfile")
    })?;
    let lock = Lockfile::from_json(&bytes).map_err(OperationalError::from)?;
    let locked = lock.packages_typed().map_err(OperationalError::from)?;
    let direct_dependencies = direct_dependencies(&manifest);
    let local_workspace_versions = Workspace::discover(project_dir)?
        .members()
        .iter()
        .map(|member| {
            (
                member.name().to_owned(),
                member.manifest().version().clone(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    drop(recovery_lock);
    let mut transports = std::collections::BTreeMap::new();
    let allowed_origins = registry_config.configured_origins();
    let mut entries = Vec::new();
    for (identity, declared, kind) in direct_dependencies {
        if let Some(local_version) = local_workspace_versions.get(&identity) {
            let requirement = crate::online::workspace_requirement(&declared, local_version)?;
            let compatible = requirement.matches(local_version);
            let locked_version = lock
                .workspace_packages()
                .keys()
                .filter_map(|key| key.parse::<LockfilePackageKey>().ok())
                .filter(|key| {
                    key.source
                        .workspace()
                        .is_some_and(|source| source.name() == identity)
                })
                .map(|key| key.version)
                .max();
            entries.push(OutdatedEntry {
                identity,
                kind: kind_name(kind).to_owned(),
                declared: declared.clone(),
                locked: locked_version.map(|version| version.to_string()),
                newest_compatible: compatible.then(|| local_version.to_string()),
                newest_available: Some(local_version.to_string()),
                diagnostic: (!compatible).then(|| {
                    OperationalError::new(
                        ErrorKind::InvalidRequest,
                        format!(
                            "local workspace version {local_version} does not satisfy declared range {declared}"
                        ),
                    )
                }),
            });
            continue;
        }
        if declared.starts_with("workspace:") {
            return Err(OperationalError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "workspace dependency '{identity}@{declared}' has no matching local workspace member; refusing registry fallback"
                ),
            ));
        }
        if crate::online::copied_declaration(&declared)
            .map_err(|reason| {
                OperationalError::new(
                    ErrorKind::InvalidRequest,
                    format!("dependency '{identity}': {reason}"),
                )
            })?
            .is_some()
        {
            let pinned = lock
                .root_bindings()
                .get(&identity)
                .and_then(|key| key.parse::<LockfilePackageKey>().ok())
                .filter(|key| {
                    crate::online::validate_copied_root_binding(
                        &manifest,
                        &identity,
                        key.source.copied(),
                    )
                    .unwrap_or(false)
                });
            entries.push(OutdatedEntry {
                identity,
                kind: kind_name(kind).into(),
                declared,
                locked: pinned.map(|key| key.version.to_string()),
                newest_compatible: None,
                newest_available: None,
                diagnostic: None,
            });
            continue;
        }
        let (manifest_origin, local_name) = crate::online::dep_parts(&identity)?;
        let requirement = declared.parse::<Requirement>().ok();
        let package_name = requirement
            .as_ref()
            .map(|requirement| requirement.package_name(&local_name))
            .unwrap_or(&local_name)
            .clone();
        let origin = if manifest_origin.to_string() == "https://jsr.io" {
            manifest_origin
        } else {
            registry_config
                .origin_for_name(&package_name)
                .map_err(|error| OperationalError::new(ErrorKind::RegistryConfiguration, error))?
        };
        let bound_root = lock.root_bindings().get(local_name.as_str());
        let locked_version = locked
            .iter()
            .filter(|(key, _)| {
                matches!(
                    &key.source,
                    LockfilePackageSource::Registry(registry)
                        if registry == &origin
                            && key.name == package_name
                            && bound_root.is_none_or(|target| key.to_string() == *target)
                )
            })
            .map(|(key, _)| key.version.clone())
            .max();
        let versions = match registry_fixture {
            Some(path) => versions_from_fixture(path, &origin, &package_name),
            None => {
                let transport = crate::online::metadata_transport_for_package(
                    &mut transports,
                    &registry_config,
                    &origin,
                    &package_name,
                    &allowed_origins,
                );
                match transport {
                    Ok(transport) => versions_from_registry(transport, &origin, &package_name),
                    Err(error) => Err(error),
                }
            }
        };
        let (newest_compatible, newest_available, diagnostic) = match versions {
            Ok(mut versions) => {
                versions.sort();
                let available = versions.last().map(|(version, _)| version.clone());
                let compatible = requirement.as_ref().and_then(|requirement| {
                    versions
                        .iter()
                        .filter(|(version, tags)| requirement.matches_tagged_version(version, tags))
                        .map(|(version, _)| version)
                        .max()
                        .cloned()
                });
                let diagnostic = if versions.is_empty() {
                    Some(OperationalError::new(
                        ErrorKind::RegistryMetadata,
                        "registry metadata returned no versions",
                    ))
                } else if requirement.is_none() {
                    Some(OperationalError::new(
                        ErrorKind::InvalidRequest,
                        format!("unsupported declared requirement: {declared}"),
                    ))
                } else {
                    None
                };
                (compatible, available, diagnostic)
            }
            Err(error) => (
                None,
                None,
                Some(error.context("registry metadata unavailable")),
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

    #[test]
    fn outdated_missing_lock_reports_typed_unchanged_context() {
        let project = tapid_test_support::TempProject::new("outdated-outcome").unwrap();
        fs::write(
            project.path().join("package.json"),
            r#"{"name":"app","version":"1.0.0"}"#,
        )
        .unwrap();
        let failure = outdated_report(project.path(), None, None).unwrap_err();
        assert_eq!(failure.error.kind, ErrorKind::LockfileMissing);
        assert_eq!(
            failure.outcome.state,
            super::super::outcome::ChangeState::Unchanged
        );
        assert_eq!(
            failure.outcome.project_dir,
            project.path().canonicalize().unwrap()
        );
    }

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
    fn add_planner_preserves_workspace_protocol_for_workspace_preflight() {
        let mutation = DependencyMutation {
            name: "local-pkg".into(),
            requirement: Some("workspace:*".into()),
            kind: DependencyKind::Dependencies,
        };
        let plan = plan_add(&manifest(), &[mutation]).unwrap();
        assert_eq!(plan.manifest.dependencies()["local-pkg"], "workspace:*");
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
    fn update_plans_each_section_once_and_preserves_its_alias_target() {
        let manifest = PackageManifest::parse(
            r#"{"name":"app","version":"1.0.0","dependencies":{"local":"npm:regular@^1"},"devDependencies":{"local":"npm:dev@^2"},"optionalDependencies":{"local":"npm:optional@^3"},"peerDependencies":{"local":"npm:peer@^4"}}"#,
        ).unwrap();
        for names in [Vec::new(), vec!["local".into(), "local".into()]] {
            let unchanged = plan_update(&manifest, &names, false).unwrap();
            assert_eq!(unchanged.manifest, manifest);
            assert_eq!(unchanged.mutations.len(), 4);

            let latest = plan_update(&manifest, &names, true).unwrap();
            assert_eq!(latest.action, LifecycleAction::Update);
            assert_eq!(latest.mutations.len(), 4);
            assert_eq!(latest.manifest.dependencies()["local"], "npm:regular@*");
            assert_eq!(latest.manifest.dev_dependencies()["local"], "npm:dev@*");
            assert_eq!(
                latest.manifest.optional_dependencies()["local"],
                "npm:optional@*"
            );
            assert_eq!(latest.manifest.peer_dependencies()["local"], "npm:peer@*");
        }
    }

    #[test]
    fn update_rejects_undeclared_names_and_invalid_overlapping_ranges() {
        let manifest = PackageManifest::parse(
            r#"{"name":"app","version":"1.0.0","devDependencies":{"foo":"^1"},"peerDependencies":{"foo":"npm:"}}"#,
        ).unwrap();
        for names in [Vec::new(), vec!["foo".into()]] {
            let error = plan_update(&manifest, &names, true).unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidRequest);
        }
        let error = plan_update(&manifest, &["missing".into()], true).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidRequest);
        assert_eq!(manifest.dev_dependencies()["foo"], "^1");
        assert_eq!(manifest.peer_dependencies()["foo"], "npm:");
    }
}
