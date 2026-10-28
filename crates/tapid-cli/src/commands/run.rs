use crate::filesystem::read::{ReadError, read_bounded_file};
use clap::Args as ClapArgs;
mod permissions;
mod result;
use result::RunFailure;
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Script name from package.json. Use a configured profile or explicit invocation permission flags.
    pub(crate) script: String,
    /// Project directory containing package.json.
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Select a workspace member by package name while retaining root policy and containment.
    #[arg(long)]
    pub(crate) workspace: Option<String>,
    /// Exact Node executable; otherwise the first valid Node on the host PATH is used.
    #[arg(long)]
    pub(crate) node_runtime: Option<PathBuf>,
    /// Emit the versioned receipt as one JSON line on stderr after child output.
    #[arg(long)]
    pub(crate) receipt_json: bool,
    /// Write a trusted JSON result to a new file outside the project; preserves child streams.
    #[arg(long, value_name = "PATH", conflicts_with = "receipt_json")]
    pub(crate) result_file: Option<PathBuf>,
    /// Allow process-memory statistics in required Linux sandbox mode by creating a private PID/mount namespace with read-only procfs.
    #[arg(long, visible_aliases = ["allow-procfs", "allow-memory-read"])]
    pub(crate) allow_process_memory_stats: bool,
    #[command(flatten)]
    pub(crate) permissions: permissions::InvocationPermissions,
    /// Arguments forwarded after `--` to the script.
    #[arg(last = true)]
    pub(crate) arguments: Vec<OsString>,
}

fn config_error_category(category: tapid_runner::ConfigErrorCategory) -> &'static str {
    use tapid_runner::ConfigErrorCategory;
    match category {
        ConfigErrorCategory::Malformed => "malformed",
        ConfigErrorCategory::UnknownKey => "unknown-key",
        ConfigErrorCategory::InvalidPath => "invalid-path",
        ConfigErrorCategory::InvalidEnvironment => "invalid-environment",
        ConfigErrorCategory::InvalidLimit => "invalid-limit",
        ConfigErrorCategory::CapacityExceeded => "capacity-exceeded",
    }
}

pub(crate) fn run(args: Args, configuration: &crate::registry::ConfigSelection) -> ExitCode {
    let mut result_file = match args.result_file.as_deref() {
        Some(path) => match result::create_file(path, &args.project_dir) {
            Ok(file) => Some(file),
            Err(error) => {
                eprintln!("error: cannot create run result file: {error}");
                return ExitCode::from(1);
            }
        },
        None => None,
    };
    let mut policy_inputs = None;
    let outcome = execute(&args, &mut policy_inputs, configuration);
    if let Some(file) = &mut result_file
        && let Err(error) = result::write(file, &outcome, policy_inputs.as_ref())
    {
        eprintln!("error: cannot write run result file: {error}");
    }
    match outcome {
        Ok(outcome) => render_outcome(
            &outcome,
            result_file.is_none().then_some(args.receipt_json),
            args.permissions
                .enabled()
                .then_some(policy_inputs)
                .flatten(),
        ),
        Err(_) => ExitCode::from(1),
    }
}

fn execute(
    args: &Args,
    policy_inputs: &mut Option<serde_json::Value>,
    configuration: &crate::registry::ConfigSelection,
) -> Result<tapid_runner::ExecutionOutcome, RunFailure> {
    let project_dir = match fs::canonicalize(&args.project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            eprintln!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return Err(RunFailure::ProjectUnavailable);
        }
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                args.project_dir.display()
            );
            return Err(RunFailure::ProjectUnavailable);
        }
    };
    let (manifest, working_directory) = if let Some(workspace_name) = args.workspace.as_deref() {
        let workspace = match tapid_manifest::Workspace::discover(&project_dir) {
            Ok(workspace) => workspace,
            Err(error) => {
                eprintln!("error: {error}");
                return Err(RunFailure::WorkspaceInvalid);
            }
        };
        let manifest = match workspace.select(Some(workspace_name)) {
            Ok(manifest) => manifest.clone(),
            Err(error) => {
                eprintln!("error: {error}");
                return Err(RunFailure::WorkspaceInvalid);
            }
        };
        let member_manifest = match workspace.select_path(Some(workspace_name)) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return Err(RunFailure::WorkspaceInvalid);
            }
        };
        let Some(working_directory) = member_manifest.parent() else {
            eprintln!("error: workspace member manifest has no parent");
            return Err(RunFailure::WorkspaceInvalid);
        };
        (manifest, working_directory.to_owned())
    } else {
        let manifest = match super::manifest::read_manifest(&project_dir.join("package.json")) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return Err(RunFailure::ManifestInvalid);
            }
        };
        (manifest, project_dir.clone())
    };
    let Some(script) = manifest.scripts().get(&args.script).cloned() else {
        match args.workspace.as_deref() {
            Some(workspace_name) => eprintln!(
                "error: workspace member '{workspace_name}' package script is missing: {}",
                args.script
            ),
            None => eprintln!("error: root package script is missing: {}", args.script),
        }
        return Err(RunFailure::ScriptMissing);
    };
    let config_path = project_dir.join("tapid.toml");
    let invocation_mode = args.permissions.enabled();
    let config_bytes = match read_run_config(&config_path) {
        Ok(bytes) => bytes,
        Err(ReadError::CapacityExceeded) => {
            eprintln!("error: invalid run configuration (capacity-exceeded)");
            return Err(RunFailure::ConfigInvalid);
        }
        Err(ReadError::Io(error)) => {
            if error.kind() == std::io::ErrorKind::NotFound && invocation_mode {
                Vec::new()
            } else {
                if error.kind() == std::io::ErrorKind::NotFound {
                    eprintln!("error: required run configuration is missing: tapid.toml");
                } else {
                    eprintln!("error: cannot read run configuration 'tapid.toml': {error}");
                }
                return Err(RunFailure::ConfigUnavailable);
            }
        }
    };
    let config = match tapid_runner::RunConfig::parse_toml_bytes(&config_bytes) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "error: invalid run configuration ({})",
                config_error_category(error.category())
            );
            return Err(RunFailure::ConfigInvalid);
        }
    };
    if !invocation_mode && config.exact_profile(&args.script).is_none() {
        eprintln!(
            "error: run policy profile is missing for script: {}",
            args.script
        );
        return Err(RunFailure::ProfileMissing);
    }
    let selection = match args.permissions.select(&config, &args.script) {
        Ok(selection) => selection,
        Err(error) => {
            eprintln!(
                "error: invalid invocation permissions ({})",
                config_error_category(error.category())
            );
            return Err(RunFailure::PermissionsInvalid);
        }
    };
    *policy_inputs = Some(selection.provenance(&args.permissions));
    let registry_config = match configuration.load_for_script(&project_dir, &config_bytes) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(RunFailure::RegistryConfigurationInvalid);
        }
    };
    let registry_credential_names = registry_config.credential_environment_names();
    if registry_config
        .allowlist_contains_registry_credentials(selection.policy.environment(), cfg!(windows))
    {
        eprintln!("error: registry credential environment variables cannot be passed to scripts");
        return Err(RunFailure::EnvironmentInvalid);
    }
    let ambient_environment = match crate::run::read_allowlisted_environment_with_denied(
        selection.policy.environment(),
        &registry_credential_names,
    ) {
        Ok(environment) => environment,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(RunFailure::EnvironmentInvalid);
        }
    };
    let host_path = args
        .node_runtime
        .is_none()
        .then(|| env::var_os("PATH"))
        .flatten();
    let host = crate::run::HostExecutionEnvironment {
        node_runtime: args.node_runtime.as_deref(),
        path: host_path.as_deref(),
        allowlisted: &ambient_environment,
        allow_process_memory_stats: args.allow_process_memory_stats,
    };
    let prepared_result = if invocation_mode {
        crate::run::prepare_execution_request_with_policy(
            &project_dir,
            &working_directory,
            &selection.policy,
            selection.ceiling.as_ref(),
            &script,
            &args.arguments,
            host,
        )
    } else if args.workspace.is_some() {
        crate::run::prepare_execution_request_with_working_directory(
            &project_dir,
            &working_directory,
            &args.script,
            &config,
            &script,
            &args.arguments,
            host,
        )
    } else {
        crate::run::prepare_execution_request(
            &project_dir,
            &args.script,
            &config,
            &script,
            &args.arguments,
            host,
        )
    };
    let prepared = match prepared_result {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(RunFailure::PreparationFailed);
        }
    };
    match crate::run::execute_checked(&prepared) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            if error.category() == tapid_runner::ExecutionErrorCategory::UnsupportedContainment {
                eprintln!(
                    "error: sandbox execution failed (unsupported-containment): {error}; no enforcement receipt was issued"
                );
            } else {
                eprintln!(
                    "error: sandbox execution failed ({}): {error}; no enforcement receipt was accepted",
                    execution_error_category(error.category())
                );
            }
            Err(RunFailure::Execution(error))
        }
    }
}

fn read_run_config(path: &std::path::Path) -> Result<Vec<u8>, ReadError> {
    read_bounded_file(path, tapid_runner::MAX_CONFIG_BYTES)
}

fn execution_error_category(category: tapid_runner::ExecutionErrorCategory) -> &'static str {
    use tapid_runner::ExecutionErrorCategory;
    match category {
        ExecutionErrorCategory::InvalidRequest => "invalid-request",
        ExecutionErrorCategory::UnsupportedContainment => "unsupported-containment",
        ExecutionErrorCategory::PolicyViolation => "policy-violation",
        ExecutionErrorCategory::Spawn => "spawn",
        ExecutionErrorCategory::Timeout => "timeout",
        ExecutionErrorCategory::OutputLimit => "output-limit",
        ExecutionErrorCategory::ProcessLimit => "process-limit",
        ExecutionErrorCategory::MemoryLimit => "memory-limit",
        ExecutionErrorCategory::Internal => "internal",
        _ => "unknown",
    }
}

fn evidence_value(items: &[tapid_runner::DimensionEvidence]) -> Vec<serde_json::Value> {
    items.iter().map(|item| serde_json::json!({
        "dimension": format!("{:?}", item.dimension()), "scope": format!("{:?}", item.scope()),
        "mechanism": item.mechanism(), "limitations": item.limitations(),
    })).collect()
}

fn completion_value(completion: &tapid_runner::CompletionEvidence) -> serde_json::Value {
    serde_json::json!({
        "confirmed": completion.confirmed(),
        "evidence": evidence_value(completion.evidence()),
        "cleanup_confidence": format!("{:?}", completion.cleanup_confidence()),
    })
}

fn receipt_value(outcome: &tapid_runner::ExecutionOutcome) -> serde_json::Value {
    use serde_json::json;
    let receipt = outcome.enforcement();
    let grants = receipt.resolved_filesystem().grants().iter().map(|grant| {
        #[cfg(unix)]
        let native_path = { use std::os::unix::ffi::OsStrExt; json!({"encoding": "unix-bytes", "units": grant.path().as_os_str().as_bytes()}) };
        #[cfg(windows)]
        let native_path = { use std::os::windows::ffi::OsStrExt; json!({"encoding": "windows-utf16", "units": grant.path().as_os_str().encode_wide().collect::<Vec<_>>()}) };
        #[cfg(not(any(unix, windows)))]
        let native_path = json!({"encoding": "display", "units": grant.path().to_string_lossy()});
        json!({"path": grant.path().to_string_lossy(), "native_path": native_path,
            "access": format!("{:?}", grant.access()), "kind": format!("{:?}", grant.kind()),
            "source": format!("{:?}", grant.source()), "binding": format!("{:?}", grant.binding())})
    }).collect::<Vec<_>>();
    let limits = receipt.configured_limits();
    json!({
        "executable_resolution": receipt.executable_resolution(),
        "schema_version": 1, "assurance": format!("{:?}", receipt.assurance()),
        "backend": {"name": receipt.backend().name(), "version": receipt.backend().version(), "deprecation": receipt.backend().deprecation()},
        "requested": receipt.requested(), "declared": receipt.declared(), "observed": receipt.observed(), "enforced": receipt.enforced(),
        "declared_evidence": evidence_value(receipt.support().declared_evidence()),
        "observed_evidence": evidence_value(receipt.support().observed_evidence()),
        "established_evidence": evidence_value(receipt.established_evidence()),
        "effective_filesystem": grants,
        "configured_limits": {"timeout_seconds": limits.timeout_seconds(), "max_output_bytes": limits.max_output_bytes(), "max_processes": limits.max_processes(), "max_memory_bytes": limits.max_memory_bytes()},
        "termination": format!("{:?}", outcome.termination()),
        "completion": completion_value(outcome.completion())
    })
}

#[cfg(any(not(any(target_os = "macos", target_os = "linux")), test))]
fn forward_child_output(
    child_stdout: &[u8],
    child_stderr: &[u8],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<()> {
    stdout.write_all(child_stdout)?;
    stdout.flush()?;
    stderr.write_all(child_stderr)?;
    stderr.flush()
}

fn forward_child_output_for_cli(
    child_stdout: &[u8],
    child_stderr: &[u8],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // The macOS backend streams bytes while capturing them; replay would duplicate output.
        let _ = (child_stdout, child_stderr, stdout, stderr);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        {
            // The Linux backend also streams bytes while capturing them.
            let _ = (child_stdout, child_stderr, stdout, stderr);
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            forward_child_output(child_stdout, child_stderr, stdout, stderr)
        }
    }
}

fn render_outcome(
    outcome: &tapid_runner::ExecutionOutcome,
    machine: Option<bool>,
    provenance: Option<serde_json::Value>,
) -> ExitCode {
    let output_error = forward_child_output_for_cli(
        outcome.stdout(),
        outcome.stderr(),
        &mut io::stdout(),
        &mut io::stderr(),
    )
    .err();
    let mut value = receipt_value(outcome);
    if let Some(provenance) = provenance {
        value["policy_inputs"] = provenance;
    }
    if machine == Some(true) {
        eprintln!("\n{value}");
    } else if machine == Some(false) {
        eprintln!(
            "\nsandbox receipt: {}",
            serde_json::to_string_pretty(&value).expect("receipt JSON values are serializable")
        );
    }
    if let Some(error) = output_error {
        eprintln!("error: failed to forward root package script output: {error}");
        return ExitCode::from(1);
    }
    if outcome.process_memory_stats_hint() {
        eprintln!(
            "hint: this script appears to need process memory statistics blocked by the sandbox; if you trust it, retry with --allow-process-memory-stats (aliases: --allow-memory-read, --allow-procfs)"
        );
    }
    match outcome.termination() {
        tapid_runner::Termination::TimedOut => {
            eprintln!("error: root package script exceeded its timeout limit");
        }
        tapid_runner::Termination::OutputLimitExceeded => {
            eprintln!("error: root package script exceeded its output limit");
        }
        tapid_runner::Termination::ProcessLimitExceeded => {
            eprintln!("error: root package script exceeded its process limit");
        }
        tapid_runner::Termination::MemoryLimitExceeded => {
            eprintln!("error: root package script exceeded its memory limit");
        }
        tapid_runner::Termination::Cancelled => {
            eprintln!("error: root package script cancelled by Ctrl+C");
        }
        tapid_runner::Termination::Exited(_) | tapid_runner::Termination::Signaled(_) => {}
    }
    termination_exit_code(outcome.termination())
}

fn termination_exit_code(termination: &tapid_runner::Termination) -> ExitCode {
    match termination {
        tapid_runner::Termination::Exited(code) => crate::output::child_exit_code(*code),
        tapid_runner::Termination::Signaled(signal) => {
            crate::output::child_exit_code(128_i32.saturating_add(*signal))
        }
        tapid_runner::Termination::TimedOut
        | tapid_runner::Termination::OutputLimitExceeded
        | tapid_runner::Termination::ProcessLimitExceeded
        | tapid_runner::Termination::MemoryLimitExceeded => ExitCode::from(1),
        tapid_runner::Termination::Cancelled => ExitCode::from(130),
    }
}

#[cfg(test)]
mod tests;
