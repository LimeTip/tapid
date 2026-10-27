use crate::filesystem::read::{ReadError, read_bounded_file};
use clap::Args as ClapArgs;
mod permissions;
mod planning;
mod preflight;
mod result;
use result::RunFailure;
use std::{
    ffi::OsString,
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
    /// Inspect script selection and requested permissions without executing or reading environment values.
    #[arg(long, conflicts_with_all = ["result_file", "receipt_json"])]
    pub(crate) preflight: bool,
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

pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    if args.preflight {
        return preflight::run(&args, json);
    }
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
    let outcome = execute(&args, &mut policy_inputs);
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
) -> Result<tapid_runner::ExecutionOutcome, RunFailure> {
    let mut evidence = planning::SelectionEvidence::default();
    let plan = planning::select(args, &mut evidence);
    *policy_inputs = evidence.policy_inputs;
    let plan = plan?;
    let ambient_environment = match crate::run::read_allowlisted_environment_with_denied(
        plan.selection.policy.environment(),
        &plan.registry_credential_names,
    ) {
        Ok(environment) => environment,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(RunFailure::EnvironmentInvalid);
        }
    };
    let prepared = match plan.prepare(args, &ambient_environment) {
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
