//! Metadata normalization, overrides, and incremental dependency graph resolution.
use crate::application::outcome::{ErrorKind, OperationalError};

use super::*;
use crate::application::outcome::{
    CandidateExclusions, DependencyCategory, ExclusionReason, RequirementDiagnostic,
};

mod exclusions;
use exclusions::{candidate_exclusions, with_exclusions};

fn resolution_error(
    error: ResolveError,
    roots: &[Dependency],
    records: &BTreeMap<PackageRecordKey, PackageRecord>,
) -> OperationalError {
    let mut result = OperationalError::from(error);
    if let Some(context) = &mut result.diagnostic
        && context.peer.is_some()
        && let Some(package) = &context.package
    {
        let record = records.get(&(
            package.source.clone(),
            package.name.clone(),
            package.version.clone().unwrap_or_default(),
        ));
        context.requirements = roots
            .iter()
            .filter(|root| {
                root.registry.to_string() == package.source
                    && root.requirement.package_name(&root.name).as_str() == package.name
                    && record.is_some_and(|record| {
                        root.requirement
                            .matches_tagged_version(&record.version, &record.dist_tags)
                    })
            })
            .map(|root| RequirementDiagnostic {
                local_name: Some(root.name.to_string()),
                requirement: root.requirement.raw.clone(),
                package: None,
                declaration: None,
            })
            .collect();
    }
    result
}

#[derive(Clone)]
pub(super) struct NormalizedRecord {
    pub(super) metadata: PackageVersionMetadata,
    pub(super) optional_dependencies: BTreeMap<PackageName, Requirement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CandidateRejection {
    pub(super) reason: ExclusionReason,
    pub(super) category: Option<DependencyCategory>,
}

impl CandidateRejection {
    fn new(reason: ExclusionReason, category: Option<DependencyCategory>) -> Self {
        Self { reason, category }
    }
}

type NormalizedRecords = BTreeMap<PackageRecordKey, Result<NormalizedRecord, CandidateRejection>>;

// Parsing is shared; callers choose either typed exclusion evidence or the
// existing explanatory error contract used outside candidate normalization.
enum RegistryRequirementRejection {
    Empty,
    Unsupported(ResolveError),
}

fn parse_registry_range(requirement: &str) -> Result<Requirement, RegistryRequirementRejection> {
    if requirement.trim().is_empty() {
        return Err(RegistryRequirementRejection::Empty);
    }
    requirement
        .parse()
        .map_err(RegistryRequirementRejection::Unsupported)
}

pub(super) fn parse_registry_requirement(
    name: &str,
    kind: &str,
    requirement: &str,
) -> Result<Requirement, String> {
    parse_registry_range(requirement).map_err(|rejection| match rejection {
        RegistryRequirementRejection::Empty => format!("{kind} {name} has an empty requirement"),
        RegistryRequirementRejection::Unsupported(error) => {
            format!("{kind} {name} has unsupported requirement {requirement}: {error}")
        }
    })
}

fn normalize_dependencies(
    values: &BTreeMap<String, String>,
    category: DependencyCategory,
) -> Result<BTreeMap<PackageName, Requirement>, CandidateRejection> {
    values
        .iter()
        .map(|(name, requirement)| {
            let name = name.parse().map_err(|_| {
                CandidateRejection::new(ExclusionReason::UnsupportedDependencyName, Some(category))
            })?;
            let requirement = parse_registry_range(requirement).map_err(|rejection| {
                CandidateRejection::new(
                    match rejection {
                        RegistryRequirementRejection::Empty => {
                            ExclusionReason::EmptyDependencyRequirement
                        }
                        RegistryRequirementRejection::Unsupported(_) => {
                            ExclusionReason::UnsupportedDependencyRequirement
                        }
                    },
                    Some(category),
                )
            })?;
            Ok((name, requirement))
        })
        .collect()
}

pub(super) fn normalize_record(
    package: &PackageRecord,
) -> Result<NormalizedRecord, CandidateRejection> {
    use DependencyCategory::*;
    if !current_platform_matches(&package.platform) {
        return Err(CandidateRejection::new(
            ExclusionReason::PlatformMismatch,
            None,
        ));
    }
    let dependencies = normalize_dependencies(&package.dependencies, Dependency)?;
    let peer_dependencies = normalize_dependencies(&package.peer_dependencies, PeerDependency)?;
    let optional_peer_dependencies = package
        .optional_peer_dependencies
        .iter()
        .map(|name| {
            name.parse::<PackageName>().map_err(|_| {
                CandidateRejection::new(
                    ExclusionReason::UnsupportedDependencyName,
                    Some(OptionalPeer),
                )
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if optional_peer_dependencies
        .iter()
        .any(|name| !peer_dependencies.contains_key(name))
    {
        return Err(CandidateRejection::new(
            ExclusionReason::UndeclaredOptionalPeer,
            Some(OptionalPeer),
        ));
    }
    let metadata = PackageVersionMetadata {
        dist_tags: package.dist_tags.clone(),
        name: package.name.clone(),
        version: package.version.clone(),
        dependencies,
        peer_dependencies,
        optional_peer_dependencies,
    };
    let optional_dependencies =
        normalize_dependencies(&package.optional_dependencies, OptionalDependency)?;
    if package.registry.as_str() == JSR
        && metadata
            .dependencies
            .values()
            .chain(metadata.peer_dependencies.values())
            .chain(optional_dependencies.values())
            .any(Requirement::is_alias)
    {
        return Err(CandidateRejection::new(
            ExclusionReason::UnsupportedJsrAlias,
            None,
        ));
    }
    Ok(NormalizedRecord {
        metadata,
        optional_dependencies,
    })
}

type ResolvedRecords = (
    Resolution,
    BTreeMap<PackageRecordKey, PackageRecord>,
    CandidateExclusions,
);

#[cfg(test)]
thread_local! {
    pub(super) static RESOLVER_METADATA_BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RESOLVER_METADATA_VERSION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static RESOLVER_METADATA_PARENT_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn insert_records<F>(
    records: &mut BTreeMap<PackageRecordKey, PackageRecord>,
    normalized: &mut NormalizedRecords,
    metadata: &mut Vec<RegistryMetadata>,
    overrides: &BTreeMap<PackageName, Requirement>,
    packages: Vec<PackageRecord>,
    registry_for_dependency: &mut F,
) -> Result<(), String>
where
    F: FnMut(&PackageSource, &PackageName) -> Result<PackageSource, String>,
{
    let mut inserted_keys = BTreeSet::new();
    let mut inserted_names = BTreeMap::<String, BTreeSet<PackageName>>::new();
    for mut package in packages {
        if package.registry.to_string() == NPM {
            for (name, requirement) in overrides {
                if let Some(dependency) = package.dependencies.get_mut(name.as_str()) {
                    *dependency = overridden_requirement(name, dependency, requirement);
                }
                if let Some(dependency) = package.optional_dependencies.get_mut(name.as_str()) {
                    *dependency = overridden_requirement(name, dependency, requirement);
                }
            }
        }
        let registry = package.registry.to_string();
        let key = (
            registry.clone(),
            package.name.to_string(),
            package.version.to_string(),
        );
        inserted_names
            .entry(registry)
            .or_default()
            .insert(package.name.clone());
        normalized.insert(key.clone(), normalize_record(&package));
        records.insert(key.clone(), package);
        inserted_keys.insert(key);
    }

    let mut candidates = BTreeMap::<(String, String), Vec<&PackageRecordKey>>::new();
    for key in records.keys() {
        candidates
            .entry((key.0.clone(), key.1.clone()))
            .or_default()
            .push(key);
    }
    let candidate_matches = |registry: &PackageSource,
                             name: &PackageName,
                             requirement: &Requirement| {
        candidates
            .get(&(registry.to_string(), name.to_string()))
            .into_iter()
            .flatten()
            .any(|key| {
                normalized
                    .get(*key)
                    .and_then(|record| record.as_ref().ok())
                    .is_some()
                    && records.get(*key).is_some_and(|candidate| {
                        current_platform_matches(&candidate.platform)
                            && requirement
                                .matches_tagged_version(&candidate.version, &candidate.dist_tags)
                    })
            })
    };

    let mut additions = BTreeMap::<PackageSource, Vec<PackageVersionMetadata>>::new();
    for key in &inserted_keys {
        let Some(record) = normalized.get(key).and_then(|record| record.as_ref().ok()) else {
            continue;
        };
        #[cfg(test)]
        RESOLVER_METADATA_VERSION_VISITS.set(RESOLVER_METADATA_VERSION_VISITS.get() + 1);
        let package = records.get(key).expect("record inserted before metadata");
        let mut package_metadata = record.metadata.clone();
        for (name, requirement) in &record.optional_dependencies {
            let actual_name = requirement.package_name(name);
            let target_registry = registry_for_dependency(&package.registry, actual_name)?;
            if candidate_matches(&target_registry, actual_name, requirement) {
                package_metadata
                    .dependencies
                    .insert(name.clone(), requirement.clone());
            }
        }
        additions
            .entry(package.registry.clone())
            .or_default()
            .push(package_metadata);
    }

    for (registry, additions) in additions {
        let registry_index =
            if let Some(index) = metadata.iter().position(|entry| entry.registry == registry) {
                index
            } else {
                metadata.push(
                    RegistryMetadata::normalize(registry.clone(), Vec::new())
                        .map_err(|error| error.to_string())?,
                );
                metadata.sort_by(|left, right| left.registry.cmp(&right.registry));
                metadata
                    .iter()
                    .position(|entry| entry.registry == registry)
                    .expect("inserted registry metadata")
            };
        let mut combined = std::mem::take(&mut metadata[registry_index].packages);
        combined.retain(|package| {
            !inserted_keys.contains(&(
                registry.to_string(),
                package.name.to_string(),
                package.version.to_string(),
            ))
        });
        combined.extend(additions);
        metadata[registry_index] =
            RegistryMetadata::normalize(registry, combined).map_err(|error| error.to_string())?;
    }

    for registry_metadata in metadata {
        let parent_registry = registry_metadata.registry.clone();
        for parent in &mut registry_metadata.packages {
            #[cfg(test)]
            RESOLVER_METADATA_PARENT_VISITS.set(RESOLVER_METADATA_PARENT_VISITS.get() + 1);
            let parent_key = (
                parent_registry.to_string(),
                parent.name.to_string(),
                parent.version.to_string(),
            );
            let Some(parent_record) = normalized
                .get(&parent_key)
                .and_then(|record| record.as_ref().ok())
            else {
                continue;
            };
            for (name, requirement) in &parent_record.optional_dependencies {
                let actual_name = requirement.package_name(name);
                let target_registry = registry_for_dependency(&parent_registry, actual_name)?;
                if inserted_names
                    .get(&target_registry.to_string())
                    .is_some_and(|names| names.contains(actual_name))
                    && candidate_matches(&target_registry, actual_name, requirement)
                {
                    parent
                        .dependencies
                        .insert(name.clone(), requirement.clone());
                }
            }
        }
    }
    Ok(())
}

fn overridden_requirement(name: &PackageName, previous: &str, replacement: &Requirement) -> String {
    if previous.trim().starts_with("npm:") {
        return match previous.parse::<Requirement>() {
            Ok(alias) => format!("npm:{}@{}", alias.package_name(name), replacement.raw),
            // Keep malformed aliases invalid rather than changing their source.
            Err(_) => previous.to_owned(),
        };
    }
    replacement.raw.clone()
}

/// Locate effective ranges using the same override rules as metadata and workspace roots.
pub(super) fn locate_override_requirements(
    context: &mut crate::application::outcome::DiagnosticContext,
    declarations: &[RequirementDiagnostic],
    manifest: &PackageManifest,
    path: &Path,
    overrides: &BTreeMap<PackageName, Requirement>,
) {
    use crate::application::outcome::Declaration;
    let mut located = Vec::new();
    for requirement in &context.requirements {
        let mut origins = Vec::new();
        for selector in manifest.overrides().keys() {
            let Ok((_, name)) = dep_parts(selector) else {
                continue;
            };
            let Some(replacement) = overrides.get(&name) else {
                continue;
            };
            let metadata_override = requirement.package.as_ref().is_some_and(|package| {
                package.source == NPM
                    && requirement.local_name.as_deref() == Some(name.as_str())
                    && overridden_requirement(&name, &requirement.requirement, replacement)
                        == requirement.requirement
            });
            let workspace_override = requirement.package.is_none()
                && requirement.declaration.as_ref().is_none_or(|declaration| {
                    declaration.workspace.is_some() && declaration.selects_resolver_root()
                })
                && declarations.iter().any(|candidate| {
                    candidate.local_name == requirement.local_name
                        && candidate.declaration.as_ref().is_some_and(|declaration| {
                            declaration.workspace.is_some() && declaration.selects_resolver_root()
                        })
                        && candidate
                            .requirement
                            .parse::<Requirement>()
                            .is_ok_and(|original| {
                                candidate.local_name.as_deref().is_some_and(|local| {
                                    local
                                        .parse::<PackageName>()
                                        .is_ok_and(|local| original.package_name(&local) == &name)
                                })
                            })
                        && requirement.requirement == replacement.raw
                });
            if metadata_override || workspace_override {
                let mut effective = requirement.clone();
                effective.declaration = Some(Declaration::manifest(
                    path,
                    Declaration::pointer("overrides", selector),
                ));
                origins.push(effective);
            }
        }
        if origins.is_empty() {
            located.push(requirement.clone());
        } else {
            located.extend(origins);
        }
    }
    context.requirements = located;
}

pub(super) fn metadata_progress_checkpoint(fetches: usize) -> bool {
    fetches == 1 || fetches.is_multiple_of(50)
}

/// Resolves incrementally, fetching metadata only when the resolver reaches a
/// package on its currently selected graph.
#[cfg(test)]
pub(super) fn resolve_with_fetch<F>(
    roots: &[Dependency],
    mut fetch: F,
) -> Result<ResolvedRecords, String>
where
    F: FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(
        roots,
        |parent, _| Ok(parent.clone()),
        &BTreeMap::new(),
        |registry, name| {
            fetch(registry, name)
                .map_err(|error| OperationalError::new(ErrorKind::RegistryMetadata, error))
        },
        |_| {},
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn resolve_with_overrides<F>(
    roots: &[Dependency],
    overrides: &BTreeMap<PackageName, Requirement>,
    mut fetch: F,
) -> Result<ResolvedRecords, String>
where
    F: FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(
        roots,
        |parent, _| Ok(parent.clone()),
        overrides,
        |registry, name| {
            fetch(registry, name)
                .map_err(|error| OperationalError::new(ErrorKind::RegistryMetadata, error))
        },
        |_| {},
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn resolve_with_fetch_routed<R, F>(
    roots: &[Dependency],
    registry_for_dependency: R,
    mut fetch: F,
) -> Result<ResolvedRecords, String>
where
    R: FnMut(&PackageSource, &PackageName) -> Result<PackageSource, String>,
    F: FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(
        roots,
        registry_for_dependency,
        &BTreeMap::new(),
        |registry, name| {
            fetch(registry, name)
                .map_err(|error| OperationalError::new(ErrorKind::RegistryMetadata, error))
        },
        |_| {},
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn resolve_with_fetch_routed_and_overrides<R, F>(
    roots: &[Dependency],
    registry_for_dependency: R,
    overrides: &BTreeMap<PackageName, Requirement>,
    fetch: F,
    progress: impl FnMut(usize),
) -> Result<ResolvedRecords, OperationalError>
where
    R: FnMut(&PackageSource, &PackageName) -> Result<PackageSource, String>,
    F: FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, OperationalError>,
{
    resolve_with_preferences(
        roots,
        registry_for_dependency,
        overrides,
        fetch,
        &tapid_resolver::ResolutionPreferences::default(),
        Vec::new(),
        progress,
    )
}

pub(super) fn preserve_pinned_record(
    package: PackageRecord,
    pinned: &PackageRecord,
) -> PackageRecord {
    // Preserve exact edges, platform constraints, and artifact pins. Fresh metadata
    // may fill an absent registry URL and recover tags and original peer ranges.
    let mut preserved = pinned.clone();
    if preserved.artifact.is_empty() && !package.fixture {
        preserved.artifact = package.artifact;
    }
    preserved.dist_tags = package.dist_tags;
    preserved.peer_dependencies = package.peer_dependencies;
    preserved.optional_peer_dependencies = package.optional_peer_dependencies;
    preserved
}

pub(super) fn resolve_with_preferences<R, F>(
    roots: &[Dependency],
    mut registry_for_dependency: R,
    overrides: &BTreeMap<PackageName, Requirement>,
    mut fetch: F,
    preferred: &tapid_resolver::ResolutionPreferences,
    seed: Vec<PackageRecord>,
    mut progress: impl FnMut(usize),
) -> Result<ResolvedRecords, OperationalError>
where
    R: FnMut(&PackageSource, &PackageName) -> Result<PackageSource, String>,
    F: FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, OperationalError>,
{
    let mut roots = roots.to_vec();
    let mut fetched = BTreeSet::<(String, String)>::new();
    let mut records = BTreeMap::<PackageRecordKey, PackageRecord>::new();
    let mut normalized = NormalizedRecords::new();
    let mut metadata = Vec::<RegistryMetadata>::new();
    let mut preferred = preferred.clone();
    let pinned_records = seed
        .iter()
        .map(|record| {
            (
                (
                    record.registry.to_string(),
                    record.name.to_string(),
                    record.version.to_string(),
                ),
                record.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut fetch = |registry: &PackageSource, name: &PackageName| {
        fetch(registry, name).map(|packages| {
            packages
                .into_iter()
                .map(|package| {
                    let key = (
                        package.registry.to_string(),
                        package.name.to_string(),
                        package.version.to_string(),
                    );
                    if let Some(pinned) = pinned_records.get(&key) {
                        preserve_pinned_record(package, pinned)
                    } else {
                        package
                    }
                })
                .collect()
        })
    };
    insert_records(
        &mut records,
        &mut normalized,
        &mut metadata,
        overrides,
        seed,
        &mut registry_for_dependency,
    )?;

    loop {
        #[cfg(test)]
        RESOLVER_METADATA_BUILD_COUNT.set(RESOLVER_METADATA_BUILD_COUNT.get() + 1);

        match tapid_resolver::resolve_graph_with_preferences(
            &roots,
            &metadata,
            ResolutionOptions::default(),
            |parent, dependency| registry_for_dependency(parent, dependency),
            &preferred,
        ) {
            Ok(resolution) => {
                let mut optional_frontier = BTreeSet::<(PackageSource, PackageName)>::new();
                for parent in &resolution.selected {
                    let key = (
                        parent.registry.to_string(),
                        parent.name.to_string(),
                        parent.version.to_string(),
                    );
                    if let Some(record) = records.get(&key) {
                        for (raw_name, raw_requirement) in &record.optional_dependencies {
                            let name: PackageName = raw_name
                                .parse()
                                .map_err(|error: tapid_core::DomainError| error.to_string())?;
                            let requirement = parse_registry_requirement(
                                &name.to_string(),
                                "optional dependency",
                                raw_requirement,
                            )?;
                            let name = requirement.package_name(&name).clone();
                            let registry = registry_for_dependency(&parent.registry, &name)?;
                            if !fetched.contains(&(registry.to_string(), name.to_string())) {
                                optional_frontier.insert((registry, name));
                            }
                        }
                    }
                }
                if !optional_frontier.is_empty() {
                    for (registry, name) in optional_frontier {
                        fetched.insert((registry.to_string(), name.to_string()));
                        if metadata_progress_checkpoint(fetched.len()) {
                            progress(fetched.len());
                        }
                        insert_records(
                            &mut records,
                            &mut normalized,
                            &mut metadata,
                            overrides,
                            fetch(&registry, &name)?,
                            &mut registry_for_dependency,
                        )?;
                    }
                    continue;
                }
                return Ok((resolution, records, candidate_exclusions(&normalized, None)));
            }
            Err(ResolveError::MissingMetadata { packages }) => {
                for (registry, name) in packages {
                    let key = (registry.clone(), name.clone());
                    if !fetched.insert(key) {
                        let error = OperationalError::new(
                            ErrorKind::Resolution,
                            format!(
                                "resolution failed: metadata for {registry}:{name} remains unavailable"
                            ),
                        );
                        return Err(with_exclusions(error, &normalized, &registry, &name));
                    }
                    if metadata_progress_checkpoint(fetched.len()) {
                        progress(fetched.len());
                    }
                    let registry: PackageSource = registry
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?;
                    let name: PackageName = name
                        .parse()
                        .map_err(|error: tapid_core::DomainError| error.to_string())?;
                    insert_records(
                        &mut records,
                        &mut normalized,
                        &mut metadata,
                        overrides,
                        fetch(&registry, &name)?,
                        &mut registry_for_dependency,
                    )?;
                }
            }
            Err(error @ ResolveError::MissingCandidate { .. })
            | Err(error @ ResolveError::Conflict { .. }) => {
                let (registry, name) = match &error {
                    ResolveError::MissingCandidate { registry, name, .. }
                    | ResolveError::Conflict { registry, name, .. } => {
                        (registry.clone(), name.clone())
                    }
                    _ => unreachable!(),
                };
                let key = (registry.clone(), name.clone());
                if !fetched.insert(key) {
                    return Err(with_exclusions(
                        resolution_error(error, &roots, &records).context("resolution failed"),
                        &normalized,
                        &registry,
                        &name,
                    ));
                }
                if metadata_progress_checkpoint(fetched.len()) {
                    progress(fetched.len());
                }
                let registry: PackageSource = registry
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                let name: PackageName = name
                    .parse()
                    .map_err(|error: tapid_core::DomainError| error.to_string())?;
                insert_records(
                    &mut records,
                    &mut normalized,
                    &mut metadata,
                    overrides,
                    fetch(&registry, &name)?,
                    &mut registry_for_dependency,
                )?;
            }
            Err(error @ ResolveError::PeerDependency { .. }) => {
                let ResolveError::PeerDependency {
                    package,
                    peer,
                    requirement,
                    provider,
                } = &error
                else {
                    unreachable!()
                };
                if requirement
                    .parse::<Requirement>()
                    .is_ok_and(|requirement| requirement.dist_tag().is_some())
                    && let Some(provider) = roots.iter().find(|root| root.name.as_str() == peer)
                {
                    let actual_name = provider.requirement.package_name(&provider.name);
                    if fetched.insert((provider.registry.to_string(), actual_name.to_string())) {
                        if metadata_progress_checkpoint(fetched.len()) {
                            progress(fetched.len());
                        }
                        insert_records(
                            &mut records,
                            &mut normalized,
                            &mut metadata,
                            overrides,
                            fetch(&provider.registry, actual_name)?,
                            &mut registry_for_dependency,
                        )?;
                        continue;
                    }
                }
                let candidate = records
                    .values()
                    .find(|record| {
                        tapid_registry_client::RegistryPackageId::from_source(
                            record.registry.clone(),
                            record.name.clone(),
                            record.version.clone(),
                        ) == **package
                    })
                    .map(|record| {
                        (
                            record.registry.clone(),
                            record.name.clone(),
                            record.version.clone(),
                        )
                    });
                let Some((registry, name, version)) = candidate else {
                    return Err(
                        resolution_error(error, &roots, &records).context("resolution failed")
                    );
                };
                if fetched.insert((registry.to_string(), name.to_string())) {
                    if metadata_progress_checkpoint(fetched.len()) {
                        progress(fetched.len());
                    }
                    insert_records(
                        &mut records,
                        &mut normalized,
                        &mut metadata,
                        overrides,
                        fetch(&registry, &name)?,
                        &mut registry_for_dependency,
                    )?;
                } else {
                    let removed = preferred.versions.remove(&(
                        registry.clone(),
                        name.clone(),
                        version.clone(),
                    ));
                    preferred.roots.retain(|(origin, local, actual), selected| {
                        !(origin == &registry
                            && selected == &version
                            && actual == &name
                            && roots.iter().any(|dependency| {
                                dependency.registry == registry
                                    && &dependency.name == local
                                    && dependency.requirement.package_name(local) == actual
                            }))
                    });
                    if !removed {
                        let Some(root) = roots
                            .iter()
                            .find(|root| root.name.as_str() == peer)
                            .cloned()
                        else {
                            return Err(resolution_error(error, &roots, &records)
                                .context("resolution failed"));
                        };
                        let actual = root.requirement.package_name(&root.name).clone();
                        let provider_version = provider.as_deref().map(|id| id.version.clone());
                        let released_version = provider_version.is_some_and(|version| {
                            preferred.versions.contains(&(
                                root.registry.clone(),
                                actual.clone(),
                                version,
                            ))
                        });
                        let released_root = preferred
                            .roots
                            .remove(&(root.registry.clone(), root.name.clone(), actual.clone()))
                            .is_some();
                        let refreshed = preferred.refresh_roots.insert((
                            root.registry.clone(),
                            root.name.clone(),
                            actual.clone(),
                        ));
                        if !(released_root || released_version && refreshed) {
                            return Err(resolution_error(error, &roots, &records)
                                .context("resolution failed"));
                        }
                        // Keep the provider's declared range and intersect the new peer
                        // constraint. Releasing a preference never overrides a manifest pin.
                        let peer_requirement: Requirement =
                            requirement.parse().map_err(OperationalError::from)?;
                        let constraint = if peer_requirement.is_alias() || actual == root.name {
                            peer_requirement
                        } else {
                            format!("npm:{actual}@{requirement}")
                                .parse()
                                .map_err(OperationalError::from)?
                        };
                        roots.push(Dependency::new(root.registry, root.name, constraint));
                    }
                }
            }
            Err(error) => {
                return Err(resolution_error(error, &roots, &records).context("resolution failed"));
            }
        }
    }
}

pub(crate) fn manifest_overrides(
    manifest: &PackageManifest,
) -> Result<BTreeMap<PackageName, Requirement>, String> {
    let mut overrides = BTreeMap::new();
    for (name, range) in manifest.overrides() {
        if name.starts_with("jsr:") {
            return Err(format!(
                "unsupported override target '{name}': npm registry package names only are supported"
            ));
        }
        let selector_name = name.strip_prefix("npm:").unwrap_or(name);
        let has_version_selector = if let Some(scoped) = selector_name.strip_prefix('@') {
            scoped.contains('@')
        } else {
            selector_name.contains('@')
        };
        if has_version_selector {
            return Err(format!(
                "unsupported override selector '{name}': version-qualified selectors are not supported"
            ));
        }
        let (registry, package) = dep_parts(name)?;
        if registry.to_string() != NPM {
            return Err(format!(
                "unsupported override target '{name}': npm registry package names only are supported"
            ));
        }
        let requirement = range
            .parse::<Requirement>()
            .map_err(|error| format!("invalid override '{name}' range '{range}': {error}"))?;
        if requirement.is_alias() {
            return Err(format!(
                "unsupported alias override '{name}': override values must be version ranges"
            ));
        }
        match overrides.insert(package.clone(), requirement.clone()) {
            Some(previous) if previous != requirement => {
                return Err(format!("conflicting override declarations for '{package}'"));
            }
            _ => {}
        }
    }
    Ok(overrides)
}

pub(crate) fn manifest_roots(manifest: &PackageManifest) -> Result<Vec<Dependency>, String> {
    let overrides = manifest_overrides(manifest)?;
    let mut roots = Vec::new();
    for (kind, name, range) in manifest.effective_install_declarations() {
        if copied::declaration(range)
            .map_err(|reason| format!("dependency '{name}': {reason}"))?
            .is_some()
        {
            continue;
        }
        if range.starts_with("workspace:") {
            continue;
        }
        let (registry, package) = dep_parts(name)?;
        let requirement = range.parse::<Requirement>().map_err(|error| {
            format!(
                "invalid {} dependency '{name}' range '{range}': {error}",
                kind.field_name()
            )
        })?;
        if registry.as_str() == JSR && requirement.is_alias() {
            return Err(format!(
                "npm alias '{name}@{range}' cannot use a JSR dependency name"
            ));
        }
        if registry.as_str() == JSR && requirement.dist_tag().is_some() {
            return Err(format!(
                "unsupported JSR dist-tag dependency '{name}@{range}': npm dist-tags only are supported"
            ));
        }
        roots.push(Dependency::new(registry.into(), package, requirement));
    }
    for dependency in &roots {
        if dependency.registry.to_string() == NPM
            && overrides
                .get(&dependency.name)
                .is_some_and(|override_requirement| {
                    override_requirement.raw != dependency.requirement.raw
                })
        {
            return Err(format!(
                "unsupported direct dependency override for '{}': npm requires the override range to match the declared dependency range",
                dependency.name
            ));
        }
    }
    Ok(roots)
}
