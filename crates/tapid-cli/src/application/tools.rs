//! Standalone-tool orchestration. Project installation stays in the existing installer.
mod launchers;
mod state;
mod transaction;

use super::{
    install::{self, InstallMode, Progress},
    outcome::{
        ChangeState, ErrorKind, OperationFailure, OperationOutcome, OperationalError, Warning,
    },
};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use state::Catalog;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tapid_manifest::PackageManifest;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tool {
    pub requirement: String,
    pub version: String,
    pub generation: String,
    pub commands: BTreeMap<String, ToolBin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_platform: Option<String>,
}
fn installation_platform() -> String {
    tapid_core::PlatformContext::new(
        Some(std::env::consts::OS),
        Some(std::env::consts::ARCH),
        crate::online::current_libc(),
    )
    .expect("host platform constants are valid")
    .to_string()
}
fn validate_environment_platform(tool: &Tool) -> Result<(), OperationalError> {
    if tool.installation_platform.as_deref() != Some(installation_platform().as_str()) {
        return Err(error(
            "tool environment has a different or unknown installation platform; update or reinstall the tool",
        ));
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolBin {
    pub target: PathBuf,
    pub node: bool,
}
pub(crate) struct ToolInfo {
    pub name: String,
    pub tool: Tool,
    pub environment: PathBuf,
}
pub(crate) struct Report {
    pub outcome: OperationOutcome,
    pub tools: Vec<ToolInfo>,
    pub bin_dir: PathBuf,
}
#[derive(Clone, Copy)]
pub(crate) enum Action<'a> {
    Install(&'a str),
    List,
    Update(&'a str),
    Remove(&'a str),
}
pub(crate) struct Options<'a> {
    pub tools_dir: Option<&'a Path>,
    pub store_dir: Option<&'a Path>,
    pub registry_fixture: Option<&'a Path>,
}
fn error(message: impl std::fmt::Display) -> OperationalError {
    OperationalError::new(ErrorKind::InvalidData, message.to_string())
}
fn spec(value: &str) -> Result<(String, String), OperationalError> {
    let (name, requirement) = crate::package_spec::parse(value);
    let name = name.strip_prefix("npm:").unwrap_or(name);
    name.parse::<tapid_core::PackageName>().map_err(error)?;
    validate_requirement(requirement)?;
    Ok((name.to_owned(), requirement.to_owned()))
}

fn validate_requirement(requirement: &str) -> Result<(), OperationalError> {
    let range = requirement
        .parse::<tapid_resolver::Requirement>()
        .map_err(error)?;
    if range.is_alias() || range.dist_tag().is_some() {
        return Err(error(
            "tool installation supports npm package names and semver requirements",
        ));
    }
    Ok(())
}
fn view(root: &Path, catalog: &Catalog, outcome: OperationOutcome) -> Report {
    Report {
        tools: catalog
            .tools
            .iter()
            .map(|(name, tool)| ToolInfo {
                name: name.clone(),
                tool: tool.clone(),
                environment: root.join("environments").join(&tool.generation),
            })
            .collect(),
        outcome,
        bin_dir: root.join("bin"),
    }
}
fn stage(
    root: &Path,
    name: &str,
    requirement: &str,
    options: &Options<'_>,
    outcome: &mut OperationOutcome,
    progress: &mut impl FnMut(Progress),
) -> Result<Tool, OperationalError> {
    let generation = format!("{:x}", crate::filesystem::atomic::unique_nonce());
    let environment = transaction::create_environment(root, &generation)?;
    let manifest = PackageManifest::parse(
        &serde_json::json!({
            "name":"tapid-tool-environment", "version":"0.0.0", "private":true,
            "dependencies":{name:requirement}
        })
        .to_string(),
    )
    .map_err(error)?;
    state::write_new(
        &environment.join("package.json"),
        manifest.to_json().as_bytes(),
        false,
    )?;
    let report = install::run(
        install::InstallRequest {
            project_dir: &environment,
            target_manifest_path: Path::new("package.json"),
            mutation: install::ManifestMutation::None,
            store_root: options.store_dir,
            mode: InstallMode::Online,
            registry_fixture: options.registry_fixture,
            allow_unverified_registry_artifacts: false,
        },
        progress,
    )
    .map_err(|failure| *failure.error)?;
    outcome.warnings.extend(report.outcome.warnings);
    let package_root = environment.join("node_modules").join(name);
    state::check_path(&package_root)?;
    let installed =
        crate::commands::manifest::read_manifest_typed(&package_root.join("package.json"))?;
    let lock_json = std::io::read_to_string(state::open_regular(&environment.join("tapid.lock"))?)
        .map_err(error)?;
    let lock = tapid_lockfile::Lockfile::from_json(&lock_json).map_err(error)?;
    let selected: tapid_lockfile::LockfilePackageKey = lock
        .root_bindings()
        .get(name)
        .ok_or_else(|| error("tool has no direct locked package identity"))?
        .parse()
        .map_err(error)?;
    if selected.name.as_str() != name
        || installed.name() != &selected.name
        || installed.version() != selected.version
    {
        return Err(OperationalError::new(
            ErrorKind::Integrity,
            "installed tool manifest identity differs from its direct locked package",
        ));
    }
    let commands = launchers::package_commands(root, &environment, &package_root, &installed)?;
    Ok(Tool {
        requirement: requirement.to_owned(),
        version: selected.version.to_string(),
        generation,
        commands,
        installation_platform: Some(installation_platform()),
    })
}
pub(crate) fn run(
    action: Action<'_>,
    options: Options<'_>,
    mut progress: impl FnMut(Progress),
) -> Result<Report, OperationFailure> {
    let root = state::root_path(options.tools_dir)
        .map_err(|e| OperationFailure::unchanged(options.tools_dir.unwrap_or(Path::new(".")), e))?;
    // Validate package arguments before creating durable tool state.
    let request = match action {
        Action::Install(value) => Some(spec(value)),
        Action::Update(name) | Action::Remove(name) => Some(
            name.strip_prefix("npm:")
                .unwrap_or(name)
                .parse::<tapid_core::PackageName>()
                .map(|name| (name.to_string(), String::new()))
                .map_err(error),
        ),
        Action::List => None,
    }
    .transpose()
    .map_err(|e| OperationFailure::unchanged(&root, e))?;
    let mut outcome = OperationOutcome::unchanged(&root);
    if !root
        .try_exists()
        .map_err(|e| OperationFailure::unchanged(&root, error(e)))?
    {
        match action {
            Action::List => return Ok(view(&root, &Catalog::default(), outcome)),
            Action::Update(_) | Action::Remove(_) => {
                return Err(OperationFailure::unchanged(
                    &root,
                    error("tool is not installed"),
                ));
            }
            Action::Install(_) => {}
        }
    }
    if matches!(action, Action::List) {
        state::resume_initialization(&root)
    } else {
        state::initialize(&root)
    }
    .map_err(|e| OperationFailure::unchanged(&root, e))?;
    state::check_path(&root.join(".tapid-activation.lock"))
        .map_err(|e| OperationFailure::unchanged(&root, e))?;
    let _lock = crate::filesystem::activation::ActivationLock::acquire_without_recovery(&root)
        .map_err(|e| OperationFailure::unchanged(&root, e))?;
    let mut catalog_lock =
        Some(state::lock_catalog(&root).map_err(|e| OperationFailure::unchanged(&root, e))?);
    if let Err(e) = transaction::recover(&root).map(|recovered| {
        if recovered {
            outcome.warnings.push(Warning::PreviousTransactionRecovered);
        }
    }) {
        outcome.state = ChangeState::RecoveryRequired;
        return Err(OperationFailure::with_recovery_paths(
            e,
            outcome,
            None,
            [
                "catalog.json",
                "pending.json",
                "committed",
                "bin",
                "environments",
            ]
            .map(|name| root.join(name)),
        ));
    }
    let before = state::load(&root).map_err(|e| OperationFailure::unchanged(&root, e))?;
    let result = (|| {
        let cleanup = transaction::cleanup(&root, &before)?;
        if cleanup.changed {
            outcome.changed_files.push(root.join("environments"));
        }
        let mut after = before.clone();
        match action {
            Action::List => {
                outcome.state = if cleanup.pending {
                    ChangeState::CommittedCleanupPending
                } else if cleanup.changed {
                    ChangeState::Committed
                } else {
                    ChangeState::Unchanged
                };
                return Ok(before.clone());
            }
            Action::Install(_) | Action::Update(_) => {
                let (name, requirement) = request.as_ref().expect("validated request");
                let requirement = if matches!(action, Action::Update(_)) {
                    &before
                        .tools
                        .get(name)
                        .ok_or_else(|| error("tool is not installed"))?
                        .requirement
                } else {
                    requirement
                };
                // The mutation lock excludes other writers and cleanup. Invocations
                // can lease the current generation while its replacement is staged.
                drop(catalog_lock.take());
                let staged = stage(
                    &root,
                    name,
                    requirement,
                    &options,
                    &mut outcome,
                    &mut progress,
                );
                catalog_lock = Some(state::lock_catalog(&root)?);
                let tool = staged?;
                after.tools.insert(name.to_owned(), tool);
            }
            Action::Remove(_) => {
                let name = &request.as_ref().expect("validated request").0;
                if after.tools.remove(name).is_none() {
                    return Err(error("tool is not installed"));
                }
            }
        }
        after.shims = launchers::plan(&root, &after)?;
        transaction::activate(&root, &before, &after)?;
        outcome.state = ChangeState::Committed;
        outcome.changed_files.extend([
            root.join("catalog.json"),
            root.join("bin"),
            root.join("environments"),
        ]);
        let after = state::load(&root)?;
        if transaction::cleanup(&root, &after)?.pending {
            outcome.state = ChangeState::CommittedCleanupPending;
        }
        Ok(after)
    })();
    match result {
        Ok(catalog) => Ok(view(&root, &catalog, outcome)),
        Err(e) => {
            // Failure to reacquire after staging cannot have published a journal.
            // Leave the owned stage for the next serialized mutation to clean.
            if catalog_lock.is_none() {
                return Err(OperationFailure::unchanged(&root, e));
            }
            let committed = root.join("committed").exists();
            let pending = root.join("pending.json").exists();
            let recovery = (|| {
                transaction::recover(&root)?;
                let catalog = state::load(&root)?;
                transaction::cleanup(&root, &catalog)?;
                Ok::<(), OperationalError>(())
            })();
            if recovery.is_err() {
                outcome.state = if committed {
                    ChangeState::CommittedCleanupPending
                } else {
                    ChangeState::RecoveryRequired
                };
            } else if committed {
                outcome.state = ChangeState::Committed;
            } else if pending {
                outcome.state = ChangeState::RolledBack;
            }
            Err(OperationFailure::with_recovery_paths(
                e,
                outcome,
                recovery.err(),
                [
                    "catalog.json",
                    "pending.json",
                    "committed",
                    "bin",
                    "environments",
                ]
                .map(|name| root.join(name)),
            ))
        }
    }
}
#[allow(
    unsafe_code,
    reason = "pre_exec preserves the tool lease using async-signal-safe descriptor operations"
)]
pub(crate) fn execute(
    root: &Path,
    command: &str,
    arguments: &[OsString],
) -> Result<i32, OperationalError> {
    launchers::validate_exported_command(command, crate::application::replay::current_platform())?;
    let root = state::root_path(Some(root))?;
    state::verify(&root)?;
    let lock = state::lock_catalog(&root)?;
    transaction::recover(&root)?;
    let catalog = state::load(&root)?;
    let (tool, bin) = catalog
        .tools
        .values()
        .find_map(|tool| tool.commands.get(command).map(|bin| (tool, bin)))
        .ok_or_else(|| error("tool command is not installed"))?;
    let environment = root.join("environments").join(&tool.generation);
    validate_environment_platform(tool)?;
    if state::read(&environment.join(".tapid-tool"))? != tool.generation.as_bytes() {
        return Err(error("tool ownership marker mismatch"));
    }
    let target = environment.join(&bin.target);
    state::check_path(&target)?;
    if !fs::symlink_metadata(&target).map_err(error)?.is_file() {
        return Err(error("tool target is not a regular file"));
    }
    let lease = crate::filesystem::activation::open_lock_file(&environment.join(".in-use"))
        .map_err(error)?;
    FileExt::try_lock_shared(&lease).map_err(error)?;
    drop(lock);
    let mut process = if bin.node {
        let node = crate::run::discover_node_runtime(std::env::var_os("PATH").as_deref())
            .map_err(error)?;
        let mut process = Command::new(node);
        process.arg(&target);
        process
    } else {
        Command::new(&target)
    };
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let descriptor = lease.as_raw_fd();
        // The child must retain the flock if a terminal signal kills this
        // wrapper first. Change inheritance only in the forked child, keeping
        // the parent's descriptor close-on-exec for unrelated spawns.
        // SAFETY: the lease stays open through status(); the callback uses only
        // async-signal-safe fcntl calls and propagates errors before exec.
        unsafe {
            process.pre_exec(move || {
                let flags = libc::fcntl(descriptor, libc::F_GETFD);
                if flags == -1
                    || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let status = process.args(arguments).status().map_err(error)?;
    drop(lease);
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Ok(128 + signal);
        }
    }
    Ok(status.code().unwrap_or(1))
}
