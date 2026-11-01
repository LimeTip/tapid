//! The caller-owned result file is separate from all child streams and grants.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

use serde_json::{Value, json};
use tapid_runner::{ExecutionError, ExecutionOutcome};

pub(in crate::commands) enum RunFailure {
    ProjectUnavailable,
    WorkspaceInvalid,
    ManifestInvalid,
    ScriptMissing,
    ConfigUnavailable,
    ConfigInvalid,
    ProfileMissing,
    PermissionsInvalid,
    RegistryConfigurationInvalid,
    EnvironmentInvalid,
    PreparationFailed,
    InstalledSelection(crate::application::installed::SelectionError),
    Execution(ExecutionError),
}

impl RunFailure {
    fn code(&self) -> &'static str {
        match self {
            Self::ProjectUnavailable => "PROJECT_UNAVAILABLE",
            Self::WorkspaceInvalid => "WORKSPACE_INVALID",
            Self::ManifestInvalid => "MANIFEST_INVALID",
            Self::ScriptMissing => "SCRIPT_MISSING",
            Self::ConfigUnavailable => "CONFIG_UNAVAILABLE",
            Self::ConfigInvalid => "CONFIG_INVALID",
            Self::ProfileMissing => "PROFILE_MISSING",
            Self::PermissionsInvalid => "PERMISSIONS_INVALID",
            Self::RegistryConfigurationInvalid => "REGISTRY_CONFIGURATION_INVALID",
            Self::EnvironmentInvalid => "ENVIRONMENT_INVALID",
            Self::PreparationFailed => "PREPARATION_FAILED",
            Self::InstalledSelection(error) => {
                use crate::application::installed::SelectionError;
                match error {
                    SelectionError::MissingCommand => "COMMAND_MISSING",
                    SelectionError::Ambiguous(_) => "COMMAND_AMBIGUOUS",
                    SelectionError::InvalidCommand => "COMMAND_INVALID",
                    SelectionError::InvalidPackageSelection(_) => "PACKAGE_INELIGIBLE",
                    SelectionError::InvalidProjectSelection(_) => "WORKSPACE_INVALID",
                    SelectionError::InvalidMapping(
                        tapid_linker::PlanError::UnsupportedBinExecutable(_),
                    ) => "BIN_EXECUTABLE_UNSUPPORTED",
                    SelectionError::InvalidMapping(_) => "BIN_MAPPING_INVALID",
                    SelectionError::InconsistentInstallation(_)
                    | SelectionError::ProjectEvidence(_) => "INSTALLATION_INCONSISTENT",
                }
            }
            Self::Execution(error) => super::execution_error_category(error.category()),
        }
    }
}

pub(in crate::commands) fn create_file(path: &Path, project: &Path) -> io::Result<File> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "result file needs a filename")
    })?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = fs::canonicalize(parent.unwrap_or(Path::new(".")))?;
    // Every project write grant is confined to the canonical project root by
    // tapid-runner. Backend additions do not grant writes to caller directories.
    // A missing/invalid project cannot launch a child, but still needs a result.
    if let Ok(project) = fs::canonicalize(project)
        && parent.starts_with(project)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "result file must be outside the project root",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    // std opens non-inheritable handles / close-on-exec descriptors. Keep this
    // exact file open; never follow the destination pathname again after spawn.
    options.open(parent.join(name))
}

pub(in crate::commands) fn write(
    file: &mut File,
    outcome: &Result<ExecutionOutcome, RunFailure>,
    policy_inputs: Option<&Value>,
) -> io::Result<()> {
    write_record(file, outcome, policy_inputs, "run", None)
}

pub(in crate::commands) fn write_exec(
    file: &mut File,
    outcome: &Result<ExecutionOutcome, RunFailure>,
    policy_inputs: Option<&Value>,
    owner: Option<&Value>,
) -> io::Result<()> {
    write_record(file, outcome, policy_inputs, "exec", owner)
}

fn write_record(
    file: &mut File,
    outcome: &Result<ExecutionOutcome, RunFailure>,
    policy_inputs: Option<&Value>,
    operation: &str,
    owner: Option<&Value>,
) -> io::Result<()> {
    let (status, error, receipt) = match outcome {
        Ok(outcome) => ("completed", Value::Null, super::receipt_value(outcome)),
        Err(failure) => (
            if matches!(failure, RunFailure::Execution(_)) {
                "execution_failure"
            } else {
                "prelaunch_failure"
            },
            json!({
                "code": failure.code(),
                "completion": match failure {
                    RunFailure::Execution(error) => error.completion().map(super::completion_value),
                    _ => None,
                },
            }),
            Value::Null,
        ),
    };
    let mut value = json!({
        "schema_version": 1,
        "operation": operation,
        "outcome": status,
        "error": error,
        "policy_inputs": policy_inputs,
        "receipt": receipt,
    });
    if operation == "exec" {
        value["owner"] = owner.cloned().unwrap_or(Value::Null);
    }
    let mut bytes = serde_json::to_vec(&value).expect("run result JSON values are serializable");
    bytes.push(b'\n');
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    if result.is_err() {
        // Best effort only. Consumers must reject partial records even when
        // truncation fails, and must wait for the CLI to finish before reading.
        let _ = file.set_len(0);
    }
    result
}
