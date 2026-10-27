//! Read-only dependency inspection. Results are observations, never executable plans.
use super::{
    install::InstallMode,
    outcome::{ErrorKind, OperationFailure, OperationOutcome, OperationalError},
};
use crate::{
    commands::manifest::read_manifest_typed,
    online::{self, GraphPackage, GraphSnapshot},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
use tapid_lockfile::Lockfile;
use tapid_manifest::PackageManifest;

#[derive(Debug, Serialize)]
pub(crate) struct DeclarationChange {
    section: &'static str,
    name: String,
    before: Option<String>,
    after: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RootChange {
    name: String,
    before: Option<String>,
    after: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Reason {
    graph: &'static str,
    relation: &'static str,
    parent: Option<String>,
    dependency: String,
    requirement: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PackageChange {
    #[serde(flatten)]
    package: GraphPackage,
    pub change: &'static str,
    before: Option<GraphPackage>,
    after: Option<GraphPackage>,
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Effects {
    network_allowed: bool,
    metadata_source: &'static str,
    metadata_lookups: usize,
    artifact_requests: usize,
    cache_reads: bool,
    cache_writes: bool,
    project_writes: bool,
    recovery: bool,
    scripts: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PreviewReport {
    #[serde(skip_serializing_if = "super::outcome::CandidateExclusions::is_empty")]
    pub candidate_exclusions: crate::application::outcome::CandidateExclusions,
    #[serde(skip)]
    pub outcome: OperationOutcome,
    preview_kind: &'static str,
    artifacts_prepared: bool,
    target: String,
    pub declarations: Vec<DeclarationChange>,
    pub packages: Vec<PackageChange>,
    pub root_changes: Vec<RootChange>,
    pub effects: Effects,
    limitations: Vec<&'static str>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    project: &Path,
    target: &Path,
    before: &PackageManifest,
    after: &PackageManifest,
    mode: InstallMode,
    fixture: Option<&Path>,
    allow_unverified: bool,
) -> Result<PreviewReport, OperationFailure> {
    inspect(
        project,
        target,
        before,
        after,
        mode,
        fixture,
        allow_unverified,
    )
    .map_err(|error| OperationFailure::unchanged(project, error))
}

#[allow(clippy::too_many_arguments)]
fn inspect(
    project: &Path,
    target: &Path,
    before: &PackageManifest,
    after: &PackageManifest,
    mode: InstallMode,
    fixture: Option<&Path>,
    allow_unverified: bool,
) -> Result<PreviewReport, OperationalError> {
    if crate::filesystem::lifecycle_journal::has_pending(project)
        .map_err(|e| OperationalError::new(ErrorKind::Recovery, e))?
    {
        return Err(OperationalError::new(
            ErrorKind::Recovery,
            "an interrupted transaction is pending; run tapid install to recover before previewing dependencies",
        ));
    }
    let root = project.join("package.json");
    let target = if target.is_absolute() {
        target.to_owned()
    } else {
        project.join(target)
    };
    if &read_manifest_typed(&target)? != before {
        return Err(OperationalError::new(
            ErrorKind::ProjectChanged,
            "package.json changed while preparing the preview; retry against current inputs",
        ));
    }
    let manifest = if target == root {
        after.clone()
    } else {
        read_manifest_typed(&root)?
    };
    if !online::copied_root_names(&manifest)?.is_empty() {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "resolved previews of copied file/Git dependencies are unsupported; use an ordinary install",
        ));
    }
    let config = crate::registry::RegistryConfig::load(project)
        .map_err(|e| OperationalError::new(ErrorKind::RegistryConfiguration, e))?;
    let previous = match fs::read_to_string(project.join("tapid.lock")) {
        Ok(bytes) => Some(Lockfile::from_json(&bytes).map_err(OperationalError::from)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(OperationalError::from_source(ErrorKind::Lockfile, e)),
    };
    let offline = matches!(mode, InstallMode::Offline | InstallMode::CiOffline);
    let frozen = matches!(
        mode,
        InstallMode::Frozen | InstallMode::Ci | InstallMode::CiOffline
    );
    if allow_unverified && (offline || frozen) {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "--allow-unverified-registry-artifacts cannot be used with --offline or --frozen",
        ));
    }
    let mut replayable = true;
    if let Some(lock) = &previous {
        match lock.validate_replay(lock.root_manifest_digest()) {
            Ok(()) => {}
            Err(tapid_lockfile::LockfileError::UnverifiedRegistryArtifact(_))
                if allow_unverified && !offline && !frozen =>
            {
                replayable = false;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let old = previous
        .as_ref()
        .map(GraphSnapshot::from_lock)
        .transpose()?
        .unwrap_or_default();
    let update = match mode {
        InstallMode::Update(request) => Some(request),
        _ => None,
    };
    let preserve =
        matches!(mode, InstallMode::Online) || update.is_some_and(|request| request.named);
    let mut updated_manifest = None;
    let mut candidate_exclusions = super::outcome::CandidateExclusions::default();

    let policy = super::dependency_scripts::load_policy(project)?;
    let matching = if let Some(lock) = &previous {
        matches!(mode, InstallMode::Online)
            && replayable
            && before == after
            && super::replay::can_replay_online(project, lock, &config, &policy)?
    } else {
        false
    };
    let (next, metadata_lookups, metadata_source) = if offline || frozen {
        let lock = previous.as_ref().ok_or_else(|| {
            OperationalError::new(
                ErrorKind::LockfileMissing,
                "a dry-run replay requires tapid.lock",
            )
        })?;
        lock.validate_replay(&online::root_digest(project)?)
            .map_err(OperationalError::from)?;
        let workspace = online::workspace_materialization(project, &config)?;
        if !online::workspace_matches_lock(&workspace, lock) {
            return Err(OperationalError::new(
                ErrorKind::LockManifestMismatch,
                "workspace membership or member manifest changed; regenerate tapid.lock",
            ));
        }
        let dependencies =
            online::resolved_workspace_registry_dependencies(&manifest, &workspace, &config)?;
        super::replay::validate_workspace_dependency_edges(&workspace, &dependencies, lock)?;
        super::replay::validate_root_bindings(lock, &manifest, &config)?;
        online::validate_locked_files(project, lock)?;
        if !online::validate_locked_routes(lock, &config)? {
            return Err(OperationalError::new(
                ErrorKind::Lockfile,
                "locked packages target a different platform; regenerate tapid.lock",
            ));
        }
        (GraphSnapshot::from_lock(lock)?, 0, "lockfile")
    } else if preserve && matching {
        (
            GraphSnapshot::from_lock(previous.as_ref().expect("matching lock"))?,
            0,
            "lockfile",
        )
    } else {
        let (graph, count, saved, exclusions) = online::resolve_preview_graph(
            project,
            &manifest,
            &target,
            after,
            fixture,
            allow_unverified,
            &config,
            if preserve { previous.as_ref() } else { None },
            update,
        )?;
        updated_manifest = saved;
        candidate_exclusions = exclusions;
        (
            graph,
            count,
            if fixture.is_some() {
                "fixture"
            } else {
                "registry"
            },
        )
    };
    let declarations = declarations(before, updated_manifest.as_ref().unwrap_or(after));
    let root_changes = old
        .roots
        .keys()
        .chain(next.roots.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|name| old.roots.get(*name) != next.roots.get(*name))
        .map(|name| RootChange {
            name: name.clone(),
            before: old.roots.get(name).cloned(),
            after: next.roots.get(name).cloned(),
        })
        .collect();
    let mut old_reasons = reasons_by_target(&old, "before");
    let mut next_reasons = reasons_by_target(&next, "after");
    let keys = old
        .packages
        .keys()
        .chain(next.packages.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let packages = keys
        .into_iter()
        .map(|key| {
            let prior = old.packages.get(&key);
            let current = next.packages.get(&key);
            let change = match (prior, current) {
                (None, Some(_)) => "added",
                (Some(_), None) => "removed",
                (Some(a), Some(b)) if a == b => "unchanged",
                _ => "changed",
            };
            let mut reasons = old_reasons.remove(&key).unwrap_or_default();
            reasons.extend(next_reasons.remove(&key).unwrap_or_default());
            PackageChange {
                package: current.or(prior).expect("union key").clone(),
                change,
                before: prior.cloned(),
                after: current.cloned(),
                reasons,
            }
        })
        .collect();
    let mut outcome = OperationOutcome::unchanged(project);
    if allow_unverified {
        outcome
            .warnings
            .push(super::outcome::Warning::UnverifiedRegistryArtifactsAllowed);
    }
    Ok(PreviewReport {
        candidate_exclusions,
        outcome,
        preview_kind: "resolved_graph",
        artifacts_prepared: false,
        target: format!(
            "{}/{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            online::current_libc().unwrap_or("unspecified")
        ),
        declarations,
        packages,
        root_changes,
        effects: Effects {
            network_allowed: !offline && !frozen && fixture.is_none(),
            metadata_source,
            metadata_lookups,
            artifact_requests: 0,
            cache_reads: false,
            cache_writes: false,
            project_writes: false,
            recovery: false,
            scripts: false,
        },
        limitations: vec![
            "Artifact availability, archive validity, store contents, and lifecycle outputs are not verified.",
            "This observation is not an executable plan; later commands read current inputs and may resolve different metadata.",
            "Transitive lockfile edges do not retain their original requirement or dependency kind.",
        ],
    })
}

fn declarations(before: &PackageManifest, after: &PackageManifest) -> Vec<DeclarationChange> {
    let mut result = Vec::new();
    for (section, old, new) in [
        ("dependencies", before.dependencies(), after.dependencies()),
        (
            "devDependencies",
            before.dev_dependencies(),
            after.dev_dependencies(),
        ),
        (
            "optionalDependencies",
            before.optional_dependencies(),
            after.optional_dependencies(),
        ),
        (
            "peerDependencies",
            before.peer_dependencies(),
            after.peer_dependencies(),
        ),
    ] {
        for name in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
            if old.get(name) != new.get(name) {
                result.push(DeclarationChange {
                    section,
                    name: name.clone(),
                    before: old.get(name).cloned(),
                    after: new.get(name).cloned(),
                });
            }
        }
    }
    result
}

fn reasons_by_target(graph: &GraphSnapshot, phase: &'static str) -> BTreeMap<String, Vec<Reason>> {
    let mut reasons = BTreeMap::<String, Vec<Reason>>::new();
    for (name, target) in &graph.roots {
        reasons.entry(target.clone()).or_default().push(Reason {
            graph: phase,
            relation: "direct",
            parent: None,
            dependency: name.clone(),
            requirement: None,
        });
    }
    for (parent, package) in &graph.packages {
        for (name, target) in &package.dependencies {
            reasons.entry(target.clone()).or_default().push(Reason {
                graph: phase,
                relation: "dependency",
                parent: Some(parent.clone()),
                dependency: name.clone(),
                requirement: graph
                    .requirements
                    .get(&(parent.clone(), name.clone()))
                    .cloned(),
            });
        }
    }
    reasons
}
