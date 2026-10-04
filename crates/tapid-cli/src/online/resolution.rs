//! Metadata normalization, overrides, and incremental dependency graph resolution.

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

type PackageRecordKey = (String, String, String);
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
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
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
    let candidate_matches =
        |registry: &RegistryOrigin, name: &PackageName, requirement: &Requirement| {
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
                                && requirement.matches(&candidate.version)
                        })
                })
        };

    let mut additions = BTreeMap::<RegistryOrigin, Vec<PackageVersionMetadata>>::new();
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

fn report_metadata_progress(fetches: usize) {
    if metadata_progress_checkpoint(fetches) {
        eprintln!("Registry metadata progress: {fetches} package(s) fetched");
    }
}

/// Resolves incrementally, fetching metadata only when the resolver reaches a
/// package on its currently selected graph.
#[cfg(test)]
pub(super) fn resolve_with_fetch<F>(
    roots: &[Dependency],
    fetch: F,
) -> Result<ResolvedRecords, String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(
        roots,
        |parent, _| Ok(parent.clone()),
        &BTreeMap::new(),
        fetch,
    )
}

#[cfg(test)]
pub(super) fn resolve_with_overrides<F>(
    roots: &[Dependency],
    overrides: &BTreeMap<PackageName, Requirement>,
    fetch: F,
) -> Result<ResolvedRecords, String>
where
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(roots, |parent, _| Ok(parent.clone()), overrides, fetch)
}

#[cfg(test)]
pub(super) fn resolve_with_fetch_routed<R, F>(
    roots: &[Dependency],
    registry_for_dependency: R,
    fetch: F,
) -> Result<ResolvedRecords, String>
where
    R: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    resolve_with_fetch_routed_and_overrides(roots, registry_for_dependency, &BTreeMap::new(), fetch)
}

pub(super) fn resolve_with_fetch_routed_and_overrides<R, F>(
    roots: &[Dependency],
    mut registry_for_dependency: R,
    overrides: &BTreeMap<PackageName, Requirement>,
    mut fetch: F,
) -> Result<ResolvedRecords, String>
where
    R: FnMut(&RegistryOrigin, &PackageName) -> Result<RegistryOrigin, String>,
    F: FnMut(&RegistryOrigin, &PackageName) -> Result<Vec<PackageRecord>, String>,
{
    let mut fetched = BTreeSet::<(String, String)>::new();
    let mut records = BTreeMap::<PackageRecordKey, PackageRecord>::new();
    let mut normalized = NormalizedRecords::new();
    let mut metadata = Vec::<RegistryMetadata>::new();

    loop {
        #[cfg(test)]
        RESOLVER_METADATA_BUILD_COUNT.set(RESOLVER_METADATA_BUILD_COUNT.get() + 1);

        match resolve_graph_with_routing(
            roots,
            &metadata,
            ResolutionOptions::default(),
            |parent, dependency| registry_for_dependency(parent, dependency),
        ) {
            Ok(resolution) => {
                let mut optional_frontier = BTreeSet::<(RegistryOrigin, PackageName)>::new();
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
                        report_metadata_progress(fetched.len());
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
                            Err(error)
                        } else {
                            Err(format!("{error}; {discarded}"))
                        };
                    }
                    report_metadata_progress(fetched.len());
                    let registry: RegistryOrigin = registry
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
                        Err(format!("resolution failed: {error}"))
                    } else {
                        Err(format!("resolution failed: {error}; {discarded}"))
                    };
                }
                report_metadata_progress(fetched.len());
                let registry: RegistryOrigin = registry
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
            Err(error) => return Err(format!("resolution failed: {error}")),
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
            roots.push(Dependency::new(registry, package, requirement));
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
