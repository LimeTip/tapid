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
fn commands(tree: &Path) -> Result<Vec<(String, String)>, OperationalError> {
    let root = crate::filesystem::tree::package_content_root(tree).map_err(error)?;
    let path = root.join("package.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
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
    tapid_manifest::dependency_lifecycle_commands(text, root.join("binding.gyp").is_file())
        .map_err(error)
}

/// Source trees remain immutable. Derived trees participate in the install's
/// existing store publication and project activation transaction.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    project: &Path,
    policy: &DependencyLifecyclePolicy,
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
    let order = if policy.approvals().is_empty() {
        lock.packages().keys().cloned().collect()
    } else {
        dependency_order(lock)?
    };
    let mut snapshots = SnapshotGuard {
        paths: Vec::new(),
        retained: BTreeSet::new(),
    };
    if !online {
        snapshots.paths.extend(trees.values().cloned());
    }
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
                {
                    return Err(discovery_error);
                }
                warnings.push(Warning::DependencyLifecycleDiscoveryFailed {
                    package: format!("{}@{}", package.name(), package.version()),
                });
                Vec::new()
            }
        };
        let mut outputs = Vec::new();
        let mut current = source.clone();
        let mut private = None;
        for (hook, script) in hooks {
            let Some(approval) = policy
                .approval_for(
                    package.name(),
                    package.version(),
                    package.artifact_integrity(),
                    &hook,
                    &script,
                )
                .map_err(error)?
            else {
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
            if system_identity.is_none() {
                system_identity =
                    Some(tapid_runner::dependency_toolchain_identity().map_err(error)?);
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
            if build.exists() {
                fs::remove_dir_all(&build).map_err(error)?;
            }
            let content = crate::filesystem::tree::package_content_root(&current).map_err(error)?;
            crate::filesystem::tree::copy_tree_contents(&content, &build).map_err(error)?;
            let marker = build.join(".tapid-tree");
            if marker.exists() {
                fs::remove_file(marker).map_err(error)?;
            }
            run_hook(stage, approval, &script, input, trees, lock)?;
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
        if let Some(output) = outputs.last() {
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
struct BuildStage(PathBuf);
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
    let path = fs::canonicalize(tool.path()).map_err(error)?;
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
    if bytes.len() > 512 * 1024 * 1024
        || format!("sha256-{:x}", Sha256::digest(&bytes)) != tool.digest()
    {
        return Err(error(format!(
            "lifecycle tool digest mismatch for {}",
            tool.name()
        )));
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
fn run_hook(
    stage: &BuildStage,
    approval: &DependencyLifecycleApproval,
    script: &str,
    input: &NamedLayoutInput,
    trees: &BTreeMap<String, PathBuf>,
    lock: &Lockfile,
) -> Result<(), OperationalError> {
    let tools = stage.0.join(".tools");
    if tools.exists() {
        fs::remove_dir_all(&tools).map_err(error)?;
    }
    fs::create_dir(&tools).map_err(error)?;
    for tool in approval.tools() {
        let path = tools.join(tool.name());
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
    let mut builder = tapid_runner::ExecutionRequest::builder(tools.join("sh"))
        .args(["-c", script, "tapid-lifecycle"])
        .project_root(&stage.0)
        .working_directory(stage.0.join("package"))
        .policy(policy)
        .executable_search_path(&tools)
        .allow_process_memory_stats(approval.process_memory_stats())
        .envs(approval.environment().iter());
    if approval.dependencies() && stage.0.join("node_modules/.bin").is_dir() {
        builder = builder.executable_search_path(stage.0.join("node_modules/.bin"));
    }
    builder = builder.executable_search_path("/usr/bin");
    if tools.join("node").is_file() {
        builder = builder.trusted_node_runtime(tools.join("node"));
    }
    let request = builder.build().map_err(error)?;
    let outcome = tapid_runner::execute(&request).map_err(error)?;
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
fn validate_output(root: &Path) -> Result<(), OperationalError> {
    tapid_archive::validate_tree(root, tapid_archive::ValidationLimits::default()).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            commands(project.path()).unwrap(),
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
