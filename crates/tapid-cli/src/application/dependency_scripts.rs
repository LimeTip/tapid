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
    match crate::commands::run::read_bounded_config_file(
        &project.join("tapid.lifecycle.toml"),
        tapid_runner::config::MAX_CONFIG_BYTES,
    ) {
        Ok(bytes) => DependencyLifecyclePolicy::parse(&bytes).map_err(error),
        Err(crate::commands::run::ConfigReadError::Io(e))
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
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(error(e)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(error(
            "dependency package.json must be a regular file no larger than 1 MiB",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(error)?;
    if bytes.len() > 1024 * 1024 {
        return Err(error("dependency package.json exceeds 1 MiB"));
    }
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
    retained_stages: &mut Vec<BuildStage>,
    lock: &mut Lockfile,
    input: &mut NamedLayoutInput,
    trees: &mut BTreeMap<String, PathBuf>,
    store: &tapid_store::Store,
    mut transaction: Option<&mut tapid_store::StoreTransaction>,
    activation: &crate::filesystem::activation::ActivationLock,
) -> Result<Vec<Warning>, OperationalError> {
    let source_graph = lock.source_graph().to_json().map_err(error)?;
    let online = transaction.is_some();
    let cached = if online {
        fs::read_to_string(project.join("tapid.lock"))
            .ok()
            .and_then(|s| Lockfile::from_json(&s).ok())
    } else {
        Some(lock.clone())
    };
    let mut warnings = Vec::new();
    let mut completed = BTreeMap::<String, Vec<DerivedHookOutput>>::new();
    let order = if policy.approvals().is_empty() && !overrides.enabled() {
        lock.packages().keys().cloned().collect()
    } else {
        dependency_order(lock)?
    };
    let mut snapshots = SnapshotGuard {
        paths: Vec::new(),
        retained: BTreeSet::new(),
    };
    // The caller owns source snapshots and transaction staging trees.
    // This guard owns only snapshots acquired while selecting derived outputs.
    let mut system_identity = None;
    for key in order {
        let package = lock.packages()[&key].clone();
        let index = input
            .instances
            .iter()
            .position(|i| {
                i.id.name.as_str() == package.name()
                    && i.id.version.to_string() == package.version()
                    && i.id.registry.as_str() == package.registry()
                    && trees.get(&key) == Some(&i.tree.root)
            })
            .ok_or_else(|| error("lifecycle package mapping lost"))?;
        let source = input.instances[index].tree.root.clone();
        let hooks = match commands(&source) {
            Ok(hooks) => hooks,
            Err(discovery_error) => {
                if policy
                    .approvals()
                    .iter()
                    .any(|approval| approval.package() == package.name())
                    || overrides.enabled()
                {
                    return Err(discovery_error);
                }
                warnings.push(Warning::DependencyLifecycleDiscoveryFailed {
                    package: format!("{}@{}", package.name(), package.version()),
                });
                Default::default()
            }
        };
        let mut outputs = Vec::new();
        let mut current = source.clone();
        let mut private = None;
        let mut bypassed = false;
        let requires_install = hooks.requires_install();
        for (hook, script) in hooks.into_commands() {
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
                selected.map_err(error)?
            };
            let invocation_policy;
            let approval = if let Some(approval) = selected {
                approval
            } else if overrides.allow_unapproved
                && matches!(hook.as_str(), "preinstall" | "install" | "postinstall")
            {
                invocation_policy = invocation_approval(&package, &hook, &script)?;
                &invocation_policy.approvals()[0]
            } else {
                if requires_install && hook == "install" {
                    return Err(error(format!(
                        "required dependency lifecycle hook install for {}@{} is not approved; approve its exact bytes in tapid.lifecycle.toml or explicitly accept the risk with --allow-unapproved-dependency-scripts; containment remains required unless --unsafe-no-dependency-sandbox is also explicitly selected",
                        package.name(),
                        package.version()
                    )));
                }
                warnings.push(Warning::DependencyLifecycleHookSkipped {
                    package: format!("{}@{}", package.name(), package.version()),
                    hook,
                });
                continue;
            };
            if !package.has_declared_registry_integrity() {
                return Err(error(
                    "approved dependency scripts require registry-declared archive integrity",
                ));
            }
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
                run_hook(
                    stage,
                    approval,
                    &script,
                    input,
                    trees,
                    lock,
                    overrides.without_containment,
                )?;
                validate_output(&stage.0.join("package"))?;
                current = stage.0.join("package");
                bypassed = true;
                continue;
            }
            if system_identity.is_none() {
                system_identity =
                    Some(tapid_runner::dependency_toolchain_identity().map_err(containment_error)?);
            }
            let toolchain = tool_identity(approval, system_identity.as_ref().unwrap())?;
            let graph =
                serde_json::to_string(&(&source_graph, &completed, &outputs)).map_err(error)?;
            let recipe = policy.derived_key(approval, &toolchain, &graph);
            let previous = cached
                .as_ref()
                .and_then(|l| l.packages().get(&key))
                .and_then(|p| {
                    p.derived_hooks()
                        .iter()
                        .find(|o| o.hook() == hook && o.key() == recipe)
                });
            if let Some(previous) = previous
                && let Ok(snapshot) = store.verified_lifecycle_snapshot(
                    &recipe.parse().map_err(error)?,
                    &previous.tree_digest().parse().map_err(error)?,
                    previous.attestation(),
                )
            {
                snapshots.paths.push(snapshot.clone());
                current = if let Some(transaction) = transaction.as_deref_mut() {
                    transaction
                        .stage_verified_tree(
                            &previous.tree_digest().parse().map_err(error)?,
                            &snapshot,
                        )
                        .map_err(error)?
                } else {
                    snapshot
                };
                outputs.push(previous.clone());
                continue;
            }
            if !online {
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
            run_hook(stage, approval, &script, input, trees, lock, false)?;
            validate_output(&build)?;
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
    if !online {
        snapshots.retained.extend(trees.values().cloned());
    }
    Ok(warnings)
}

fn dependency_order(lock: &Lockfile) -> Result<Vec<String>, OperationalError> {
    fn visit(
        key: &str,
        lock: &Lockfile,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<(), OperationalError> {
        if done.contains(key) {
            return Ok(());
        }
        if active.len() >= 1024 || !active.insert(key.into()) {
            return Err(error(
                "approved dependency lifecycle graphs must be acyclic and no deeper than 1024 packages",
            ));
        }
        for child in lock.packages()[key].dependencies().values() {
            if lock.packages().contains_key(child) {
                visit(child, lock, active, done, order)?;
            }
        }
        active.remove(key);
        done.insert(key.into());
        order.push(key.into());
        Ok(())
    }
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    let mut active = BTreeSet::new();
    for key in lock.packages().keys() {
        visit(key, lock, &mut active, &mut done, &mut order)?;
    }
    Ok(order)
}
struct SnapshotGuard {
    paths: Vec<PathBuf>,
    retained: BTreeSet<PathBuf>,
}
impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            if !self.retained.contains(path) {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
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
    let bytes = executable_bytes(Path::new(tool.path()))?;
    if format!("sha256-{:x}", Sha256::digest(&bytes)) != tool.digest() {
        return Err(error(format!(
            "lifecycle tool digest mismatch for {}",
            tool.name()
        )));
    }
    Ok(bytes)
}
fn executable_bytes(path: &Path) -> Result<Vec<u8>, OperationalError> {
    let path = fs::canonicalize(path).map_err(error)?;
    let metadata = fs::metadata(&path).map_err(error)?;
    if !metadata.is_file() || metadata.len() > 512 * 1024 * 1024 {
        return Err(error(
            "lifecycle tool must be a regular executable no larger than 512 MiB",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(512 * 1024 * 1024 + 1).read_to_end(&mut bytes))
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
        "system-toolchain":true,"dependencies":true,"read":["."],"write":["."],"network":false,"environment":{},
        "timeout-seconds":60,"max-output-bytes":16777216,"max-processes":128,"max-memory-bytes":536870912,"tools":tools
    }]});
    let document = toml::to_string(&document).map_err(error)?;
    DependencyLifecyclePolicy::parse(document.as_bytes()).map_err(error)
}
fn run_hook(
    stage: &BuildStage,
    approval: &DependencyLifecycleApproval,
    script: &str,
    input: &NamedLayoutInput,
    trees: &BTreeMap<String, PathBuf>,
    lock: &Lockfile,
    without_containment: bool,
) -> Result<(), OperationalError> {
    let tools = stage.0.join(".tools");
    if tools.exists() {
        fs::remove_dir_all(&tools).map_err(error)?;
    }
    fs::create_dir(&tools).map_err(error)?;
    for tool in approval.tools() {
        if without_containment {
            tool_bytes(tool)?;
            continue;
        }
        let path = tools.join(tool.executable_name());
        fs::write(&path, tool_bytes(tool)?).map_err(error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o500)).map_err(error)?;
        }
    }
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
            fs::create_dir_all(path).map_err(error)?;
        }
    }
    if approval.dependencies() {
        if !lock.workspace_packages().is_empty() {
            return Err(error(
                "dependency lifecycle scripts cannot read live workspace packages; use published verified dependencies",
            ));
        }
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
    let program = if without_containment {
        PathBuf::from(shell_tool.path())
    } else {
        tools.join(shell_tool.executable_name())
    };
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
        search_paths = uncontained_search_paths(approval.tools())?;
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
        builder = builder.trusted_node_runtime(if without_containment {
            PathBuf::from(node.path())
        } else {
            tools.join(node.executable_name())
        });
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
    let outcome = tapid_runner::execute(&request).map_err(containment_error)?;
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
fn uncontained_search_paths(
    tools: &[tapid_runner::LifecycleTool],
) -> Result<Vec<PathBuf>, OperationalError> {
    let mut paths = Vec::new();
    for tool in tools
        .iter()
        .filter(|tool| tool.name() == "node")
        .chain(tools.iter().filter(|tool| tool.name() != "node"))
    {
        let parent = Path::new(tool.path())
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
            uncontained_search_paths(policy.approvals()[0].tools()).unwrap(),
            vec![
                fs::canonicalize(project.path().join("runtime")).unwrap(),
                fs::canonicalize(project.path().join("shell")).unwrap(),
            ]
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
