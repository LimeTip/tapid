//! The caller-owned result file is separate from all child streams and grants.
use std::{fs::File, io};

use serde_json::{Value, json};
use tapid_runner::{ExecutionError, ExecutionOutcome};

pub(super) enum RunFailure {
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
            Self::Execution(error) => super::execution_error_category(error.category()),
        }
    }
}

pub(super) use crate::filesystem::result_file::create as create_file;

pub(super) fn write(
    file: &mut File,
    outcome: &Result<ExecutionOutcome, RunFailure>,
    policy_inputs: Option<&Value>,
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
    let value = json!({
        "schema_version": 1,
        "operation": "run",
        "outcome": status,
        "error": error,
        "policy_inputs": policy_inputs,
        "receipt": receipt,
    });
    crate::filesystem::result_file::write(file, &value)
}
