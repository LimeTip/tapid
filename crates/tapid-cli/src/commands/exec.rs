use super::run::{
    permissions::InvocationPermissions,
    result::{self, RunFailure},
};
use crate::application::installed::{CommandOwner, ProjectEvidence, SelectionError};
use clap::Args as ClapArgs;
use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Explicit project/workspace root containing the managed installation.
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Exact workspace member name; otherwise select the root manifest.
    #[arg(long)]
    workspace: Option<String>,
    /// Direct dependency's local install name, including aliases.
    #[arg(long)]
    package: Option<String>,
    /// Existing verified package store; no initialization or acquisition.
    #[arg(long)]
    store_dir: Option<PathBuf>,
    /// Exact Node executable; otherwise discover Node on the invoking host PATH.
    #[arg(long)]
    node_runtime: Option<PathBuf>,
    /// Write a trusted result to a new caller-controlled file outside the project.
    #[arg(long, value_name = "PATH")]
    result_file: Option<PathBuf>,
    /// Enable the existing Linux private procfs mechanism for process memory statistics.
    #[arg(long, visible_aliases = ["allow-procfs", "allow-memory-read"])]
    allow_process_memory_stats: bool,
    #[command(flatten)]
    permissions: InvocationPermissions,
    /// Exported Node binary to execute.
    #[arg(required = true)]
    pub(super) command: Option<String>,
    /// Ordered opaque arguments for the selected binary.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(super) arguments: Vec<OsString>,
}

pub(crate) fn run(args: Args) -> ExitCode {
    let mut file = match args
        .result_file
        .as_deref()
        .map(|path| result::create_file(path, &args.project_dir))
        .transpose()
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("error: cannot create exec result file: {error}");
            return ExitCode::from(1);
        }
    };
    let mut inputs = None;
    let mut owner = None;
    let outcome = execute(&args, &mut inputs, &mut owner);
    if let Some(file) = file.as_mut()
        && let Err(error) = result::write_exec(file, &outcome, inputs.as_ref(), owner.as_ref())
    {
        eprintln!("error: cannot write exec result file: {error}");
        return ExitCode::from(1);
    }
    match outcome {
        Ok(outcome) => {
            super::run::render_outcome(&outcome, file.is_none().then_some(false), inputs)
        }
        Err(_) => ExitCode::from(1),
    }
}

fn selection_failure(error: SelectionError) -> RunFailure {
    eprintln!("error: {error}");
    RunFailure::InstalledSelection(error)
}

fn preparation_failure(error: impl std::fmt::Display) -> RunFailure {
    eprintln!("error: {error}");
    RunFailure::PreparationFailed
}

fn execute(
    args: &Args,
    inputs: &mut Option<serde_json::Value>,
    owner: &mut Option<serde_json::Value>,
) -> Result<tapid_runner::ExecutionOutcome, RunFailure> {
    let store = tapid_store::Store::new(match &args.store_dir {
        Some(path) => path.clone(),
        None => crate::application::install::default_store_root().map_err(preparation_failure)?,
    });
    let evidence = ProjectEvidence::observe(&args.project_dir, &store)
        .map_err(|error| selection_failure(SelectionError::ProjectEvidence(error)))?;
    let command = args
        .command
        .as_deref()
        .ok_or(RunFailure::PreparationFailed)?;
    let selected = evidence
        .select_command(args.workspace.as_deref(), args.package.as_deref(), command)
        .map_err(selection_failure)?;
    *owner = Some(match &selected.selection.owner {
        CommandOwner::Authenticated(key) => serde_json::json!({
            "kind": "authenticated-package", "install_name": selected.selection.install_name.to_string(),
            "name": key.id.name.to_string(), "version": key.id.version.to_string(),
            "source": key.id.registry.to_string(), "peer_context": key.peer_context.to_string(),
            "platform_context": key.platform_context.to_string(),
        }),
        CommandOwner::Workspace(link) => serde_json::json!({
            "kind": "mutable-workspace", "install_name": selected.selection.install_name.to_string(),
            "name": link.name.to_string(), "version": link.version.to_string(), "source": link.source.to_string_lossy(),
        }),
    });
    let config = evidence.run_config().map_err(preparation_failure)?;
    // Keep exec profiles distinct from root package scripts with the same name.
    let profile = format!("exec:{}:{command}", selected.selection.install_name);
    let policy = args
        .permissions
        .select(&config, &profile)
        .map_err(preparation_failure)?;
    *inputs = Some(policy.provenance(&args.permissions));
    let credentials = evidence.config.credential_environment_names();
    if evidence
        .config
        .allowlist_contains_registry_credentials(policy.policy.environment(), cfg!(windows))
    {
        return Err(preparation_failure(
            "registry credential environment variables cannot be passed to binaries",
        ));
    }
    let environment = crate::run::read_allowlisted_environment_with_denied(
        policy.policy.environment(),
        &credentials,
    )
    .map_err(preparation_failure)?;
    let path = args
        .node_runtime
        .is_none()
        .then(|| env::var_os("PATH"))
        .flatten();
    let cwd = evidence
        .workspace
        .select_path(args.workspace.as_deref())
        .map_err(preparation_failure)?
        .parent()
        .ok_or(RunFailure::PreparationFailed)?;
    let entry = std::fs::canonicalize(&selected.entry).map_err(preparation_failure)?;
    let prepared = crate::run::prepare_node_execution_request_with_policy(
        &evidence.root,
        cwd,
        &policy.policy,
        policy.ceiling.as_ref(),
        &entry,
        &args.arguments,
        crate::run::HostExecutionEnvironment {
            node_runtime: args.node_runtime.as_deref(),
            path: path.as_deref(),
            allowlisted: &environment,
            allow_process_memory_stats: args.allow_process_memory_stats,
        },
    )
    .map_err(preparation_failure)?;
    evidence
        .revalidate_command(
            args.workspace.as_deref(),
            args.package.as_deref(),
            command,
            &selected,
        )
        .map_err(selection_failure)?;
    // Evidence (and its project/store guards) stays live until checked execution
    // completes. The runner revalidates the bound Node runtime before spawn.
    crate::run::execute_checked(&prepared).map_err(|error| {
        eprintln!(
            "error: contained binary execution failed: {error}; no launch receipt was accepted"
        );
        RunFailure::Execution(error)
    })
}
