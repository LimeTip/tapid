//! Construction and checked execution of root-package script requests.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
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
    MissingNodeRuntime,
    DuplicateEnvironmentName(String),
    ReservedPath,
    #[cfg_attr(not(windows), allow(dead_code))]
    InvalidWindowsArgument,
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
                f.write_str("node runtime must be an executable named node or node.exe")
            }
            Self::MissingNodeRuntime => f.write_str(
                "cannot discover an executable named node or node.exe on the invoking host PATH",
            ),
            Self::DuplicateEnvironmentName(name) => write!(
                f,
                "run policy contains case-equivalent environment names: {name}"
            ),
            Self::ReservedPath => {
                f.write_str("run policy cannot allowlist reserved environment variable PATH")
            }
            Self::InvalidWindowsArgument => {
                f.write_str("Windows command arguments cannot contain CR or LF")
            }
            Self::InvalidRequest(error) => write!(f, "invalid runner execution request: {error}"),
        }
    }
}

impl std::error::Error for RunPreparationError {}

pub struct HostExecutionEnvironment<'a> {
    pub node_runtime: Option<&'a Path>,
    pub path: Option<&'a OsStr>,
    pub allowlisted: &'a BTreeMap<String, OsString>,
}

/// Constructs the exact request accepted by `tapid-runner` without consulting ambient `PATH`.
pub fn prepare_execution_request(
    project_dir: &Path,
    script_name: &str,
    config: &RunConfig,
    script: &str,
    arguments: &[OsString],
    host: HostExecutionEnvironment<'_>,
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

    let managed_bin_path = project_dir.join("node_modules/.bin");
    let managed_bin_metadata = fs::symlink_metadata(&managed_bin_path)
        .map_err(|_| RunPreparationError::InvalidManagedBin)?;
    if managed_bin_metadata.file_type().is_symlink() || !managed_bin_metadata.is_dir() {
        return Err(RunPreparationError::InvalidManagedBin);
    }
    let managed_bin =
        fs::canonicalize(&managed_bin_path).map_err(|_| RunPreparationError::InvalidManagedBin)?;
    if managed_bin == project_dir || !managed_bin.starts_with(&project_dir) || !managed_bin.is_dir()
    {
        return Err(RunPreparationError::InvalidManagedBin);
    }
    let node_runtime = match host.node_runtime {
        Some(runtime) => canonical_node_executable(runtime)?,
        None => discover_node_runtime(host.path)?,
    };
    let runtime_bin = node_runtime
        .parent()
        .ok_or(RunPreparationError::InvalidNodeRuntime)?
        .to_owned();
    let search_directories = vec![runtime_bin, managed_bin];

    let environment = policy
        .environment()
        .iter()
        .filter_map(|name| {
            host.allowlisted
                .get(name)
                .map(|value| (name.clone(), value.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let (program, shell_arguments) = shell_invocation(script, arguments, &search_directories)?;
    let request = ExecutionRequest::builder(program)
        .args(shell_arguments)
        .windows_verbatim_arguments(cfg!(windows))
        .executable_search_paths(search_directories.iter().cloned())
        .trusted_node_runtime(&node_runtime)
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

const MAX_HOST_PATH_DIRECTORIES: usize = 256;

fn discover_node_runtime(host_path: Option<&OsStr>) -> Result<PathBuf, RunPreparationError> {
    let Some(host_path) = host_path else {
        return Err(RunPreparationError::MissingNodeRuntime);
    };
    for directory in std::env::split_paths(host_path).take(MAX_HOST_PATH_DIRECTORIES) {
        let candidate = directory.join(if cfg!(windows) { "node.exe" } else { "node" });
        if let Ok(runtime) = canonical_node_executable(&candidate) {
            return Ok(runtime);
        }
    }
    Err(RunPreparationError::MissingNodeRuntime)
}

fn canonical_node_executable(path: &Path) -> Result<PathBuf, RunPreparationError> {
    let canonical = fs::canonicalize(path).map_err(|_| RunPreparationError::InvalidNodeRuntime)?;
    let valid_name = canonical.file_name().is_some_and(|name| {
        if cfg!(windows) {
            name.to_string_lossy().eq_ignore_ascii_case("node.exe")
        } else {
            name == OsStr::new("node")
        }
    });
    if !valid_name {
        return Err(RunPreparationError::InvalidNodeRuntime);
    }
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

pub fn validate_allowlisted_environment_names(
    names: &[String],
    case_insensitive: bool,
) -> Result<(), RunPreparationError> {
    for (index, name) in names.iter().enumerate() {
        if case_insensitive
            && names[..index]
                .iter()
                .any(|prior| prior.eq_ignore_ascii_case(name))
        {
            return Err(RunPreparationError::DuplicateEnvironmentName(name.clone()));
        }
    }
    Ok(())
}

pub fn read_allowlisted_environment(
    names: &[String],
) -> Result<BTreeMap<String, OsString>, RunPreparationError> {
    validate_allowlisted_environment_names(names, cfg!(windows))?;
    let mut environment = BTreeMap::new();
    for name in names {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(name.clone(), value);
        }
    }
    Ok(environment)
}

#[cfg(not(windows))]
fn shell_invocation(
    script: &str,
    arguments: &[OsString],
    _search_directories: &[PathBuf],
) -> Result<(PathBuf, Vec<OsString>), RunPreparationError> {
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
    shell_arguments.extend(arguments.iter().cloned());
    Ok((PathBuf::from("/bin/sh"), shell_arguments))
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_cmd_escape_argument(
    input: &str,
    double_escape: bool,
) -> Result<String, RunPreparationError> {
    if input.contains(['\r', '\n']) {
        return Err(RunPreparationError::InvalidWindowsArgument);
    }
    if input.is_empty() {
        return Ok("\"\"".to_owned());
    }

    let mut result = if input.contains([' ', '\t', '\u{000b}', '"']) {
        let mut quoted = String::from("\"");
        let characters = input.chars().collect::<Vec<_>>();
        let mut index = 0;
        while index <= characters.len() {
            let mut slash_count = 0;
            while index < characters.len() && characters[index] == '\\' {
                index += 1;
                slash_count += 1;
            }
            if index == characters.len() {
                quoted.extend(std::iter::repeat_n('\\', slash_count * 2));
                break;
            }
            if characters[index] == '"' {
                quoted.extend(std::iter::repeat_n('\\', slash_count * 2 + 1));
            } else {
                quoted.extend(std::iter::repeat_n('\\', slash_count));
            }
            quoted.push(characters[index]);
            index += 1;
        }
        quoted.push('"');
        quoted
    } else {
        input.to_owned()
    };

    let escape_once = |value: &str| {
        let mut escaped = String::with_capacity(value.len());
        for character in value.chars() {
            if matches!(
                character,
                ' ' | '!' | '%' | '^' | '&' | '(' | ')' | '<' | '>' | '|' | '"'
            ) {
                escaped.push('^');
            }
            escaped.push(character);
        }
        escaped
    };
    result = escape_once(&result);
    if double_escape {
        result = escape_once(&result);
    }
    Ok(result)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_initial_command_double_escape(script: &str, search_directories: &[PathBuf]) -> bool {
    // Match npm promise-spawn's initial-command scan exactly: only a literal
    // space terminates the first token, quote characters remain part of the
    // lookup string, and either quote kind toggles the in-quotes state.
    let mut initial = String::new();
    let mut inside_quotes = false;
    for character in script.chars() {
        if character == ' ' && !inside_quotes {
            break;
        }
        initial.push(character);
        if character == '"' || character == '\'' {
            inside_quotes = !inside_quotes;
        }
    }
    let initial_lower = initial.to_ascii_lowercase();
    let explicit_wrapper = initial_lower.ends_with(".cmd") || initial_lower.ends_with(".bat");
    if initial.is_empty() {
        return false;
    }
    let has_extension = Path::new(&initial).extension().is_some();
    let extensions: &[&str] = if has_extension {
        &[""]
    } else {
        &["", ".com", ".exe", ".bat", ".cmd"]
    };
    for directory in search_directories {
        for extension in extensions {
            let candidate = directory.join(format!("{initial}{extension}"));
            if candidate.is_file() {
                let extension = candidate
                    .extension()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default();
                return extension.eq_ignore_ascii_case("cmd")
                    || extension.eq_ignore_ascii_case("bat");
            }
        }
    }
    explicit_wrapper
}

#[cfg(windows)]
fn shell_invocation(
    script: &str,
    arguments: &[OsString],
    search_directories: &[PathBuf],
) -> Result<(PathBuf, Vec<OsString>), RunPreparationError> {
    let double_escape = windows_initial_command_double_escape(script, search_directories);
    let suffix = arguments
        .iter()
        .map(|argument| {
            argument
                .to_str()
                .ok_or(RunPreparationError::InvalidWindowsArgument)
                .and_then(|argument| windows_cmd_escape_argument(argument, double_escape))
                .map(|argument| format!(" {argument}"))
        })
        .collect::<Result<String, _>>()?;
    Ok((
        PathBuf::from("cmd.exe"),
        vec![
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
            OsString::from(format!("{script}{suffix}")),
        ],
    ))
}

/// Executes only through tapid-runner's checked, fail-closed platform path.
pub fn execute_checked(prepared: &PreparedExecution) -> Result<ExecutionOutcome, ExecutionError> {
    tapid_runner::execute(prepared.request())
}
