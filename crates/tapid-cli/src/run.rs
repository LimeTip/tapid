//! Construction and checked execution of root-package script requests.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt, fs,
    path::{Path, PathBuf},
};
use tapid_runner::{ExecutionError, ExecutionOutcome, ExecutionRequest, RunConfig};

/// A validated runner request plus its explicit executable search contract.
#[derive(Debug)]
pub struct PreparedExecution {
    request: ExecutionRequest,
    // Retained for boundary tests; the runner intentionally keeps these paths private.
    #[allow(dead_code)]
    executable_search_directories: Vec<PathBuf>,
}

impl PreparedExecution {
    pub fn request(&self) -> &ExecutionRequest {
        &self.request
    }

    #[allow(dead_code)]
    pub fn executable_search_directories(&self) -> &[PathBuf] {
        &self.executable_search_directories
    }
}

#[derive(Debug)]
pub enum RunPreparationError {
    MissingProfile(String),
    InvalidProjectDirectory,
    InvalidManagedBin,
    InvalidNodeRuntime,
    ReservedPath,
    InvalidRequest(ExecutionError),
}

impl fmt::Display for RunPreparationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProfile(name) => {
                write!(f, "run policy profile is missing for script: {name}")
            }
            Self::InvalidProjectDirectory => f.write_str("project directory is not canonical"),
            Self::InvalidManagedBin => f.write_str(
                "managed executable directory is missing or is not a directory: node_modules/.bin",
            ),
            Self::InvalidNodeRuntime => {
                f.write_str("node runtime must be an existing executable file")
            }
            Self::ReservedPath => {
                f.write_str("run policy cannot allowlist reserved environment variable PATH")
            }
            Self::InvalidRequest(error) => write!(f, "invalid runner execution request: {error}"),
        }
    }
}

impl std::error::Error for RunPreparationError {}

/// Constructs the exact request accepted by `tapid-runner` without consulting ambient `PATH`.
pub fn prepare_execution_request(
    project_dir: &Path,
    script_name: &str,
    config: &RunConfig,
    script: &str,
    arguments: &[String],
    node_runtime: &Path,
    ambient_environment: &BTreeMap<String, OsString>,
) -> Result<PreparedExecution, RunPreparationError> {
    let project_dir =
        fs::canonicalize(project_dir).map_err(|_| RunPreparationError::InvalidProjectDirectory)?;
    if !project_dir.is_dir() {
        return Err(RunPreparationError::InvalidProjectDirectory);
    }
    let policy = config
        .exact_profile(script_name)
        .cloned()
        .ok_or_else(|| RunPreparationError::MissingProfile(script_name.to_owned()))?;
    if policy
        .environment()
        .iter()
        .any(|name| name.eq_ignore_ascii_case("PATH"))
    {
        return Err(RunPreparationError::ReservedPath);
    }

    let managed_bin = fs::canonicalize(project_dir.join("node_modules/.bin"))
        .map_err(|_| RunPreparationError::InvalidManagedBin)?;
    if !managed_bin.is_dir() {
        return Err(RunPreparationError::InvalidManagedBin);
    }
    let node_runtime = canonical_executable(node_runtime)?;
    let runtime_bin = node_runtime
        .parent()
        .ok_or(RunPreparationError::InvalidNodeRuntime)?
        .to_owned();
    let search_directories = vec![managed_bin, runtime_bin];

    let environment = policy
        .environment()
        .iter()
        .filter_map(|name| {
            ambient_environment
                .get(name)
                .map(|value| (name.clone(), value.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let (program, shell_arguments) = shell_invocation(script, arguments);
    let request = ExecutionRequest::builder(program)
        .args(shell_arguments)
        .executable_search_paths(search_directories.iter().cloned())
        .project_root(project_dir)
        .policy(policy)
        .envs(environment)
        .build()
        .map_err(RunPreparationError::InvalidRequest)?;

    Ok(PreparedExecution {
        request,
        executable_search_directories: search_directories,
    })
}

fn canonical_executable(path: &Path) -> Result<PathBuf, RunPreparationError> {
    let canonical = fs::canonicalize(path).map_err(|_| RunPreparationError::InvalidNodeRuntime)?;
    let metadata = fs::metadata(&canonical).map_err(|_| RunPreparationError::InvalidNodeRuntime)?;
    if !metadata.is_file() {
        return Err(RunPreparationError::InvalidNodeRuntime);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(RunPreparationError::InvalidNodeRuntime);
        }
    }
    Ok(canonical)
}

#[cfg(not(windows))]
fn shell_invocation(script: &str, arguments: &[String]) -> (PathBuf, Vec<OsString>) {
    let command = if arguments.is_empty() {
        script.to_owned()
    } else {
        format!("{script} \"$@\"")
    };
    let mut shell_arguments = vec![
        OsString::from("-c"),
        OsString::from(command),
        OsString::from("tapid-script"),
    ];
    shell_arguments.extend(arguments.iter().map(OsString::from));
    (PathBuf::from("/bin/sh"), shell_arguments)
}

#[cfg(windows)]
fn shell_invocation(script: &str, arguments: &[String]) -> (PathBuf, Vec<OsString>) {
    let suffix = arguments
        .iter()
        .map(|argument| format!(" \"{}\"", argument.replace('"', "\"\"")))
        .collect::<String>();
    (
        PathBuf::from("cmd.exe"),
        vec![
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
            OsString::from(format!("{script}{suffix}")),
        ],
    )
}

/// Executes only through tapid-runner's checked, fail-closed platform path.
pub fn execute_checked(prepared: &PreparedExecution) -> Result<ExecutionOutcome, ExecutionError> {
    tapid_runner::execute(prepared.request())
}
