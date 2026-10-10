//! Metadata normalization, overrides, and incremental dependency graph resolution.
use crate::application::outcome::{ErrorKind, OperationalError};

use super::*;

#[derive(Clone)]
pub(super) struct NormalizedRecord {
    pub(super) metadata: PackageVersionMetadata,
    pub(super) optional_dependencies: BTreeMap<PackageName, Requirement>,
}

type NormalizedRecords = BTreeMap<PackageRecordKey, Result<NormalizedRecord, String>>;

pub(super) fn parse_registry_requirement(
    name: &str,
    kind: &str,
    requirement: &str,
) -> Result<Requirement, String> {
    if requirement.trim().is_empty() {
        return Err(format!("{kind} {name} has an empty requirement"));
    }
    requirement.parse::<Requirement>().map_err(|error| {
        format!("{kind} {name} has unsupported requirement {requirement}: {error}")
    })
}

pub(super) fn normalize_record(package: &PackageRecord) -> Result<NormalizedRecord, String> {
    if !current_platform_matches(&package.platform) {
        return Err("version is incompatible with the current platform".to_owned());
    }
    let dependencies = package
        .dependencies
        .iter()
        .map(|(name, requirement)| {
            let parsed_name =
                name.parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| {
                        format!("dependency {name} has an unsupported name: {error}")
                    })?;
            let parsed_requirement = parse_registry_requirement(name, "dependency", requirement)?;
            Ok((parsed_name, parsed_requirement))
        })
        .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
    let peer_dependencies = package
        .peer_dependencies
        .iter()
        .map(|(name, requirement)| {
            let parsed_name =
                name.parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| {
                        format!("peer dependency {name} has an unsupported name: {error}")
                    })?;
            let parsed_requirement =
                parse_registry_requirement(name, "peer dependency", requirement)?;
            Ok((parsed_name, parsed_requirement))
        })
        .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
    let optional_peer_dependencies = package
        .optional_peer_dependencies
        .iter()
        .map(|name| {
            name.parse::<PackageName>()
                .map_err(|error: tapid_core::DomainError| {
                    format!("optional peer dependency {name} has an unsupported name: {error}")
                })
        })
        .collect::<Result<BTreeSet<PackageName>, String>>()?;
    if let Some(name) = optional_peer_dependencies
        .iter()
        .find(|name| !peer_dependencies.contains_key(*name))
    {
        return Err(format!(
            "optional peer metadata refers to undeclared peer {name}"
        ));
    }
    let metadata = PackageVersionMetadata {
        dist_tags: package.dist_tags.clone(),
        name: package.name.clone(),
        version: package.version.clone(),
        dependencies,
        peer_dependencies: peer_dependencies.clone(),
        optional_peer_dependencies,
    };
    let optional_dependencies = package
        .optional_dependencies
        .iter()
        .map(|(name, requirement)| {
            let parsed_name =
                name.parse::<PackageName>()
                    .map_err(|error: tapid_core::DomainError| {
                        format!("optional dependency {name} has an unsupported name: {error}")
                    })?;
            let parsed_requirement =
                parse_registry_requirement(name, "optional dependency", requirement)?;
            Ok((parsed_name, parsed_requirement))
        })
        .collect::<Result<BTreeMap<PackageName, Requirement>, String>>()?;
    if package.registry.as_str() == JSR
        && metadata
            .dependencies
            .values()
            .chain(metadata.peer_dependencies.values())
            .chain(optional_dependencies.values())
            .any(Requirement::is_alias)
    {
        return Err(
            "npm aliases in JSR package metadata are not supported; refusing JSR registry fallback"
                .into(),
        );
    }
    Ok(NormalizedRecord {
        metadata,
        optional_dependencies,
    })
}

type ResolvedRecords = (Resolution, BTreeMap<PackageRecordKey, PackageRecord>);

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

fn discarded_version_diagnostics(
    normalized: &NormalizedRecords,
    registry: &str,
    name: &str,
) -> String {
    normalized
        .iter()
        .filter_map(|((candidate_registry, candidate_name, version), result)| {
            (candidate_registry == registry && candidate_name == name)
                .then(|| {
                    result
                        .as_ref()
                        .err()
                        .map(|reason| format!("discarded version {version}: {reason}"))
                })
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("; ")
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
                        // Preserve exact edges, platform constraints, and artifact pins.
                        // Metadata may fill an absent registry URL and recover peer ranges.
                        let mut preserved = pinned.clone();
                        if preserved.artifact.is_empty() && !package.fixture {
                            preserved.artifact = package.artifact;
                        }
                        preserved.dist_tags = package.dist_tags;
                        preserved.peer_dependencies = package.peer_dependencies;
                        preserved.optional_peer_dependencies = package.optional_peer_dependencies;
                        preserved
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
            roots,
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
                return Ok((resolution, records));
            }
            Err(ResolveError::MissingMetadata { packages }) => {
                for (registry, name) in packages {
                    let key = (registry.clone(), name.clone());
                    if !fetched.insert(key) {
                        let discarded =
                            discarded_version_diagnostics(&normalized, &registry, &name);
                        let error = format!(
                            "resolution failed: metadata for {registry}:{name} remains unavailable"
                        );
                        return if discarded.is_empty() {
                            Err(OperationalError::new(ErrorKind::Resolution, error))
                        } else {
                            Err(OperationalError::new(
                                ErrorKind::Resolution,
                                format!("{error}; {discarded}"),
                            ))
                        };
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
                    let discarded = discarded_version_diagnostics(&normalized, &registry, &name);
                    return if discarded.is_empty() {
                        Err(OperationalError::from(error).context("resolution failed"))
                    } else {
                        Err(OperationalError::from(error)
                            .context(format!("resolution failed; {discarded}")))
                    };
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
                    ..
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
                        )
                        .to_string()
                            == *package
                    })
                    .map(|record| {
                        (
                            record.registry.clone(),
                            record.name.clone(),
                            record.version.clone(),
                        )
                    });
                let Some((registry, name, version)) = candidate else {
                    return Err(OperationalError::from(error).context("resolution failed"));
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
                        return Err(OperationalError::from(error).context("resolution failed"));
                    }
                }
            }
            Err(error) => return Err(OperationalError::from(error).context("resolution failed")),
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
    for (kind, map) in [
        ("dependencies", manifest.dependencies()),
        ("devDependencies", manifest.dev_dependencies()),
        ("optionalDependencies", manifest.optional_dependencies()),
    ] {
        for (name, range) in map {
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
                format!("invalid {kind} dependency '{name}' range '{range}': {error}")
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
