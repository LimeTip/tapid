//! Exact locked selection and fail-closed rebuild preflight.
use super::*;

#[derive(Clone, Copy)]
pub(crate) struct RebuildRequest<'a> {
    pub(crate) packages: &'a [String],
    pub(crate) instances: &'a [String],
    pub(crate) force: bool,
}

impl RebuildRequest<'_> {
    pub(in crate::application) fn select(
        self,
        lock: &Lockfile,
    ) -> Result<BTreeSet<String>, OperationalError> {
        let mut selected = BTreeSet::new();
        for name in self.packages {
            let matches = lock
                .packages()
                .iter()
                .filter(|(_, p)| p.name() == name)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(error(format!(
                    "rebuild selector {name:?} must identify one locked instance; found {}. Use --instance with an exact key: {}",
                    matches.len(),
                    matches
                        .iter()
                        .map(|key| format!("{key:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            selected.insert(matches[0].clone());
        }
        for key in self.instances {
            if !lock.packages().contains_key(key) {
                return Err(error(format!("rebuild instance is not locked: {key}")));
            }
            selected.insert(key.clone());
        }
        if selected.is_empty() {
            return Err(error(
                "rebuild requires at least one package or --instance selector",
            ));
        }
        Ok(selected)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn preflight(
    selected: &BTreeSet<String>,
    impact: &BTreeSet<String>,
    graph: &inputs::DependencyInputs,
    lock: &Lockfile,
    policy: &DependencyLifecyclePolicy,
    trees: &BTreeMap<String, PathBuf>,
    input: &NamedLayoutInput,
    store: &tapid_store::Store,
    snapshots: &mut Vec<tapid_store::VerifiedTreeSnapshot>,
    system_identity: &mut Option<String>,
    source_graph: &str,
    outcome: &mut super::super::outcome::OperationOutcome,
) -> Result<(), OperationalError> {
    let mut checked = BTreeMap::new();
    for key in &graph.order {
        let package = &lock.packages()[key];
        let discovered = commands(&trees[key])?;
        let requires_install = discovered.requires_install();
        let hooks = discovered.into_commands();
        if selected.contains(key) && hooks.is_empty() {
            record_failure(
                outcome,
                key,
                package,
                None,
                "not_evaluated",
                "no_supported_hooks",
            );
            return Err(error(format!(
                "selected rebuild instance has no supported hooks: {key}"
            )));
        }
        let mut outputs = Vec::new();
        for (hook, script) in hooks {
            let approval = policy
                .approval_for(
                    package.name(),
                    package.version(),
                    package.artifact_integrity(),
                    &hook,
                    &script,
                )
                .map_err(error)
                .inspect_err(|_| {
                    record_failure(
                        outcome,
                        key,
                        package,
                        Some(&hook),
                        "rejected",
                        "approval_rejected",
                    )
                })?;
            let Some(approval) = approval else {
                if impact.contains(key)
                    || !package.derived_hooks().is_empty()
                    || requires_install && hook == "install"
                {
                    record_failure(
                        outcome,
                        key,
                        package,
                        Some(&hook),
                        "not_approved",
                        "required_hook_unapproved",
                    );
                    return Err(error(format!(
                        "rebuild requires current exact approval for {key} {hook}; review tapid.lifecycle.toml"
                    )));
                }
                continue;
            };
            if !package.has_declared_registry_integrity() {
                return Err(error("rebuild requires registry-declared integrity"));
            }
            if approval.dependencies() {
                graph.layout(key, input)?;
            }
            if system_identity.is_none() {
                *system_identity =
                    Some(tapid_runner::dependency_toolchain_identity().map_err(containment_error)?);
            }
            let toolchain = tool_identity(approval, system_identity.as_ref().unwrap())?;
            if !impact.contains(key) {
                let evidence = graph.evidence(
                    key,
                    approval.dependencies(),
                    source_graph,
                    &checked,
                    &outputs,
                )?;
                let recipe = policy.derived_key(approval, &toolchain, &evidence);
                let previous = package.derived_hooks().iter().find(|output| output.hook() == hook && output.key() == recipe)
                        .ok_or_else(|| error(format!("rebuild prerequisite {key} {hook} has no matching output; run an approved online install first")))?;
                let snapshot = store.verified_lifecycle_snapshot(&recipe.parse().map_err(error)?, &previous.tree_digest().parse().map_err(error)?, previous.attestation())
                        .map_err(|_| error(format!("rebuild prerequisite {key} {hook} cannot authenticate; run an approved online install first")))?;
                snapshots.push(snapshot);
                outputs.push(previous.clone());
            }
        }
        // Impact outputs are only predictions here; the execution pass derives
        // dependant recipes from the actual staged result.
        checked.insert(
            key.clone(),
            if impact.contains(key) {
                package.derived_hooks().to_vec()
            } else {
                outputs
            },
        );
    }

    Ok(())
}

fn record_failure(
    outcome: &mut super::super::outcome::OperationOutcome,
    key: &str,
    package: &tapid_lockfile::LockedPackage,
    hook: Option<&str>,
    authorization: &'static str,
    reason: &'static str,
) {
    let report = outcome.lifecycle.as_mut().unwrap();
    let index = report.package(super::super::outcome::LifecycleIdentity::new(key, package));
    report.discovered(index, true);
    if let Some(hook) = hook {
        let hook = report.hook(index, hook);
        report.authorization(hook, authorization);
        report.disposition(hook, "blocked", Some(reason));
    }
}
