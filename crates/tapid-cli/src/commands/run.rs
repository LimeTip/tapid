use clap::Args as ClapArgs;
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Read},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Script name from package.json. Requires a matching [run.scripts.<name>] profile in tapid.toml.
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
    /// Allow process-memory statistics in Linux Restricted mode by creating a private PID/mount namespace with read-only procfs.
    #[arg(long, visible_aliases = ["allow-procfs", "allow-memory-read"])]
    pub(crate) allow_process_memory_stats: bool,
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

pub(crate) fn run(args: Args) -> ExitCode {
    let project_dir = match fs::canonicalize(&args.project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            eprintln!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!(
                "error: cannot access project directory '{}': {error}",
                args.project_dir.display()
            );
            return ExitCode::from(1);
        }
    };
    let (manifest, working_directory) = if let Some(workspace_name) = args.workspace.as_deref() {
        let workspace = match tapid_manifest::Workspace::discover(&project_dir) {
            Ok(workspace) => workspace,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        let manifest = match workspace.select(Some(workspace_name)) {
            Ok(manifest) => manifest.clone(),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        let member_manifest = match workspace.select_path(Some(workspace_name)) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        let Some(working_directory) = member_manifest.parent() else {
            eprintln!("error: workspace member manifest has no parent");
            return ExitCode::from(1);
        };
        (manifest, working_directory.to_owned())
    } else {
        let manifest = match super::manifest::read_manifest(&project_dir.join("package.json")) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        (manifest, project_dir.clone())
    };
    let Some(script) = manifest.scripts().get(&args.script).cloned() else {
        eprintln!("error: root package script is missing: {}", args.script);
        return ExitCode::from(1);
    };
    let config_path = project_dir.join("tapid.toml");
    let config_bytes = match read_run_config(&config_path) {
        Ok(bytes) => bytes,
        Err(ConfigReadError::CapacityExceeded) => {
            eprintln!("error: invalid run configuration (capacity-exceeded)");
            return ExitCode::from(1);
        }
        Err(ConfigReadError::Io(error)) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                eprintln!("error: required run configuration is missing: tapid.toml");
            } else {
                eprintln!("error: cannot read run configuration 'tapid.toml': {error}");
            }
            return ExitCode::from(1);
        }
    };
    let config = match tapid_runner::RunConfig::parse_toml_bytes(&config_bytes) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "error: invalid run configuration ({})",
                config_error_category(error.category())
            );
            return ExitCode::from(1);
        }
    };
    if config.exact_profile(&args.script).is_none() {
        eprintln!(
            "error: run policy profile is missing for script: {}",
            args.script
        );
        return ExitCode::from(1);
    }
    let registry_config = match crate::registry::RegistryConfig::parse_toml_bytes(&config_bytes) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let registry_credential_names = registry_config.credential_environment_names();
    if registry_config.allowlist_contains_registry_credentials(
        config
            .exact_profile(&args.script)
            .expect("exact profile was checked")
            .environment(),
        cfg!(windows),
    ) {
        eprintln!("error: registry credential environment variables cannot be passed to scripts");
        return ExitCode::from(1);
    }
    let ambient_environment = match crate::run::read_allowlisted_environment_with_denied(
        config
            .exact_profile(&args.script)
            .expect("exact profile was checked")
            .environment(),
        &registry_credential_names,
    ) {
        Ok(environment) => environment,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let host_path = args
        .node_runtime
        .is_none()
        .then(|| env::var_os("PATH"))
        .flatten();
    let prepared_result = if args.workspace.is_some() {
        crate::run::prepare_execution_request_with_working_directory(
            &project_dir,
            &working_directory,
            &args.script,
            &config,
            &script,
            &args.arguments,
            crate::run::HostExecutionEnvironment {
                node_runtime: args.node_runtime.as_deref(),
                path: host_path.as_deref(),
                allowlisted: &ambient_environment,
                allow_process_memory_stats: args.allow_process_memory_stats,
            },
        )
    } else {
        crate::run::prepare_execution_request(
            &project_dir,
            &args.script,
            &config,
            &script,
            &args.arguments,
            crate::run::HostExecutionEnvironment {
                node_runtime: args.node_runtime.as_deref(),
                path: host_path.as_deref(),
                allowlisted: &ambient_environment,
                allow_process_memory_stats: args.allow_process_memory_stats,
            },
        )
    };
    let prepared = match prepared_result {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    match crate::run::execute_checked(&prepared) {
        Ok(outcome) => render_outcome(&outcome, args.receipt_json),
        Err(error) => {
            if error.category() == tapid_runner::ExecutionErrorCategory::UnsupportedContainment {
                eprintln!(
                    "error: sandbox execution failed (unsupported-containment): {error}; no process was started and no enforcement receipt was issued"
                );
            } else {
                eprintln!(
                    "error: sandbox execution failed ({}): {error}; no enforcement receipt was accepted",
                    execution_error_category(error.category())
                );
            }
            ExitCode::from(1)
        }
    }
}

pub(crate) enum ConfigReadError {
    CapacityExceeded,
    Io(io::Error),
}

fn read_run_config(path: &std::path::Path) -> Result<Vec<u8>, ConfigReadError> {
    read_bounded_config_file(path, tapid_runner::MAX_CONFIG_BYTES)
}

pub(crate) fn read_bounded_config_file(
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<Vec<u8>, ConfigReadError> {
    let max_bytes = u64::try_from(max_bytes).map_err(|_| ConfigReadError::CapacityExceeded)?;
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or(ConfigReadError::CapacityExceeded)?;
    let metadata = fs::symlink_metadata(path).map_err(ConfigReadError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(ConfigReadError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration is not a regular file",
        )));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A replaced FIFO must not block the open, nor may a replaced symlink
        // redirect the read after the initial metadata check.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(ConfigReadError::Io)?;
    let metadata = file.metadata().map_err(ConfigReadError::Io)?;
    if !metadata.is_file() {
        return Err(ConfigReadError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration is not a regular file",
        )));
    }
    if metadata.len() > max_bytes {
        return Err(ConfigReadError::CapacityExceeded);
    }
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(ConfigReadError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ConfigReadError::CapacityExceeded);
    }
    Ok(bytes)
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

fn receipt_value(outcome: &tapid_runner::ExecutionOutcome) -> serde_json::Value {
    use serde_json::json;
    let receipt = outcome.enforcement();
    let evidence = |items: &[tapid_runner::DimensionEvidence]| {
        items.iter().map(|item| json!({
        "dimension": format!("{:?}", item.dimension()), "scope": format!("{:?}", item.scope()),
        "mechanism": item.mechanism(), "limitations": item.limitations(),
    })).collect::<Vec<_>>()
    };
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
        "declared_evidence": evidence(receipt.support().declared_evidence()),
        "observed_evidence": evidence(receipt.support().observed_evidence()),
        "established_evidence": evidence(receipt.established_evidence()),
        "effective_filesystem": grants,
        "configured_limits": {"timeout_seconds": limits.timeout_seconds(), "max_output_bytes": limits.max_output_bytes(), "max_processes": limits.max_processes(), "max_memory_bytes": limits.max_memory_bytes()},
        "termination": format!("{:?}", outcome.termination()),
        "completion": {"confirmed": outcome.completion().confirmed(), "evidence": evidence(outcome.completion().evidence()), "cleanup_confidence": format!("{:?}", outcome.completion().cleanup_confidence())}
    })
}

fn render_outcome(outcome: &tapid_runner::ExecutionOutcome, machine: bool) -> ExitCode {
    let value = receipt_value(outcome);
    if machine {
        eprintln!("\n{value}");
    } else {
        eprintln!(
            "\nsandbox receipt: {}",
            serde_json::to_string_pretty(&value).expect("receipt JSON values are serializable")
        );
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
    }
}

#[cfg(test)]
mod tests;
