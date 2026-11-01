use super::*;
use crate::application::lifecycle::UpdateRequest;

/// Release only selected root preferences. Other aliases of the same package
/// retain their root binding, and transitive version preferences remain intact.
pub(super) fn refresh_selected(
    request: UpdateRequest<'_>,
    roots: &[Dependency],
    preferred: &mut tapid_resolver::ResolutionPreferences,
    seed: &mut Vec<PackageRecord>,
    mut fetch: impl FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, OperationalError>,
) -> Result<(), OperationalError> {
    let selected = request
        .plan
        .mutations
        .iter()
        .map(|mutation| dep_parts(&mutation.name).map(|(_, name)| name))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut identities = BTreeSet::new();
    for root in roots {
        if selected.contains(&root.name) {
            let actual = root.requirement.package_name(&root.name).clone();
            preferred
                .roots
                .remove(&(root.registry.clone(), root.name.clone(), actual.clone()));
            preferred.refresh_roots.insert((
                root.registry.clone(),
                root.name.clone(),
                actual.clone(),
            ));
            identities.insert((root.registry.clone(), actual));
        }
    }
    for (registry, name) in identities {
        let fresh = fetch(&registry, &name)?;
        for mut record in fresh {
            if let Some(pinned) = seed.iter().find(|p| {
                p.registry == record.registry
                    && p.name == record.name
                    && p.version == record.version
            }) {
                record = resolution::preserve_pinned_record(record, pinned);
            }
            seed.retain(|p| {
                !(p.registry == record.registry
                    && p.name == record.name
                    && p.version == record.version)
            });
            seed.push(record);
        }
    }
    Ok(())
}

/// Compute saved declarations without writing, so installs and previews agree.
/// Workspace and copied source declarations keep their source semantics.
pub(super) fn saved_manifest(
    project: &Path,
    request: UpdateRequest<'_>,
    config: &crate::registry::RegistryConfig,
    resolution: &Resolution,
    mut fetch: impl FnMut(&PackageSource, &PackageName) -> Result<Vec<PackageRecord>, OperationalError>,
) -> Result<PackageManifest, OperationalError> {
    let Some(policy) = request.save else {
        return Ok(request.plan.manifest.clone());
    };
    let workspace = Workspace::discover(project)?;
    let mut manifest = request.plan.manifest.clone();
    let mut metadata = BTreeMap::new();
    for mutation in &request.plan.mutations {
        let declared = mutation.requirement.as_deref().expect("update requirement");
        if copied::declaration(declared)?.is_some()
            || declared.starts_with("workspace:")
            || workspace
                .members()
                .iter()
                .any(|member| member.name() == mutation.name)
        {
            continue;
        }
        let requirement: Requirement = declared
            .parse()
            .map_err(|error| OperationalError::from_source(ErrorKind::InvalidRequest, error))?;
        let (_, local) = dep_parts(&mutation.name)?;
        let actual = requirement.package_name(&local);
        let registry =
            tapid_resolver::declaration_registry(&mutation.name, &requirement, |name| {
                dependency_identity(config, name).map(|(registry, _)| registry)
            })?;
        let selected = resolution
            .root_bindings
            .get(&(registry.clone(), local.clone()))
            .filter(|id| &id.name == actual && requirement.matches_locked_version(&id.version));
        let version = if let Some(id) = selected {
            id.version.clone()
        } else {
            // Peer-only declarations do not create installed roots.
            let key = (registry.clone(), actual.clone());
            if !metadata.contains_key(&key) {
                metadata.insert(key.clone(), fetch(&registry, actual)?);
            }
            metadata[&key]
                .iter()
                .filter(|record| {
                    requirement.matches_tagged_version(&record.version, &record.dist_tags)
                })
                .map(|record| &record.version)
                .max()
                .cloned()
                .ok_or_else(|| {
                    OperationalError::new(
                        ErrorKind::Resolution,
                        format!(
                            "no version satisfies selected dependency '{}'",
                            mutation.name
                        ),
                    )
                })?
        };
        let range = policy.requirement(&version);
        let saved = if requirement.is_alias() {
            format!("npm:{actual}@{range}")
        } else {
            range
        };
        manifest = manifest
            .update_dependency_kind(mutation.kind, &mutation.name, &saved)
            .map_err(|error| OperationalError::from_source(ErrorKind::InvalidRequest, error))?;
    }
    Ok(manifest)
}
