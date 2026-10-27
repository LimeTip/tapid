//! Shared, value-free script and policy selection for launch and inspection.
use super::{Args, RunFailure, config_error_category, permissions, read_run_config};
use crate::filesystem::read::ReadError;
use std::{fs, path::PathBuf};

/// Value-free facts retained even when later selection or policy checks fail.
#[derive(Default)]
pub(super) struct SelectionEvidence {
    pub policy_inputs: Option<serde_json::Value>,
    pub manifest_path: Option<PathBuf>,
}

pub(super) struct Plan {
    pub project_dir: PathBuf,
    pub working_directory: PathBuf,
    pub script: String,
    pub config: tapid_runner::RunConfig,
    pub selection: permissions::PolicySelection,
    pub registry_credential_names: Vec<String>,
}

pub(super) fn select(args: &Args, evidence: &mut SelectionEvidence) -> Result<Plan, RunFailure> {
    macro_rules! diagnostic {
        ($($argument:tt)*) => { if !args.preflight { eprintln!($($argument)*); } };
    }
    let project_dir = match fs::canonicalize(&args.project_dir) {
        Ok(path) if path.is_dir() => path,
        Ok(path) => {
            diagnostic!(
                "error: project directory is not a directory: {}",
                path.display()
            );
            return Err(RunFailure::ProjectUnavailable);
        }
        Err(error) => {
            diagnostic!(
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
                diagnostic!("error: {error}");
                return Err(RunFailure::WorkspaceInvalid);
            }
        };
        let manifest = match workspace.select(Some(workspace_name)) {
            Ok(manifest) => manifest.clone(),
            Err(error) => {
                diagnostic!("error: {error}");
                return Err(RunFailure::WorkspaceInvalid);
            }
        };
        let member_manifest = match workspace.select_path(Some(workspace_name)) {
            Ok(path) => path,
            Err(error) => {
                diagnostic!("error: {error}");
                return Err(RunFailure::WorkspaceInvalid);
            }
        };
        let Some(working_directory) = member_manifest.parent() else {
            diagnostic!("error: workspace member manifest has no parent");
            return Err(RunFailure::WorkspaceInvalid);
        };
        evidence.manifest_path = Some(member_manifest.to_owned());
        (manifest, working_directory.to_owned())
    } else {
        evidence.manifest_path = Some(project_dir.join("package.json"));
        let manifest =
            match crate::commands::manifest::read_manifest(&project_dir.join("package.json")) {
                Ok(value) => value,
                Err(error) => {
                    diagnostic!("error: {error}");
                    return Err(RunFailure::ManifestInvalid);
                }
            };
        (manifest, project_dir.clone())
    };
    let Some(script) = manifest.scripts().get(&args.script).cloned() else {
        match args.workspace.as_deref() {
            Some(workspace_name) => diagnostic!(
                "error: workspace member '{workspace_name}' package script is missing: {}",
                args.script
            ),
            None => diagnostic!("error: root package script is missing: {}", args.script),
        }
        return Err(RunFailure::ScriptMissing);
    };
    let config_path = project_dir.join("tapid.toml");
    let invocation_mode = args.permissions.enabled();
    let config_bytes = match read_run_config(&config_path) {
        Ok(bytes) => bytes,
        Err(ReadError::CapacityExceeded) => {
            diagnostic!("error: invalid run configuration (capacity-exceeded)");
            return Err(RunFailure::ConfigInvalid);
        }
        Err(ReadError::Io(error)) => {
            if error.kind() == std::io::ErrorKind::NotFound && invocation_mode {
                Vec::new()
            } else {
                if error.kind() == std::io::ErrorKind::NotFound {
                    diagnostic!("error: required run configuration is missing: tapid.toml");
                } else {
                    diagnostic!("error: cannot read run configuration 'tapid.toml': {error}");
                }
                return Err(RunFailure::ConfigUnavailable);
            }
        }
    };
    let config = match tapid_runner::RunConfig::parse_toml_bytes(&config_bytes) {
        Ok(config) => config,
        Err(error) => {
            diagnostic!(
                "error: invalid run configuration ({})",
                config_error_category(error.category())
            );
            return Err(RunFailure::ConfigInvalid);
        }
    };
    if !invocation_mode && config.exact_profile(&args.script).is_none() {
        diagnostic!(
            "error: run policy profile is missing for script: {}",
            args.script
        );
        return Err(RunFailure::ProfileMissing);
    }
    let selection = match args.permissions.select(&config, &args.script) {
        Ok(selection) => selection,
        Err(error) => {
            diagnostic!(
                "error: invalid invocation permissions ({})",
                config_error_category(error.category())
            );
            return Err(RunFailure::PermissionsInvalid);
        }
    };
    evidence.policy_inputs = Some(selection.provenance(&args.permissions));
    let registry_config = match crate::registry::RegistryConfig::parse_toml_bytes(&config_bytes) {
        Ok(config) => config,
        Err(error) => {
            diagnostic!("error: {error}");
            return Err(RunFailure::RegistryConfigurationInvalid);
        }
    };
    let registry_credential_names = registry_config.credential_environment_names();
    if registry_config
        .allowlist_contains_registry_credentials(selection.policy.environment(), cfg!(windows))
    {
        diagnostic!("error: registry credential environment variables cannot be passed to scripts");
        return Err(RunFailure::EnvironmentInvalid);
    }
    Ok(Plan {
        project_dir,
        working_directory,
        script,
        config,
        selection,
        registry_credential_names,
    })
}

impl Plan {
    pub(super) fn prepare(
        &self,
        args: &Args,
        ambient_environment: &std::collections::BTreeMap<String, std::ffi::OsString>,
    ) -> Result<crate::run::PreparedExecution, crate::run::RunPreparationError> {
        let Self {
            project_dir,
            working_directory,
            script,
            config,
            selection,
            ..
        } = self;
        let invocation_mode = args.permissions.enabled();
        let host_path = args
            .node_runtime
            .is_none()
            .then(|| std::env::var_os("PATH"))
            .flatten();
        let host = crate::run::HostExecutionEnvironment {
            node_runtime: args.node_runtime.as_deref(),
            path: host_path.as_deref(),
            allowlisted: ambient_environment,
            allow_process_memory_stats: args.allow_process_memory_stats,
        };
        if invocation_mode {
            crate::run::prepare_execution_request_with_policy(
                project_dir,
                working_directory,
                &selection.policy,
                selection.ceiling.as_ref(),
                script,
                &args.arguments,
                host,
            )
        } else if args.workspace.is_some() {
            crate::run::prepare_execution_request_with_working_directory(
                project_dir,
                working_directory,
                &args.script,
                config,
                script,
                &args.arguments,
                host,
            )
        } else {
            crate::run::prepare_execution_request(
                project_dir,
                &args.script,
                config,
                script,
                &args.arguments,
                host,
            )
        }
    }
}
