//! Root-package script process execution.
//!
//! This module only starts one requested child process. It does not resolve or run
//! dependency lifecycle scripts, sandbox children, or make policy decisions.
//! Process-group signal containment is intentionally not provided: portable,
//! correct containment requires platform-specific integration outside this layer.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fmt, io,
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
};

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub project_dir: PathBuf,
    pub script: Option<String>,
    pub arguments: Vec<String>,
    pub shell: ShellBackend,
    pub environment: BTreeMap<String, String>,
}

impl RunRequest {
    pub fn new(project_dir: PathBuf, script: Option<String>) -> Self {
        Self {
            project_dir,
            script,
            arguments: Vec::new(),
            shell: ShellBackend::default_for_platform(),
            environment: BTreeMap::new(),
        }
    }

    pub fn with_arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_shell(mut self, shell: ShellBackend) -> Self {
        self.shell = shell;
        self
    }

    pub fn with_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.environment = environment;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellBackend {
    UnixSh,
    WindowsCmd,
}

impl ShellBackend {
    pub fn default_for_platform() -> Self {
        if cfg!(windows) {
            Self::WindowsCmd
        } else {
            Self::UnixSh
        }
    }

    pub fn invocation(&self, script: &str, arguments: &[String]) -> ShellInvocation {
        let (program, mut command_arguments) = match self {
            Self::UnixSh => (
                PathBuf::from("/bin/sh"),
                vec![
                    "-c".to_owned(),
                    script.to_owned(),
                    "tapid-script".to_owned(),
                ],
            ),
            Self::WindowsCmd => (
                PathBuf::from("cmd.exe"),
                vec![
                    "/D".to_owned(),
                    "/S".to_owned(),
                    "/C".to_owned(),
                    windows_command_with_arguments(script, arguments),
                ],
            ),
        };
        if matches!(self, Self::UnixSh) {
            command_arguments.extend(arguments.iter().cloned());
        }
        ShellInvocation {
            program,
            arguments: command_arguments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

#[derive(Debug)]
pub struct ChildResult {
    status: ExitStatus,
}

impl ChildResult {
    pub fn status(&self) -> ExitStatus {
        self.status
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.status.code()
    }

    pub fn success(&self) -> bool {
        self.status.success()
    }
}

#[derive(Debug)]
pub enum RunError {
    MissingScript,
    InvalidProjectDirectory(PathBuf),
    Spawn(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingScript => write!(f, "root package script is missing"),
            Self::InvalidProjectDirectory(path) => {
                write!(
                    f,
                    "project directory is not a directory: {}",
                    path.display()
                )
            }
            Self::Spawn(error) => write!(f, "cannot start root package script: {error}"),
        }
    }
}

impl std::error::Error for RunError {}

pub fn execute(request: RunRequest) -> Result<ChildResult, RunError> {
    let script = request.script.ok_or(RunError::MissingScript)?;
    if !request.project_dir.is_dir() {
        return Err(RunError::InvalidProjectDirectory(request.project_dir));
    }

    let invocation = request.shell.invocation(&script, &request.arguments);
    let mut command = Command::new(invocation.program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        if matches!(request.shell, ShellBackend::WindowsCmd) {
            command
                .args(&invocation.arguments[..3])
                .raw_arg(&invocation.arguments[3]);
        } else {
            command.args(&invocation.arguments);
        }
    }
    #[cfg(not(windows))]
    command.args(&invocation.arguments);
    command
        .current_dir(&request.project_dir)
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut environment: BTreeMap<OsString, OsString> = env::vars_os().collect();
    for (key, value) in request.environment {
        environment.insert(OsString::from(key), OsString::from(value));
    }
    let managed_bin = request.project_dir.join("node_modules").join(".bin");
    let separator = if cfg!(windows) { ";" } else { ":" };
    let path_key = environment
        .keys()
        .find(|key| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| OsString::from("PATH"));
    let inherited_path = environment.get(&path_key).cloned().unwrap_or_default();
    let mut path = managed_bin.into_os_string();
    if !inherited_path.is_empty() {
        path.push(separator);
        path.push(inherited_path);
    }
    environment.insert(path_key, path);
    command.envs(environment);

    let status = command.status().map_err(RunError::Spawn)?;
    Ok(ChildResult { status })
}

#[cfg(windows)]
fn windows_command_with_arguments(script: &str, arguments: &[String]) -> String {
    let suffix = arguments
        .iter()
        .map(|argument| format!(" \"{}\"", argument.replace('"', "\"\"")))
        .collect::<String>();
    format!("{script}{suffix}")
}

#[cfg(not(windows))]
fn windows_command_with_arguments(script: &str, _arguments: &[String]) -> String {
    script.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn project() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tapid-run-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(path.join("node_modules/.bin")).unwrap();
        path
    }

    #[test]
    fn missing_script_data_is_rejected_before_spawn() {
        let request = RunRequest::new(project(), None);
        assert!(matches!(execute(request), Err(RunError::MissingScript)));
    }

    #[test]
    fn default_shell_runs_opaque_script_in_project_cwd() {
        let dir = project();
        let mut environment = BTreeMap::new();
        environment.insert(
            "TAPID_EXPECTED_CWD".into(),
            fs::canonicalize(&dir)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        fs::write(dir.join("cwd-marker"), "").unwrap();
        let script = if cfg!(windows) {
            r#"if exist cwd-marker (exit /b 0) else (exit /b 1)"#.into()
        } else {
            r#"test "$(pwd)" = "$TAPID_EXPECTED_CWD""#.into()
        };
        let request = RunRequest::new(dir, Some(script)).with_environment(environment);
        let result = execute(request).unwrap();
        assert_eq!(result.exit_code(), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn unix_adapter_preserves_ordered_arguments_at_shell_boundary() {
        let dir = project();
        let request = RunRequest::new(
            dir,
            Some("test \"$1\" = first && test \"$2\" = second".into()),
        )
        .with_arguments(["first", "second"]);
        assert_eq!(execute(request).unwrap().exit_code(), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn managed_bin_precedes_inherited_path_and_overrides_are_inherited() {
        let dir = project();
        let bin = dir.join("node_modules/.bin/marker");
        #[cfg(unix)]
        fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = BTreeMap::new();
        env.insert("TAPID_RUN_MARKER".into(), "present".into());
        let request = RunRequest::new(
            dir,
            Some("test \"$TAPID_RUN_MARKER\" = present && command -v marker".into()),
        )
        .with_environment(env);
        #[cfg(unix)]
        assert_eq!(execute(request).unwrap().exit_code(), Some(0));
    }

    #[test]
    fn child_failure_is_returned_with_original_exit_code() {
        let request = RunRequest::new(project(), Some("exit 37".into()));
        let result = execute(request).unwrap();
        assert_eq!(result.exit_code(), Some(37));
        assert!(!result.success());
    }

    #[cfg(windows)]
    #[test]
    fn windows_child_exit_code_is_not_truncated() {
        let request = RunRequest::new(project(), Some("exit /b 256".into()));
        assert_eq!(execute(request).unwrap().exit_code(), Some(256));
    }

    #[test]
    fn shell_backend_selection_builds_platform_specific_invocations() {
        let unix = ShellBackend::UnixSh.invocation("echo hi", &["a".into(), "b".into()]);
        assert_eq!(unix.program, PathBuf::from("/bin/sh"));
        assert_eq!(
            unix.arguments,
            vec!["-c", "echo hi", "tapid-script", "a", "b"]
        );
        let windows = ShellBackend::WindowsCmd.invocation("echo hi", &["a".into()]);
        assert_eq!(windows.program, PathBuf::from("cmd.exe"));
        #[cfg(windows)]
        assert_eq!(windows.arguments, vec!["/D", "/S", "/C", "echo hi \"a\""]);
        #[cfg(not(windows))]
        assert_eq!(windows.arguments, vec!["/D", "/S", "/C", "echo hi"]);
    }
}
