//! Read-only dependency inspection. Results are observations, never executable plans.
use super::observed_inputs as inputs;
use super::{
    install::InstallMode,
    outcome::{ErrorKind, OperationFailure, OperationOutcome, OperationalError},
};
use crate::online::{self, GraphPackage, GraphSnapshot};
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
    starting_state: serde_json::Value,
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
        || {},
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
    before_recheck: impl FnOnce(),
) -> Result<PreviewReport, OperationalError> {
    let canonical_project = fs::canonicalize(project)
        .map_err(|e| OperationalError::from_source(ErrorKind::Project, e))?;
    let relative_target = if target.is_absolute() {
        target
            .strip_prefix(project)
            .or_else(|_| target.strip_prefix(&canonical_project))
            .map_err(|_| {
                OperationalError::new(
                    ErrorKind::InvalidRequest,
                    "preview manifest is outside the project",
                )
            })?
    } else {
        target
    };
    let target_path = canonical_project.join(relative_target);
    let project = canonical_project.as_path();
    let target = target_path.as_path();
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
    let inputs = inputs::Inputs::capture(project, &target)?;
    if inputs.manifest(&target) != before {
        return Err(OperationalError::new(
            ErrorKind::ProjectChanged,
            "package.json changed while preparing the preview; retry against current inputs",
        ));
    }
    let manifest = if target == root {
        after.clone()
    } else {
        inputs.manifest(&root).clone()
    };
    if !online::copied_root_names(&manifest)?.is_empty() {
        return Err(OperationalError::new(
            ErrorKind::InvalidRequest,
            "resolved previews of copied file/Git dependencies are unsupported; use an ordinary install",
        ));
    }
    let config = &inputs.config;
    let previous = inputs
        .lock
        .as_ref()
        .map(|bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| OperationalError::from_source(ErrorKind::Lockfile, e))?;
            Lockfile::from_json(text).map_err(OperationalError::from)
        })
        .transpose()?;
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

    let policy = inputs.policy.as_ref().expect("preview policy");
    let matching = if let Some(lock) = &previous {
        matches!(mode, InstallMode::Online)
            && replayable
            && before == after
            && super::replay::can_replay_inspected(
                project,
                lock,
                config,
                policy,
                inputs.root_digest(),
                &inputs.materialize(config, None)?,
            )?
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
        lock.validate_replay(inputs.root_digest())
            .map_err(OperationalError::from)?;
        let workspace = inputs.materialize(config, None)?;
        if !online::workspace_matches_lock(&workspace, lock) {
            return Err(OperationalError::new(
                ErrorKind::LockManifestMismatch,
                "workspace membership or member manifest changed; regenerate tapid.lock",
            ));
        }
        let dependencies =
            online::resolved_workspace_registry_dependencies(&manifest, &workspace, config)?;
        super::replay::validate_workspace_dependency_edges(&workspace, &dependencies, lock)?;
        super::replay::validate_root_bindings(lock, &manifest, config)?;
        online::validate_locked_files(project, lock)?;
        if !online::validate_locked_routes(lock, config)? {
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
            fixture,
            allow_unverified,
            config,
            if preserve { previous.as_ref() } else { None },
            update,
            inputs.materialize(config, Some((&target, after)))?,
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
    before_recheck();
    inputs.recheck()?;
    Ok(PreviewReport {
        starting_state: inputs.receipt(&target, mode, allow_unverified),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::TempProject;

    #[test]
    fn preview_rejects_inputs_changed_before_final_recheck_without_writes() {
        for mutation in [
            "root",
            "selected",
            "unselected",
            "added",
            "removed",
            "lock",
            "routing",
            "valid_routing",
            "valid_policy",
            "recovery",
            "policy",
            "unreadable",
        ] {
            let project = TempProject::new("preview-stale-input").unwrap();
            project
                .write(
                    "package.json",
                    br#"{"name":"app","version":"1.0.0","workspaces":["packages/*"]}"#,
                )
                .unwrap();
            for name in ["selected", "unselected"] {
                project
                    .write(
                        format!("packages/{name}/package.json"),
                        format!(r#"{{"name":"{name}","version":"1.0.0"}}"#).as_bytes(),
                    )
                    .unwrap();
            }
            project
                .write("registry.json", serde_json::json!({"packages":[{"registry":"https://registry.npmjs.org","name":"unused","version":"1.0.0","integrity":format!("sha512-{}==", "A".repeat(86)),"artifact":"never-download"}]}).to_string().as_bytes())
                .unwrap();
            project
                .write("node_modules/sentinel", b"installed")
                .unwrap();
            project.write("store/sentinel", b"cached").unwrap();
            let target = project.path().join("packages/selected/package.json");
            let manifest = crate::commands::manifest::read_manifest_typed(&target).unwrap();
            let result = inspect(
                project.path(),
                &target,
                &manifest,
                &manifest,
                InstallMode::Online,
                Some(&project.path().join("registry.json")),
                false,
                || match mutation {
                    "root" => {
                        project
                            .write("package.json", br#"{"name":"changed","version":"1.0.0"}"#)
                            .unwrap();
                    }
                    "selected" | "unselected" => {
                        project
                            .write(
                                format!("packages/{mutation}/package.json"),
                                format!(r#"{{"name":"{mutation}","version":"2.0.0"}}"#).as_bytes(),
                            )
                            .unwrap();
                    }
                    "added" => {
                        project
                            .write(
                                "packages/new/package.json",
                                br#"{"name":"new","version":"1.0.0"}"#,
                            )
                            .unwrap();
                    }
                    "removed" => {
                        fs::remove_file(project.path().join("packages/unselected/package.json"))
                            .unwrap();
                    }
                    "lock" => {
                        project.write("tapid.lock", b"changed lock").unwrap();
                    }
                    "routing" => {
                        project
                            .write("tapid.toml", b"invalid configuration")
                            .unwrap();
                    }
                    "valid_routing" => {
                        project
                            .write(
                                "tapid.toml",
                                b"[registries.default]\nurl='https://mirror.example'\n",
                            )
                            .unwrap();
                    }
                    "valid_policy" => {
                        project
                            .write("tapid.lifecycle.toml", b"schema=1\n")
                            .unwrap();
                    }
                    "recovery" => {
                        project
                            .write(".tapid-lifecycle-journal.json", b"pending")
                            .unwrap();
                    }
                    "policy" => {
                        project
                            .write("tapid.lifecycle.toml", b"changed policy")
                            .unwrap();
                    }
                    "unreadable" => {
                        fs::remove_file(&target).unwrap();
                        fs::create_dir(&target).unwrap();
                    }
                    _ => unreachable!(),
                },
            );
            let error = result.unwrap_err();
            assert_eq!(error.kind, ErrorKind::ProjectChanged, "{mutation}: {error}");
            assert_eq!(
                fs::read(project.path().join("node_modules/sentinel")).unwrap(),
                b"installed"
            );
            assert_eq!(
                fs::read(project.path().join("store/sentinel")).unwrap(),
                b"cached"
            );
            assert!(!project.path().join(".tapid-activation.lock").exists());
            if mutation == "recovery" {
                assert_eq!(
                    fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
                    b"pending"
                );
            } else {
                assert!(
                    !project
                        .path()
                        .join(".tapid-lifecycle-journal.json")
                        .exists()
                );
            }
        }
    }
}
