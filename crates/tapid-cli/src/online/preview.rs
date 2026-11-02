//! Metadata-only graph inspection. This module never prepares artifacts or a store.
use super::*;
use crate::application::lifecycle::UpdateRequest;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct GraphPackage {
    pub key: String,
    pub source: String,
    pub name: String,
    pub version: String,
    pub integrity: Option<String>,
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Default)]
pub(crate) struct GraphSnapshot {
    pub packages: BTreeMap<String, GraphPackage>,
    pub roots: BTreeMap<String, String>,
    pub requirements: BTreeMap<(String, String), String>,
}

impl GraphSnapshot {
    pub(crate) fn from_lock(lock: &Lockfile) -> Result<Self, OperationalError> {
        let mut graph = Self::default();
        for (key, package) in lock.packages() {
            let id: LockfilePackageKey = key.parse()?;
            graph.packages.insert(
                key.clone(),
                GraphPackage {
                    key: key.clone(),
                    source: source_text(&id.source),
                    name: id.name.to_string(),
                    version: id.version.to_string(),
                    integrity: Some(package.artifact_integrity().to_owned()),
                    dependencies: package.dependencies().clone(),
                },
            );
        }
        for (key, package) in lock.workspace_packages() {
            let id: LockfilePackageKey = key.parse()?;
            graph.packages.insert(
                key.clone(),
                GraphPackage {
                    key: key.clone(),
                    source: source_text(&id.source),
                    name: id.name.to_string(),
                    version: id.version.to_string(),
                    integrity: None,
                    dependencies: package.dependencies().clone(),
                },
            );
        }
        graph.roots = lock.root_bindings().clone();
        for key in lock.roots() {
            if !graph.roots.values().any(|target| target == key) {
                let id: LockfilePackageKey = key.parse()?;
                graph.roots.insert(id.name.to_string(), key.clone());
            }
        }
        Ok(graph)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve(
    project: &Path,
    manifest: &PackageManifest,
    fixture: Option<&Path>,
    allow_missing_integrity: bool,
    config: &crate::registry::RegistryConfig,
    previous: Option<&Lockfile>,
    update: Option<UpdateRequest<'_>>,
    workspace: WorkspaceMaterialization,
) -> Result<
    (
        GraphSnapshot,
        usize,
        Option<PackageManifest>,
        crate::application::outcome::CandidateExclusions,
    ),
    OperationalError,
> {
    let resolved = graph::resolve_package_graph(
        project,
        manifest,
        workspace,
        fixture,
        allow_missing_integrity,
        config,
        previous,
        update,
        |_| {},
    )?;
    let pinned_identities = previous
        .map(locked::reusable_locked_packages)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|(key, _)| {
            (
                source_text(&key.source),
                key.name.to_string(),
                key.version.to_string(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut snapshot = GraphSnapshot::default();
    let mut keys = BTreeMap::new();
    for id in &resolved.resolution.selected {
        let record = &resolved.records[&(
            id.registry.to_string(),
            id.name.to_string(),
            id.version.to_string(),
        )];
        let platform = record
            .platform
            .selected_context(std::env::consts::OS, std::env::consts::ARCH, current_libc())
            .map_err(|e| OperationalError::new(ErrorKind::Resolution, e))?;
        let peer = resolved
            .resolution
            .peer_contexts
            .get(id)
            .cloned()
            .unwrap_or_default();
        let key = LockfilePackageKey::from_source(
            id.registry.clone(),
            id.name.clone(),
            id.version.clone(),
            &peer,
            &platform,
        )
        .to_string();
        snapshot.packages.insert(
            key.clone(),
            GraphPackage {
                key: key.clone(),
                source: id.registry.to_string(),
                name: id.name.to_string(),
                version: id.version.to_string(),
                integrity: record.integrity.as_ref().map(ToString::to_string),
                dependencies: BTreeMap::new(),
            },
        );
        keys.insert(id.clone(), key.clone());
        // Locked metadata reconstructs exact edge pins, not original declared ranges.
        if pinned_identities.contains(&(
            id.registry.to_string(),
            id.name.to_string(),
            id.version.to_string(),
        )) {
            continue;
        }
        for (name, requirement) in record
            .dependencies
            .iter()
            .chain(&record.optional_dependencies)
        {
            snapshot
                .requirements
                .insert((key.clone(), name.clone()), requirement.clone());
        }
    }
    for edge in &resolved.resolution.dependencies {
        snapshot
            .packages
            .get_mut(&keys[&edge.parent])
            .expect("selected parent")
            .dependencies
            .insert(edge.dependency.to_string(), keys[&edge.child].clone());
    }
    for ((_, name), id) in &resolved.resolution.root_bindings {
        if resolved.direct_local_names.contains(name) {
            snapshot.roots.insert(name.to_string(), keys[id].clone());
        }
    }
    for member in &resolved.workspace_locked {
        let key = member.key();
        let id: LockfilePackageKey = key.parse()?;
        snapshot.packages.insert(
            key.clone(),
            GraphPackage {
                key,
                source: source_text(&id.source),
                name: id.name.to_string(),
                version: id.version.to_string(),
                integrity: None,
                dependencies: member.dependencies().clone(),
            },
        );
    }
    for dependency in &resolved.workspace_registry_dependencies {
        let id =
            graph::workspace_registry_target(dependency, &resolved.resolution, &resolved.records)?;
        snapshot
            .packages
            .get_mut(&dependency.member_key)
            .expect("workspace member")
            .dependencies
            .insert(dependency.manifest_name.clone(), keys[id].clone());
        snapshot.requirements.insert(
            (
                dependency.member_key.clone(),
                dependency.manifest_name.clone(),
            ),
            dependency.requirement.raw.clone(),
        );
    }
    for key in &resolved.workspace_root_keys {
        let id: LockfilePackageKey = key.parse()?;
        snapshot.roots.insert(id.name.to_string(), key.clone());
    }
    Ok((
        snapshot,
        resolved.metadata_lookups,
        resolved.updated_manifest,
        resolved.candidate_exclusions,
    ))
}

fn source_text(source: &tapid_lockfile::LockfilePackageSource) -> String {
    match source {
        tapid_lockfile::LockfilePackageSource::Registry(origin) => origin.to_string(),
        tapid_lockfile::LockfilePackageSource::Copied(source) => source.to_string(),
        tapid_lockfile::LockfilePackageSource::Workspace(source) => source.to_string(),
    }
}
