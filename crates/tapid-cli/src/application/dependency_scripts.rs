#[path = "dependency_inputs.rs"]
mod inputs;
#[path = "dependency_node_gyp.rs"]
mod node_gyp;

use super::outcome::{ErrorKind, OperationalError, Warning};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tapid_linker::NamedLayoutInput;
use tapid_lockfile::{DerivedHookOutput, Lockfile};
use tapid_runner::{DependencyLifecycleApproval, DependencyLifecyclePolicy};

#[path = "dependency_rebuild.rs"]
mod rebuild;
pub(crate) use rebuild::RebuildRequest;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExecutionOverrides {
    pub(crate) allow_unapproved: bool,
    pub(crate) without_containment: bool,
}
impl ExecutionOverrides {
    pub(crate) fn enabled(self) -> bool {
        self.allow_unapproved || self.without_containment
    }
    pub(crate) fn warnings(self) -> Vec<Warning> {
        let mut warnings = Vec::new();
        if self.allow_unapproved {
            warnings.push(Warning::UnapprovedDependencyScriptsAllowed);
        }
        if self.without_containment {
            warnings.push(Warning::DependencyScriptsWithoutContainment);
        }
        warnings
    }
}

fn error(message: impl std::fmt::Display) -> OperationalError {
    OperationalError::new(ErrorKind::InvalidRequest, message)
}
pub(super) fn load_policy(project: &Path) -> Result<DependencyLifecyclePolicy, OperationalError> {
    match crate::filesystem::read::read_bounded_file(
        &project.join("tapid.lifecycle.toml"),
        tapid_runner::config::MAX_CONFIG_BYTES,
    ) {
        Ok(bytes) => DependencyLifecyclePolicy::parse(&bytes).map_err(error),
        Err(crate::filesystem::read::ReadError::Io(e))
            if e.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(DependencyLifecyclePolicy::denied())
        }
        Err(_) => Err(error(
            "dependency lifecycle policy must be a regular file no larger than 1 MiB",
        )),
    }
}
fn commands(tree: &Path) -> Result<tapid_manifest::DependencyLifecyclePlan, OperationalError> {
    let root = crate::filesystem::tree::package_content_root(tree).map_err(error)?;
    let path = root.join("package.json");
    // Only an initially absent manifest is optional. A later disappearance is an error.
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(error(e)),
    }
    let bytes = crate::filesystem::read::read_bounded_file(&path, 1024 * 1024).map_err(|_| {
        error("dependency package.json must be a regular file no larger than 1 MiB")
    })?;
    let text = std::str::from_utf8(&bytes).map_err(error)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    tapid_manifest::dependency_lifecycle_plan(text, root.join("binding.gyp").is_file())
        .map_err(error)
}

/// Source trees remain immutable. Derived trees participate in the install's
/// existing store publication and project activation transaction.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    project: &Path,
    policy: &DependencyLifecyclePolicy,
    overrides: ExecutionOverrides,
    rebuild: Option<RebuildRequest<'_>>,
    retained_stages: &mut Vec<BuildStage>,
    lock: &mut Lockfile,
    input: &mut NamedLayoutInput,
    trees: &mut BTreeMap<String, PathBuf>,
    store: &tapid_store::Store,
    mut transaction: Option<&mut tapid_store::StoreTransaction>,
    activation: &crate::filesystem::activation::ActivationLock,
    snapshots: &mut Vec<tapid_store::VerifiedTreeSnapshot>,
    outcome: &mut super::outcome::OperationOutcome,
) -> Result<(), OperationalError> {
    outcome.lifecycle = Some(super::outcome::LifecycleReport::new());
    outcome
        .lifecycle
        .as_mut()
        .unwrap()
        .begin(lock.packages().len());
    let source_graph = lock.source_graph().to_json().map_err(error)?;
    let online = transaction.is_some();
    let cached = if online {
        fs::read_to_string(project.join("tapid.lock"))
            .ok()
            .and_then(|s| Lockfile::from_json(&s).ok())
    } else {
        Some(lock.clone())
    };
    let mut instance_indices = BTreeMap::new();
    for (index, instance) in input.instances.iter().enumerate() {
        let key = tapid_linker::InstanceKey::from(instance);
        if instance_indices.insert(key, index).is_some() {
            return Err(error("duplicate lifecycle package instance"));
        }
    }
    let mut completed = BTreeMap::<String, Vec<DerivedHookOutput>>::new();
    let dependency_inputs =
        if rebuild.is_none() && policy.approvals().is_empty() && !overrides.allow_unapproved {
            None
        } else {
            Some(inputs::DependencyInputs::new(lock)?)
        };
    let order = dependency_inputs.as_ref().map_or_else(
        || lock.packages().keys().cloned().collect(),
        |graph| graph.order.clone(),
    );
    let explicit_selection = rebuild.map(|request| request.select(lock)).transpose()?;
    let impact = explicit_selection.as_ref().map(|selected| {
        let graph = dependency_inputs.as_ref().expect("rebuild graph required");
        graph
            .order
            .iter()
            .filter(|key| {
                if selected.contains(*key) {
                    return true;
                }
                let package = &lock.packages()[*key];
                // Only potential dependency readers can be invalidated by a
                // selected output. Preflight still validates every exact hook
                // approval; this planning filter grants no execution authority.
                policy.approvals().iter().any(|approval| {
                    approval.dependencies()
                        && approval.package() == package.name()
                        && approval.version() == package.version()
                        && approval.archive_digest() == package.artifact_integrity()
                }) && !graph.closure(key).is_disjoint(selected)
            })
            .cloned()
            .collect::<BTreeSet<_>>()
    });
    if let Some(selected) = &explicit_selection {
        let report = outcome.lifecycle.as_mut().unwrap();
        report.explicitly_selected_packages = Some(selected.len());
        report.impacted_packages = Some(impact.as_ref().unwrap().len());
        report.reason = Some("rebuild_preflight_failed");
    }
    let mut system_identity = None;

    if let Some(selected) = explicit_selection.as_ref() {
        rebuild::preflight(
            selected,
            impact.as_ref().unwrap(),
            dependency_inputs.as_ref().unwrap(),
            lock,
            policy,
            trees,
            input,
            store,
            snapshots,
            &mut system_identity,
            &source_graph,
            outcome,
        )?;
    }
    for key in order {
        let package = lock.packages()[&key].clone();
        let identity = inputs::identity(&key)?;
        let index = *instance_indices
            .get(&identity)
            .filter(|&&index| trees.get(&key) == Some(&input.instances[index].tree.root))
            .ok_or_else(|| error("lifecycle package mapping lost"))?;
        let source = input.instances[index].tree.root.clone();
        let observation = outcome
            .lifecycle
            .as_mut()
            .unwrap()
            .package(super::outcome::LifecycleIdentity::new(&key, &package));
        if let Some(selected) = &explicit_selection
            && let Some(index) = observation
        {
            let package_report = &mut outcome.lifecycle.as_mut().unwrap().packages[index];
            package_report.rebuild_role = Some(if selected.contains(&key) {
                "selected"
            } else if impact.as_ref().unwrap().contains(&key) {
                "dependant"
            } else {
                "prerequisite"
            });
            package_report.force_requested = selected.contains(&key) && rebuild.unwrap().force;
        }
        let hooks = match commands(&source) {
            Ok(hooks) => {
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .discovered(observation, true);
                hooks
            }
            Err(discovery_error) => {
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .discovered(observation, false);
                outcome.lifecycle.as_mut().unwrap().reason = Some("discovery_failed");
                if policy
                    .approvals()
                    .iter()
                    .any(|approval| approval.package() == package.name())
                    || overrides.allow_unapproved
                {
                    return Err(discovery_error);
                }
                outcome.lifecycle_warning(Warning::DependencyLifecycleDiscoveryFailed {
                    package: Box::new(super::outcome::LifecycleIdentity::new(&key, &package)),
                });
                Default::default()
            }
        };
        let mut outputs = Vec::new();
        let mut current = source.clone();
        let mut private = None;
        let mut bypassed = false;
        let requires_install = hooks.requires_install();
        let commands = hooks.into_commands();
        let observations = commands
            .iter()
            .map(|(hook, _)| outcome.lifecycle.as_mut().unwrap().hook(observation, hook))
            .collect::<Vec<_>>();
        for ((hook, script), observed_hook) in commands.into_iter().zip(observations) {
            let selected = policy.approval_for(
                package.name(),
                package.version(),
                package.artifact_integrity(),
                &hook,
                &script,
            );
            let selected = if overrides.allow_unapproved {
                selected.ok().flatten()
            } else {
                selected.map_err(|e| {
                    let report = outcome.lifecycle.as_mut().unwrap();
                    report.authorization(observed_hook, "rejected");
                    report.disposition(observed_hook, "blocked", Some("approval_rejected"));
                    error(e)
                })?
            };
            let invocation_policy;
            let approval = if let Some(approval) = selected {
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .authorization(observed_hook, "exact_policy");
                approval
            } else if overrides.allow_unapproved
                && matches!(hook.as_str(), "preinstall" | "install" | "postinstall")
            {
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .authorization(observed_hook, "invocation_override");
                invocation_policy = invocation_approval(
                    &package,
                    &hook,
                    &script,
                    lock.workspace_packages().is_empty(),
                )?;
                &invocation_policy.approvals()[0]
            } else {
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .authorization(observed_hook, "not_approved");
                if requires_install && hook == "install" {
                    outcome.lifecycle.as_mut().unwrap().disposition(
                        observed_hook,
                        "blocked",
                        Some("required_hook_unapproved"),
                    );
                    return Err(error(format!(
                        "required dependency lifecycle hook install for {}@{} is not approved; approve its exact bytes in tapid.lifecycle.toml or explicitly accept the risk with --allow-unapproved-dependency-scripts; containment remains required unless --unsafe-no-dependency-sandbox is also explicitly selected",
                        package.name(),
                        package.version()
                    )));
                }
                outcome.lifecycle.as_mut().unwrap().disposition(
                    observed_hook,
                    "skipped",
                    Some("default_denial"),
                );
                outcome.lifecycle_warning(Warning::DependencyLifecycleHookSkipped {
                    package: Box::new(super::outcome::LifecycleIdentity::new(&key, &package)),
                    hook,
                });
                continue;
            };
            outcome.lifecycle.as_mut().unwrap().disposition(
                observed_hook,
                "blocked",
                Some("processing_stopped"),
            );
            if !package.has_declared_registry_integrity() {
                outcome.lifecycle.as_mut().unwrap().disposition(
                    observed_hook,
                    "blocked",
                    Some("missing_declared_integrity"),
                );
                return Err(error(
                    "approved dependency scripts require registry-declared archive integrity",
                ));
            }
            let projected = if approval.dependencies() {
                dependency_inputs.as_ref().unwrap().layout(&key, input)?
            } else {
                NamedLayoutInput {
                    instances: vec![],
                    root_dependencies: vec![],
                    dependency_edges: vec![],
                }
            };
            if overrides.enabled() {
                if !online {
                    return Err(error(
                        "dependency execution overrides cannot execute during frozen or offline replay",
                    ));
                }
                if private.is_none() {
                    private = Some(BuildStage::new(project, activation)?);
                }
                let stage = private.as_ref().unwrap();
                if !bypassed {
                    copy_build_source(&current, &stage.0.join("package"))?;
                }
                outcome.lifecycle.as_mut().unwrap().disposition(
                    observed_hook,
                    "blocked",
                    Some("execution_or_output_failed"),
                );
                let mut failure_reason = "execution_or_output_failed";
                run_hook(
                    stage,
                    approval,
                    &script,
                    &projected,
                    trees,
                    overrides.without_containment,
                    selected.is_none() && requires_install && hook == "install",
                    &mut failure_reason,
                )
                .inspect_err(|_| {
                    outcome.lifecycle.as_mut().unwrap().disposition(
                        observed_hook,
                        "blocked",
                        Some(failure_reason),
                    );
                })?;
                validate_output(&stage.0.join("package"))?;
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .disposition(observed_hook, "executed", None);
                current = stage.0.join("package");
                bypassed = true;
                continue;
            }
            if system_identity.is_none() {
                system_identity = Some(tapid_runner::dependency_toolchain_identity().map_err(
                    |cause| {
                        let reason = if cause.category()
                            == tapid_runner::ExecutionErrorCategory::UnsupportedContainment
                        {
                            "containment_unavailable"
                        } else {
                            "tool_verification_failed"
                        };
                        outcome.lifecycle.as_mut().unwrap().disposition(
                            observed_hook,
                            "blocked",
                            Some(reason),
                        );
                        containment_error(cause)
                    },
                )?);
            }
            let toolchain = tool_identity(approval, system_identity.as_ref().unwrap())
                .inspect_err(|_| {
                    outcome.lifecycle.as_mut().unwrap().disposition(
                        observed_hook,
                        "blocked",
                        Some("tool_verification_failed"),
                    );
                })?;
            let graph = dependency_inputs.as_ref().unwrap().evidence(
                &key,
                approval.dependencies(),
                &source_graph,
                &completed,
                &outputs,
            )?;
            let recipe = policy.derived_key(approval, &toolchain, &graph);
            let previous = cached
                .as_ref()
                .and_then(|l| l.packages().get(&key))
                .and_then(|p| {
                    p.derived_hooks()
                        .iter()
                        .find(|o| o.hook() == hook && o.key() == recipe)
                });
            let forced = rebuild.is_some_and(|request| request.force)
                && explicit_selection
                    .as_ref()
                    .is_some_and(|selected| selected.contains(&key));
            if let Some(previous) = previous.filter(|_| !forced)
                && let Ok(snapshot) = store.verified_lifecycle_snapshot(
                    &recipe.parse().map_err(error)?,
                    &previous.tree_digest().parse().map_err(error)?,
                    previous.attestation(),
                )
            {
                current = if let Some(transaction) = transaction.as_deref_mut() {
                    transaction
                        .stage_verified_tree(
                            &previous.tree_digest().parse().map_err(error)?,
                            &snapshot,
                        )
                        .map_err(error)?
                } else {
                    snapshot.path().to_owned()
                };
                snapshots.push(snapshot);
                outputs.push(previous.clone());
                outcome
                    .lifecycle
                    .as_mut()
                    .unwrap()
                    .disposition(observed_hook, "reused", None);
                continue;
            }
            if impact.as_ref().is_some_and(|impact| !impact.contains(&key)) {
                return Err(error(format!(
                    "rebuild prerequisite {key} {hook} changed; run an approved online install first"
                )));
            }
            if !online {
                outcome.lifecycle.as_mut().unwrap().disposition(
                    observed_hook,
                    "blocked",
                    Some("authenticated_output_missing_or_mismatched"),
                );
                return Err(error(format!(
                    "missing or mismatched verified lifecycle output for {}@{} {hook}; run an approved online install",
                    package.name(),
                    package.version()
                )));
            }
            if private.is_none() {
                private = Some(BuildStage::new(project, activation)?);
            }
            let stage = private.as_ref().unwrap();
            let build = stage.0.join("package");
            copy_build_source(&current, &build)?;
            outcome.lifecycle.as_mut().unwrap().disposition(
                observed_hook,
                "blocked",
                Some("execution_or_output_failed"),
            );
            let mut failure_reason = "execution_or_output_failed";
            run_hook(
                stage,
                approval,
                &script,
                &projected,
                trees,
                false,
                false,
                &mut failure_reason,
            )
            .inspect_err(|_| {
                outcome.lifecycle.as_mut().unwrap().disposition(
                    observed_hook,
                    "blocked",
                    Some(failure_reason),
                );
            })?;
            validate_output(&build)?;
            outcome
                .lifecycle
                .as_mut()
                .unwrap()
                .disposition(observed_hook, "executed", None);
            let digest = tapid_archive::canonical_tree_digest(&build).map_err(error)?;
            let staged = transaction
                .as_deref_mut()
                .unwrap()
                .stage_verified_tree(&digest.parse().map_err(error)?, &build)
                .map_err(error)?;
            let attestation = store
                .attest_lifecycle_output(
                    &recipe.parse().map_err(error)?,
                    &digest.parse().map_err(error)?,
                )
                .map_err(error)?;
            outputs.push(
                DerivedHookOutput::new(&hook, &recipe, &digest, &attestation).map_err(error)?,
            );
            current = staged;
        }
        if !online && cached.as_ref().unwrap().packages()[&key].derived_hooks() != outputs {
            return Err(error(format!(
                "lifecycle approval was removed or changed for {key}; regenerate tapid.lock online"
            )));
        }
        if bypassed {
            let digest = tapid_archive::canonical_tree_digest(&current).map_err(error)?;
            input.instances[index].tree =
                tapid_linker::VerifiedTreeReference::new(&digest, &current).map_err(error)?;
            trees.insert(key.clone(), current);
            retained_stages.push(private.take().unwrap());
        } else if let Some(output) = outputs.last() {
            input.instances[index].tree =
                tapid_linker::VerifiedTreeReference::new(output.tree_digest(), &current)
                    .map_err(error)?;
            trees.insert(key.clone(), current);
        }
        lock.set_derived_hooks(&key, outputs.clone())
            .map_err(error)?;
        completed.insert(key, outputs);
    }
    outcome.lifecycle.as_mut().unwrap().finish();
    Ok(())
}

pub(super) struct BuildStage(PathBuf);
impl BuildStage {
    fn new(
        project: &Path,
        activation: &crate::filesystem::activation::ActivationLock,
    ) -> Result<Self, OperationalError> {
        let stage = Self(activation.create_stage(project).map_err(error)?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stage.0, fs::Permissions::from_mode(0o700)).map_err(error)?;
        }
        Ok(stage)
    }
}
impl Drop for BuildStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn tool_bytes(tool: &tapid_runner::LifecycleTool) -> Result<Vec<u8>, OperationalError> {
    Ok(verified_tool(tool)?.2)
}
fn verified_tool(
    tool: &tapid_runner::LifecycleTool,
) -> Result<(PathBuf, fs::File, Vec<u8>), OperationalError> {
    let path = fs::canonicalize(tool.path()).map_err(error)?;
    let mut file = fs::File::open(&path).map_err(error)?;
    let bytes = read_executable(&mut file)?;
    if format!("sha256-{:x}", Sha256::digest(&bytes)) != tool.digest() {
        return Err(error(format!(
            "lifecycle tool digest mismatch for {}",
            tool.name()
        )));
    }
    Ok((path, file, bytes))
}
fn executable_bytes(path: &Path) -> Result<Vec<u8>, OperationalError> {
    read_executable(&mut fs::File::open(path).map_err(error)?)
}
fn read_executable(file: &mut fs::File) -> Result<Vec<u8>, OperationalError> {
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || metadata.len() > 512 * 1024 * 1024 {
        return Err(error(
            "lifecycle tool must be a regular executable no larger than 512 MiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 512 * 1024 * 1024 {
        return Err(error("lifecycle tool exceeds 512 MiB"));
    }
    Ok(bytes)
}
fn tool_identity(
    approval: &DependencyLifecycleApproval,
    system: &str,
) -> Result<String, OperationalError> {
    let mut hash = Sha256::new();
    hash.update(system.as_bytes());
    for tool in approval.tools() {
        tool_bytes(tool)?;
        for value in [tool.name(), tool.path(), tool.digest()] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    Ok(format!("sha256-{:x}", hash.finalize()))
}
fn copy_build_source(source: &Path, build: &Path) -> Result<(), OperationalError> {
    if build.exists() {
        fs::remove_dir_all(build).map_err(error)?;
    }
    let content = crate::filesystem::tree::package_content_root(source).map_err(error)?;
    crate::filesystem::tree::copy_tree_contents(&content, build).map_err(error)?;
    let marker = build.join(".tapid-tree");
    if marker.exists() {
        fs::remove_file(marker).map_err(error)?;
    }
    Ok(())
}

// Reuse the checked policy parser for invocation-local authorization. No policy
// is written to the project, and these outputs never receive cache attestations.
fn invocation_approval(
    package: &tapid_lockfile::LockedPackage,
    hook: &str,
    script: &str,
    dependencies: bool,
) -> Result<DependencyLifecyclePolicy, OperationalError> {
    #[cfg(unix)]
    let (shell_name, shell_path) = ("sh", fs::canonicalize("/bin/sh").map_err(error)?);
    #[cfg(windows)]
    let (shell_name, shell_path) = (
        "cmd",
        crate::run::windows_system_directory()
            .map_err(error)?
            .join("cmd.exe"),
    );
    let mut paths = vec![(shell_name, shell_path)];
    if let Ok(node) = crate::run::discover_node_runtime(std::env::var_os("PATH").as_deref()) {
        paths.push(("node", node));
    }
    let mut tools = Vec::new();
    for (name, path) in paths {
        let bytes = executable_bytes(&path)?;
        tools.push(serde_json::json!({"name":name, "path":path.to_str().ok_or_else(|| error("lifecycle tool path must be UTF-8"))?, "digest":format!("sha256-{:x}", Sha256::digest(&bytes))}));
    }
    let document = serde_json::json!({"schema":1,"approvals":[{
        "package":package.name(),"version":package.version(),"archive-digest":package.artifact_integrity(),
        "hook":hook,"script-digest":format!("sha256-{:x}", Sha256::digest(script.as_bytes())),
        "system-toolchain":true,"dependencies":dependencies,"read":["."],"write":["."],"network":false,"environment":{},
        "timeout-seconds":60,"max-output-bytes":16777216,"max-processes":128,"max-memory-bytes":536870912,"tools":tools
    }]});
    let document = toml::to_string(&document).map_err(error)?;
    DependencyLifecyclePolicy::parse(document.as_bytes()).map_err(error)
}
#[allow(clippy::too_many_arguments)]
fn run_hook(
    stage: &BuildStage,
    approval: &DependencyLifecycleApproval,
    script: &str,
    input: &NamedLayoutInput,
    trees: &BTreeMap<String, PathBuf>,
    without_containment: bool,
    implicit_node_gyp: bool,
    failure_reason: &mut &'static str,
) -> Result<(), OperationalError> {
    let tools = stage.0.join(".tools");
    if tools.exists() {
        fs::remove_dir_all(&tools).map_err(error)?;
    }
    fs::create_dir(&tools).map_err(error)?;
    *failure_reason = "tool_verification_failed";
    let prepared_tools = prepare_tools(&tools, approval.tools(), without_containment)?;
    *failure_reason = "execution_or_output_failed";
    let script = if implicit_node_gyp {
        let node = approval
            .tools()
            .iter()
            .find(|tool| tool.name() == "node")
            .ok_or_else(|| {
                error("implicit node-gyp builds require an executable Node runtime on PATH")
            })?;
        node_gyp::prepare(&tools, Path::new(node.path()))?;
        node_gyp::COMMAND
    } else {
        script
    };
    let prefix = |s: &str| {
        if s == "." {
            "package".into()
        } else {
            format!("package/{s}")
        }
    };
    let mut read = approval
        .policy()
        .filesystem()
        .read()
        .iter()
        .map(|s| prefix(s))
        .collect::<Vec<_>>();
    let write = approval
        .policy()
        .filesystem()
        .write()
        .iter()
        .map(|s| prefix(s))
        .collect::<Vec<_>>();
    for path in &write {
        let path = stage.0.join(path);
        if !path.exists() {
            fs::create_dir_all(&path).map_err(error)?;
        }
        #[cfg(windows)]
        if !without_containment {
            tapid_runner::prepare_private_dependency_write_tree(&path).map_err(error)?;
        }
    }
    if approval.dependencies() {
        let plan = tapid_linker::plan_named_layout(
            tapid_linker::ManagedRoot::new(&stage.0).map_err(error)?,
            input.clone(),
            super::replay::current_platform(),
        )
        .map_err(error)?;
        if stage.0.join("node_modules").exists() {
            fs::remove_dir_all(stage.0.join("node_modules")).map_err(error)?;
        }
        crate::filesystem::tree::materialize_stage_with_workspace_links(
            &stage.0,
            &plan,
            &input.instances,
            trees,
            false,
            &tapid_linker::WorkspaceLinkPlan::default(),
            stage
                .0
                .parent()
                .ok_or_else(|| error("stage has no parent"))?,
            |_, _| {},
        )
        .map_err(error)?;
        read.push("node_modules".into());
    }
    let filesystem = tapid_runner::FilesystemPolicy::new(read, write).map_err(error)?;
    let policy = tapid_runner::SandboxPolicy::new(
        tapid_runner::SandboxMode::Required,
        filesystem,
        approval.policy().network(),
        approval.policy().environment().to_vec(),
        true,
        approval.policy().limits().clone(),
    )
    .map_err(error)?;
    let shell = if cfg!(windows) && approval.tools().iter().any(|t| t.name() == "cmd") {
        "cmd"
    } else {
        "sh"
    };
    let shell_args = if shell == "cmd" {
        vec![
            "/D".to_owned(),
            "/S".into(),
            "/C".into(),
            format!("\"{script}\""),
        ]
    } else {
        vec!["-c".into(), script.into(), "tapid-lifecycle".into()]
    };
    let shell_tool = approval
        .tools()
        .iter()
        .find(|tool| tool.name() == shell)
        .ok_or_else(|| error("lifecycle policy does not pin a shell usable on this platform"))?;
    let program = prepared_tools.get(shell_tool.name()).unwrap();
    let mut builder = tapid_runner::ExecutionRequest::builder(program)
        .args(shell_args)
        .windows_verbatim_arguments(shell == "cmd")
        .project_root(&stage.0)
        .working_directory(stage.0.join("package"))
        .policy(policy)
        .allow_process_memory_stats(!without_containment && approval.process_memory_stats())
        .envs(approval.environment().iter());
    let mut search_paths = Vec::new();
    if without_containment {
        search_paths = tool_search_paths(&prepared_tools)?;
    } else {
        search_paths.push(fs::canonicalize(&tools).map_err(error)?);
    }
    if approval.dependencies() && stage.0.join("node_modules/.bin").is_dir() {
        search_paths.push(stage.0.join("node_modules/.bin"));
    }
    #[cfg(unix)]
    {
        for path in ["/usr/bin", "/bin"] {
            let path = fs::canonicalize(path).map_err(error)?;
            if !search_paths.contains(&path) {
                search_paths.push(path);
            }
        }
    }
    #[cfg(windows)]
    {
        let path = crate::run::windows_system_directory().map_err(error)?;
        if !search_paths.contains(&path) {
            search_paths.push(path);
        }
    }
    builder = builder.executable_search_paths(search_paths);
    if let Some(node) = approval.tools().iter().find(|tool| tool.name() == "node") {
        builder = builder.trusted_node_runtime(prepared_tools.get(node.name()).unwrap());
    }
    let request = builder.build().map_err(error)?;
    if without_containment {
        let capture = stage.0.join(".capture");
        if capture.exists() {
            fs::remove_dir_all(&capture).map_err(error)?;
        }
        fs::create_dir(&capture).map_err(error)?;
        let termination = tapid_runner::execute_uncontained(&request, &capture).map_err(error)?;
        if termination != tapid_runner::Termination::Exited(0) {
            let mut detail = Vec::new();
            fs::File::open(capture.join("stderr"))
                .and_then(|f| f.take(4096).read_to_end(&mut detail))
                .map_err(error)?;
            return Err(error(format!(
                "dependency lifecycle hook {} failed without containment: {termination:?}: {}",
                approval.hook(),
                String::from_utf8_lossy(&detail)
            )));
        }
        return Ok(());
    }
    let outcome = tapid_runner::execute(&request).map_err(|error| {
        if error.category() == tapid_runner::ExecutionErrorCategory::UnsupportedContainment {
            *failure_reason = "containment_unavailable";
        }
        containment_error(error)
    })?;

    if outcome.termination() != &tapid_runner::Termination::Exited(0)
        || outcome.completion().cleanup_confidence()
            != tapid_runner::CleanupConfidence::KernelOwnedComplete
    {
        return Err(error(format!(
            "dependency lifecycle hook {} failed: {:?}",
            approval.hook(),
            outcome.termination()
        )));
    }
    Ok(())
}
fn prepare_tools(
    destination: &Path,
    tools: &[tapid_runner::LifecycleTool],
    without_containment: bool,
) -> Result<BTreeMap<String, PathBuf>, OperationalError> {
    let mut paths = BTreeMap::new();
    for tool in tools {
        let (source, file, bytes) = verified_tool(tool)?;
        // macOS system executables cannot always be relocated without changing
        // their signature. A canonical source on the kernel read-only root is
        // immutable to the caller. Administrators remounting/replacing host
        // volumes remain outside the documented trusted-host boundary.
        #[cfg(target_os = "macos")]
        let retain_source = without_containment && readonly_tool_volume(&file)?;
        #[cfg(not(target_os = "macos"))]
        let retain_source = {
            let _ = (&file, without_containment);
            false
        };
        let path = if retain_source {
            source
        } else {
            let path = destination.join(tool.executable_name());
            use std::io::Write;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .and_then(|mut file| file.write_all(&bytes))
                .map_err(error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).map_err(error)?;
            }
            path
        };
        paths.insert(tool.name().to_owned(), path);
    }
    Ok(paths)
}
#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "macOS trusted tool verification requires fstatfs mount flags"
)]
fn readonly_tool_volume(file: &fs::File) -> Result<bool, OperationalError> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: the source file keeps the descriptor alive and status is writable.
    if unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    // SAFETY: successful fstatfs initialized status.
    Ok(immutable_system_tool_mount(
        unsafe { status.assume_init() }.f_flags,
    ))
}
#[cfg(target_os = "macos")]
fn immutable_system_tool_mount(flags: u32) -> bool {
    let required = (libc::MNT_RDONLY | libc::MNT_ROOTFS) as u32;
    flags & required == required
}
fn tool_search_paths(tools: &BTreeMap<String, PathBuf>) -> Result<Vec<PathBuf>, OperationalError> {
    let mut paths = Vec::new();
    for tool in tools
        .iter()
        .filter(|(name, _)| *name == "node")
        .chain(tools.iter().filter(|(name, _)| *name != "node"))
    {
        let parent = tool
            .1
            .parent()
            .ok_or_else(|| error("lifecycle tool has no parent directory"))?;
        let canonical = fs::canonicalize(parent).map_err(error)?;
        if !paths.contains(&canonical) {
            paths.push(canonical);
        }
    }
    Ok(paths)
}
fn containment_error(cause: tapid_runner::ExecutionError) -> OperationalError {
    if cause.category() == tapid_runner::ExecutionErrorCategory::UnsupportedContainment {
        error(format!(
            "{cause}; required dependency containment is unavailable; configure a supported host or explicitly accept host access with --unsafe-no-dependency-sandbox for this invocation; this flag does not grant hook approval"
        ))
    } else {
        error(cause)
    }
}
fn validate_output(root: &Path) -> Result<(), OperationalError> {
    tapid_archive::validate_tree(root, tapid_archive::ValidationLimits::default()).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_mapping_requires_the_exact_peer_and_platform_context() {
        let project = tapid_test_support::TempProject::new("lifecycle-context").unwrap();
        project
            .write("package.json", br#"{"name":"plugin","version":"1.0.0"}"#)
            .unwrap();
        let digest = tapid_archive::canonical_tree_digest(project.path()).unwrap();
        let peer = tapid_core::PeerContext::default()
            .with("react".parse().unwrap(), "1.0.0".parse().unwrap());
        let platform = tapid_core::PlatformContext::new(Some("linux"), Some("x64"), None).unwrap();
        let package = tapid_lockfile::LockedPackage::new_with_context_and_provenance(
            "https://registry.npmjs.org",
            "plugin",
            "1.0.0",
            &format!("sha512-{}==", "A".repeat(86)),
            &digest,
            (&peer, &platform),
            tapid_lockfile::RegistryIntegrityProvenance::RegistryDeclared,
        )
        .unwrap();
        let key = package.key();
        let mut lock = Lockfile::new(&digest).unwrap();
        lock.insert_package(package).unwrap();
        lock.set_roots([key.clone()]).unwrap();
        let legacy_json = lock
            .to_json()
            .unwrap()
            .replace("platform=os=linux;cpu=x64;libc=", "platform=-")
            .replace("\"os=linux;cpu=x64;libc=\"", "\"\"");
        for (actual_peer, actual_platform, accepted, legacy) in [
            (
                tapid_core::PeerContext::default(),
                platform.clone(),
                false,
                false,
            ),
            (
                peer.clone(),
                tapid_core::PlatformContext::new(None, None, None).unwrap(),
                false,
                false,
            ),
            (peer.clone(), platform.clone(), true, false),
            (
                peer.clone(),
                tapid_core::PlatformContext::new(None, None, None).unwrap(),
                true,
                true,
            ),
        ] {
            let mut case_lock = if legacy {
                Lockfile::from_json(&legacy_json).unwrap()
            } else {
                lock.clone()
            };
            let key = case_lock.packages().keys().next().unwrap().clone();
            let mut input = NamedLayoutInput {
                instances: vec![tapid_linker::PackageInstance {
                    id: tapid_core::PackageInstanceId::new(
                        "https://registry.npmjs.org".parse().unwrap(),
                        "plugin".parse().unwrap(),
                        "1.0.0".parse().unwrap(),
                    ),
                    peer_context: actual_peer,
                    platform_context: actual_platform,
                    tree: tapid_linker::VerifiedTreeReference::new(&digest, project.path())
                        .unwrap(),
                }],
                root_dependencies: Vec::new(),
                dependency_edges: Vec::new(),
            };
            let mut trees = [(key.clone(), project.path().to_path_buf())].into();
            let guard =
                crate::filesystem::activation::ActivationLock::acquire(project.path()).unwrap();
            let result = apply(
                project.path(),
                &DependencyLifecyclePolicy::denied(),
                ExecutionOverrides::default(),
                None,
                &mut Vec::new(),
                &mut case_lock,
                &mut input,
                &mut trees,
                &tapid_store::Store::new(project.path().join("store")),
                None,
                &guard,
                &mut Vec::new(),
                &mut super::super::outcome::OperationOutcome::unchanged(project.path()),
            );
            if accepted {
                result.unwrap();
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("lifecycle package mapping lost")
                );
            }
        }
    }
    #[test]
    fn uncontained_search_reserves_node_before_the_shell_directory() {
        let project = tapid_test_support::TempProject::new("uncontained-tool-search").unwrap();
        project.write("shell/sh", b"shell").unwrap();
        project.write("runtime/node", b"node").unwrap();
        let digest = format!("sha256-{}", "0".repeat(64));
        let document = toml::to_string(&serde_json::json!({"schema":1,"approvals":[{
            "package":"demo","version":"1.0.0","archive-digest":format!("sha512-{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [0u8; 64])),
            "hook":"install","script-digest":digest,"system-toolchain":true,
            "read":["."],"write":["."],"network":false,"environment":{},
            "timeout-seconds":5,"max-output-bytes":1024,"max-processes":32,"max-memory-bytes":134217728,
            "tools":[
                {"name":"sh","path":project.path().join("shell/sh"),"digest":digest},
                {"name":"node","path":project.path().join("runtime/node"),"digest":digest}
            ]
        }]})).unwrap();
        let policy = DependencyLifecyclePolicy::parse(document.as_bytes()).unwrap();
        assert_eq!(
            tool_search_paths(
                &policy.approvals()[0]
                    .tools()
                    .iter()
                    .map(|tool| (tool.name().to_owned(), PathBuf::from(tool.path())))
                    .collect()
            )
            .unwrap(),
            vec![
                fs::canonicalize(project.path().join("runtime")).unwrap(),
                fs::canonicalize(project.path().join("shell")).unwrap(),
            ]
        );
    }
    #[cfg(unix)]
    #[test]
    fn uncontained_tools_execute_verified_bytes_after_source_replacement() {
        use std::os::unix::fs::PermissionsExt;
        let project = tapid_test_support::TempProject::new("uncontained-tool-replacement").unwrap();
        let bytes = b"#!/bin/sh\nprintf original";
        project.write("sh", bytes).unwrap();
        fs::set_permissions(project.path().join("sh"), fs::Permissions::from_mode(0o700)).unwrap();
        let digest = format!("sha256-{:x}", Sha256::digest(bytes));
        let document = toml::to_string(&serde_json::json!({"schema":1,"approvals":[{
            "package":"demo","version":"1.0.0","archive-digest":format!("sha512-{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [0u8; 64])),
            "hook":"install","script-digest":digest,"system-toolchain":true,
            "read":["."],"write":["."],"network":false,"environment":{},
            "timeout-seconds":5,"max-output-bytes":1024,"max-processes":32,"max-memory-bytes":134217728,
            "tools":[{"name":"sh","path":project.path().join("sh"),"digest":digest}]
        }]})).unwrap();
        let policy = DependencyLifecyclePolicy::parse(document.as_bytes()).unwrap();
        let directory = project.path().join("private-tools");
        fs::create_dir(&directory).unwrap();
        let prepared = prepare_tools(&directory, policy.approvals()[0].tools(), true).unwrap();
        fs::rename(project.path().join("sh"), project.path().join("old-sh")).unwrap();
        project.write("sh", b"#!/bin/sh\nprintf replaced").unwrap();
        fs::set_permissions(project.path().join("sh"), fs::Permissions::from_mode(0o700)).unwrap();
        let output = std::process::Command::new(&prepared["sh"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"original");
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn readonly_removable_mount_does_not_qualify_as_an_immutable_system_tool() {
        assert!(!immutable_system_tool_mount(libc::MNT_RDONLY as u32));
        assert!(!immutable_system_tool_mount(libc::MNT_ROOTFS as u32));
        assert!(immutable_system_tool_mount(
            (libc::MNT_RDONLY | libc::MNT_ROOTFS) as u32
        ));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_tool_source_must_be_on_a_readonly_volume() {
        let shell = fs::File::open("/bin/sh").unwrap();
        assert!(readonly_tool_volume(&shell).unwrap());
        let project = tapid_test_support::TempProject::new("mutable-tool-volume").unwrap();
        project.write("tool", b"mutable").unwrap();
        assert!(
            !readonly_tool_volume(&fs::File::open(project.path().join("tool")).unwrap()).unwrap()
        );
    }
    #[test]
    fn lifecycle_discovery_accepts_a_leading_bom_without_changing_script_bytes() {
        let project = tapid_test_support::TempProject::new("lifecycle-discovery-bom").unwrap();
        project
            .write(
                "package.json",
                "\u{feff}{\"scripts\":{\"install\":\"  exact\\ncommand  \"}}".as_bytes(),
            )
            .unwrap();
        assert_eq!(
            commands(project.path()).unwrap().into_commands(),
            vec![("install".into(), "  exact\ncommand  ".into())]
        );
    }
    #[test]
    fn lifecycle_output_rejects_portable_case_collisions() {
        let project = tapid_test_support::TempProject::new("lifecycle-output-case").unwrap();
        project.write("native", b"one").unwrap();
        project.write("NATIVE", b"two").unwrap();
        #[cfg(target_os = "linux")]
        assert!(validate_output(project.path()).is_err());
        #[cfg(unix)]
        {
            fs::write(project.path().join(r"C:\escape"), b"invalid portable path").unwrap();
            assert!(validate_output(project.path()).is_err());
        }
    }
}
