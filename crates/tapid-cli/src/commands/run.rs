use clap::Args as ClapArgs;
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read, Write},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Root package script name.
    pub(crate) script: String,
    /// Project directory containing package.json.
    #[arg(long, default_value = ".")]
    pub(crate) project_dir: PathBuf,
    /// Exact Node executable used to construct the controlled runtime PATH.
    #[arg(long)]
    pub(crate) node_runtime: PathBuf,
    /// Arguments forwarded after `--` to the script.
    #[arg(last = true)]
    pub(crate) arguments: Vec<String>,
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
    let manifest = match super::manifest::read_manifest(&project_dir.join("package.json")) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
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
    let ambient_environment = env::vars_os()
        .filter_map(|(name, value)| name.into_string().ok().map(|name| (name, value)))
        .collect::<BTreeMap<_, _>>();
    let prepared = match crate::run::prepare_execution_request(
        &project_dir,
        &args.script,
        &config,
        &script,
        &args.arguments,
        &args.node_runtime,
        &ambient_environment,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    match crate::run::execute_checked(&prepared) {
        Ok(outcome) => render_outcome(&outcome),
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

enum ConfigReadError {
    CapacityExceeded,
    Io(io::Error),
}

fn read_run_config(path: &std::path::Path) -> Result<Vec<u8>, ConfigReadError> {
    let file = fs::File::open(path).map_err(ConfigReadError::Io)?;
    if file.metadata().map_err(ConfigReadError::Io)?.len() > tapid_runner::MAX_CONFIG_BYTES as u64 {
        return Err(ConfigReadError::CapacityExceeded);
    }
    let mut bytes = Vec::new();
    file.take(tapid_runner::MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(ConfigReadError::Io)?;
    if bytes.len() > tapid_runner::MAX_CONFIG_BYTES {
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

fn render_outcome(outcome: &tapid_runner::ExecutionOutcome) -> ExitCode {
    let _ = io::stdout().write_all(outcome.stdout());
    let _ = io::stderr().write_all(outcome.stderr());
    let receipt = outcome.enforcement();
    eprintln!(
        "sandbox receipt: backend={}@{} requested={:?} declared={:?} observed={:?} enforced={:?}",
        receipt.backend().name(),
        receipt.backend().version(),
        receipt.requested(),
        receipt.declared(),
        receipt.observed(),
        receipt.enforced()
    );
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
mod tests {
    use super::*;

    #[test]
    fn nonzero_and_limit_terminations_map_to_stable_cli_exits() {
        assert_eq!(
            termination_exit_code(&tapid_runner::Termination::Exited(37)),
            ExitCode::from(37)
        );
        for termination in [
            tapid_runner::Termination::TimedOut,
            tapid_runner::Termination::OutputLimitExceeded,
            tapid_runner::Termination::ProcessLimitExceeded,
            tapid_runner::Termination::MemoryLimitExceeded,
        ] {
            assert_eq!(termination_exit_code(&termination), ExitCode::from(1));
        }
    }
}
