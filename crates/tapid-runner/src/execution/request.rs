//! Validated execution requests, runtime identity, and platform argument/environment limits.

use super::*;

#[cfg(windows)]
fn windows_file_identity(path: &Path) -> Result<(u32, u64), ExecutionError> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file =
        fs::File::open(path).map_err(|error| path_error("trusted Node runtime", path, error))?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `file` keeps a valid owned handle alive for the call and `information` points to
    // writable storage of the exact structure required by `GetFileInformationByHandle`.
    let succeeded = unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as isize, information.as_mut_ptr())
    };
    if succeeded == 0 {
        return Err(path_error(
            "trusted Node runtime identity",
            path,
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: a successful API call initialized the complete output structure.
    let information = unsafe { information.assume_init() };
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, file_index))
}

/// Canonical trusted Node executable with its native file identity captured at construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TrustedNodeRuntime {
    pub(super) path: PathBuf,
    #[cfg(unix)]
    pub(super) identity: NativeIdentity,
    #[cfg(windows)]
    pub(super) volume_serial_number: u32,
    #[cfg(windows)]
    pub(super) file_index: u64,
}

impl TrustedNodeRuntime {
    pub(super) fn checked(path: &Path) -> Result<Self, ExecutionError> {
        let canonical = canonical_path(path, "trusted Node runtime")?;
        if canonical != path || !is_node_executable_name(&canonical) {
            return Err(invalid_request(
                "trusted Node runtime must be canonical and named node or node.exe",
            ));
        }
        let metadata = fs::metadata(&canonical)
            .map_err(|error| path_error("trusted Node runtime", &canonical, error))?;
        if !metadata.is_file() {
            return Err(invalid_request(
                "trusted Node runtime must be a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(invalid_request("trusted Node runtime must be executable"));
            }
            Ok(Self {
                path: canonical,
                identity: NativeIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            })
        }
        #[cfg(windows)]
        {
            let (volume_serial_number, file_index) = windows_file_identity(&canonical)?;
            Ok(Self {
                path: canonical,
                volume_serial_number,
                file_index,
            })
        }
        #[cfg(not(any(unix, windows)))]
        Ok(Self { path: canonical })
    }

    pub(super) fn validate(&self, search_paths: &[PathBuf]) -> Result<(), ExecutionError> {
        #[cfg(not(target_os = "macos"))]
        if search_paths
            .first()
            .and_then(|directory| fs::canonicalize(directory.join(self.path.file_name()?)).ok())
            .as_deref()
            != Some(self.path.as_path())
        {
            return Err(invalid_request(
                "first executable search directory must resolve node to the verified runtime",
            ));
        }
        #[cfg(target_os = "macos")]
        if !search_paths
            .iter()
            .any(|p| Some(p.as_path()) == self.path.parent())
        {
            return Err(invalid_request(
                "search paths must contain the trusted runtime parent",
            ));
        }
        let current = Self::checked(&self.path).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "trusted Node runtime identity changed after request construction",
            )
        })?;
        if &current != self {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "trusted Node runtime identity changed after request construction",
            ));
        }
        Ok(())
    }
}

fn is_node_executable_name(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        #[cfg(windows)]
        return name.to_string_lossy().eq_ignore_ascii_case("node.exe");
        #[cfg(not(windows))]
        return name == OsStr::new("node");
    })
}

/// Platform-neutral, validated request passed to a private execution backend.
///
/// Search paths remain private adapter input: external callers can add them only through the
/// checked builder and cannot replace validated paths after construction.
///
/// ```compile_fail
/// use tapid_runner::ExecutionRequest;
/// let request = ExecutionRequest::builder("node")
///     .executable_search_path("/runtime/bin")
///     .build()
///     .unwrap();
/// let _ = request.executable_search_paths();
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    #[cfg(target_os = "macos")]
    pub(super) reserved_node: Option<platform_backend::ReservedNodeBinding>,
    pub(super) launcher: Option<crate::PrivateLauncher>,
    pub(super) program: OsString,
    pub(super) arguments: Vec<OsString>,
    pub(super) executable_search_paths: Vec<PathBuf>,
    pub(super) trusted_node_runtime: Option<TrustedNodeRuntime>,
    pub(super) windows_verbatim_arguments: bool,
    pub(super) allow_process_memory_stats: bool,
    pub(super) project_root: PathBuf,
    pub(super) working_directory: Option<PathBuf>,
    pub(super) policy: SandboxPolicy,
    pub(super) environment: BTreeMap<OsString, OsString>,
}

impl ExecutionRequest {
    pub fn builder(program: impl Into<OsString>) -> ExecutionRequestBuilder {
        ExecutionRequestBuilder {
            program: program.into(),
            arguments: Vec::new(),
            executable_search_paths: Vec::new(),
            trusted_node_runtime: None,
            windows_verbatim_arguments: false,
            allow_process_memory_stats: false,
            project_root: PathBuf::from("."),
            working_directory: None,
            policy: SandboxPolicy::default(),
            environment: BTreeMap::new(),
        }
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    /// Whether a Windows adapter must pass arguments without MSVCRT re-quoting.
    pub fn uses_windows_verbatim_arguments(&self) -> bool {
        self.windows_verbatim_arguments
    }
    /// Ordered executable search directories for private platform adapters.
    pub(super) fn executable_search_paths(&self) -> &[PathBuf] {
        &self.executable_search_paths
    }
    #[cfg(all(test, unix))]
    pub(super) fn trusted_node_runtime(&self) -> &Path {
        self.trusted_node_runtime
            .as_ref()
            .expect("trusted Node runtime was requested")
            .path
            .as_path()
    }
    pub(super) fn validate_trusted_node_runtime(&self) -> Result<(), ExecutionError> {
        #[cfg(target_os = "macos")]
        if let Some(binding) = &self.reserved_node {
            return binding.validate();
        }
        if let Some(runtime) = &self.trusted_node_runtime {
            runtime.validate(&self.executable_search_paths)?;
        }
        Ok(())
    }
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
    /// Directory used as the child process working directory, confined beneath `project_root`.
    pub fn working_directory(&self) -> &Path {
        self.working_directory
            .as_deref()
            .unwrap_or(&self.project_root)
    }
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Whether the caller explicitly requested process-memory statistics through isolated procfs.
    pub fn allow_process_memory_stats(&self) -> bool {
        self.allow_process_memory_stats
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn private_launcher_executable(&self) -> Option<&Path> {
        self.launcher
            .as_ref()
            .and_then(|launcher| launcher.executable.as_deref())
    }

    /// Explicit caller-provided environment.
    ///
    /// This does not expose the mandatory adapter-owned `PATH`. Private adapters receive the
    /// explicit entries plus `PATH` serialized from the ordered executable-search directories;
    /// an empty directory list produces an explicitly empty `PATH`.
    pub fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }

    /// Complete adapter-owned child environment, including a controlled `PATH` entry.
    pub(super) fn child_environment(&self) -> BTreeMap<OsString, OsString> {
        let mut environment = self.environment.clone();
        let path = join_executable_search_paths(&self.executable_search_paths)
            .expect("validated executable search paths must remain joinable");
        environment.insert(OsString::from("PATH"), path);
        environment
    }

    pub(super) fn validate_windows_verbatim_boundary(&self) -> Result<(), ExecutionError> {
        if !self.windows_verbatim_arguments {
            return Ok(());
        }
        let is_cmd = Path::new(&self.program)
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                name.eq_ignore_ascii_case("cmd") || name.eq_ignore_ascii_case("cmd.exe")
            });
        if !is_cmd
            || self.arguments.len() != 4
            || self.arguments[0] != OsStr::new("/D")
            || self.arguments[1] != OsStr::new("/S")
            || self.arguments[2] != OsStr::new("/C")
        {
            return Err(invalid_request(
                "Windows verbatim arguments require cmd.exe /D /S /C plus one command payload",
            ));
        }
        let payload = self.arguments[3].to_str().ok_or_else(|| {
            invalid_request("Windows verbatim command payload must be valid Unicode")
        })?;
        if payload.contains(['\r', '\n']) {
            return Err(invalid_request(
                "Windows verbatim command payload cannot contain CR or LF",
            ));
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), ExecutionError> {
        self.validate_windows_verbatim_boundary()?;
        #[cfg(not(target_os = "linux"))]
        if self.allow_process_memory_stats {
            return Err(invalid_request(
                "process memory statistics opt-in is supported only by the Linux Restricted backend",
            ));
        }
        #[cfg(target_os = "linux")]
        if self.allow_process_memory_stats
            && (self.policy.mode() != SandboxMode::Required
                || self.policy.assurance() != AssuranceLevel::Restricted)
        {
            return Err(invalid_request(
                "process memory statistics opt-in requires required sandbox mode and Linux Restricted assurance",
            ));
        }
        #[cfg(target_os = "linux")]
        if self.allow_process_memory_stats && self.private_launcher_executable().is_none() {
            return Err(invalid_request(
                "process memory statistics opt-in requires early private-launcher initialization",
            ));
        }
        let program_units =
            validate_os_value("execution program", &self.program, MAX_PROGRAM_UNITS)?;
        if self.program.is_empty() {
            return Err(invalid_request("execution program must not be empty"));
        }
        if self.arguments.len() > MAX_ARGUMENT_COUNT {
            return Err(invalid_request(format!(
                "execution request exceeds {MAX_ARGUMENT_COUNT} arguments"
            )));
        }
        #[cfg(windows)]
        let mut argument_units = Vec::with_capacity(self.arguments.len());
        #[cfg(not(windows))]
        let mut argv_units = program_units.saturating_add(1);
        for argument in &self.arguments {
            let units = validate_os_value("execution argument", argument, MAX_ARGUMENT_UNITS)?;
            #[cfg(windows)]
            argument_units.push(units);
            #[cfg(not(windows))]
            {
                argv_units = argv_units.saturating_add(units).saturating_add(1);
            }
        }
        #[cfg(windows)]
        {
            validate_windows_command_line_units(program_units, argument_units.iter().copied())?;
            use std::os::windows::ffi::OsStrExt;
            let program = self.program.encode_wide().collect::<Vec<_>>();
            let arguments = self
                .arguments
                .iter()
                .map(|argument| argument.encode_wide().collect::<Vec<_>>())
                .collect::<Vec<_>>();
            serialize_windows_command_line_units(
                &program,
                &arguments,
                self.windows_verbatim_arguments,
            )?;
        }
        #[cfg(not(windows))]
        if argv_units > MAX_ARGV_UNITS {
            return Err(invalid_request(format!(
                "execution argv exceeds {MAX_ARGV_UNITS} bytes/code units"
            )));
        }
        validate_os_value(
            "project root",
            self.project_root.as_os_str(),
            MAX_PROJECT_ROOT_UNITS,
        )?;
        if self.project_root.as_os_str().is_empty() {
            return Err(invalid_request("project root must not be empty"));
        }
        if let Some(working_directory) = &self.working_directory {
            validate_os_value(
                "working directory",
                working_directory.as_os_str(),
                MAX_PROJECT_ROOT_UNITS,
            )?;
            if working_directory.as_os_str().is_empty() {
                return Err(invalid_request("working directory must not be empty"));
            }
            let canonical_root = canonical_path(&self.project_root, "project root")?;
            let canonical_working_directory =
                canonical_path(working_directory, "working directory")?;
            if canonical_root != self.project_root
                || canonical_working_directory != *working_directory
                || !canonical_working_directory.starts_with(&canonical_root)
                || !canonical_working_directory.is_dir()
            {
                return Err(invalid_request(
                    "working directory must be canonical, existing, and contained beneath project root",
                ));
            }
        }
        let executable_search_path =
            validate_executable_search_paths(&self.executable_search_paths)?;
        self.validate_trusted_node_runtime()?;
        // The complete block always contains PATH=<joined paths>\0 followed by the block's final
        // terminator. An empty path list therefore still contributes `PATH=\0\0`.
        let mut environment_units = 1usize
            .checked_add(os_units(OsStr::new("PATH")))
            .and_then(|units| units.checked_add(os_units(&executable_search_path)))
            .and_then(|units| units.checked_add(2))
            .ok_or_else(environment_block_too_large)?;
        if environment_units > MAX_ENVIRONMENT_BLOCK_UNITS {
            return Err(environment_block_too_large());
        }
        for (name, value) in &self.environment {
            let Some(name) = name.to_str() else {
                return Err(invalid_request(
                    "environment variable names must be valid UTF-8",
                ));
            };
            if name.eq_ignore_ascii_case("PATH") {
                return Err(invalid_request(
                    "caller-controlled PATH is forbidden; containment backends own executable search paths",
                ));
            }
            if validate_environment_name(name).is_err() {
                return Err(invalid_request(format!(
                    "invalid environment variable name: {name:?}"
                )));
            }
            let name_units = os_units(name.as_ref());
            let value_units = validate_os_value(
                "environment variable value",
                value,
                MAX_ENVIRONMENT_VALUE_UNITS,
            )?;
            environment_units = environment_units
                .checked_add(name_units)
                .and_then(|units| units.checked_add(value_units))
                .and_then(|units| units.checked_add(2))
                .ok_or_else(environment_block_too_large)?;
            if environment_units > MAX_ENVIRONMENT_BLOCK_UNITS {
                return Err(environment_block_too_large());
            }
            if !self
                .policy
                .environment()
                .iter()
                .any(|allowed| allowed == name)
            {
                return Err(invalid_request(format!(
                    "environment variable is not allowlisted by policy: {name}"
                )));
            }
        }
        #[cfg(windows)]
        windows_environment_block_units(&self.child_environment())?;
        Ok(())
    }
}

/// Builder for an [`ExecutionRequest`].
#[derive(Clone, Debug)]
pub struct ExecutionRequestBuilder {
    program: OsString,
    arguments: Vec<OsString>,
    executable_search_paths: Vec<PathBuf>,
    trusted_node_runtime: Option<PathBuf>,
    windows_verbatim_arguments: bool,
    allow_process_memory_stats: bool,
    project_root: PathBuf,
    working_directory: Option<PathBuf>,
    policy: SandboxPolicy,
    environment: BTreeMap<OsString, OsString>,
}

impl ExecutionRequestBuilder {
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Adds one executable search directory after existing entries.
    ///
    /// [`Self::build`] rejects entries that the target platform's [`std::env::join_paths`] cannot
    /// serialize, exact duplicates, and requests whose complete child environment (including the
    /// resulting mandatory `PATH`) exceeds [`MAX_ENVIRONMENT_BLOCK_UNITS`].
    pub fn executable_search_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable_search_paths.push(path.into());
        self
    }

    /// Binds the exact canonical Node executable to the first child PATH directory.
    pub fn trusted_node_runtime(mut self, path: impl Into<PathBuf>) -> Self {
        self.trusted_node_runtime = Some(path.into());
        self
    }

    /// Requires the Windows adapter to use `CommandExt::raw_arg` or an equivalent verbatim seam.
    pub fn windows_verbatim_arguments(mut self, enabled: bool) -> Self {
        self.windows_verbatim_arguments = enabled;
        self
    }

    /// Enables read-only procfs access inside a private PID/mount namespace for this process and its descendants.
    pub fn allow_process_memory_stats(mut self, enabled: bool) -> Self {
        self.allow_process_memory_stats = enabled;
        self
    }

    /// Adds ordered executable search directories without consulting ambient `PATH`.
    ///
    /// [`Self::build`] validates every entry's representation and size before duplicate checks,
    /// then applies the target platform's exact [`std::env::join_paths`] serialization rules.
    pub fn executable_search_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.executable_search_paths
            .extend(paths.into_iter().map(Into::into));
        self
    }

    pub fn project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        self.project_root = project_root.into();
        self
    }

    /// Sets the child process working directory, which must resolve inside `project_root`.
    pub fn working_directory(mut self, working_directory: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(working_directory.into());
        self
    }

    pub fn policy(mut self, policy: SandboxPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Adds one explicit child environment value.
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.insert(name.into(), value.into());
        self
    }

    /// Adds explicit child environment values without reading the ambient environment.
    pub fn envs<I, K, V>(mut self, environment: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.environment.extend(
            environment
                .into_iter()
                .map(|(name, value)| (name.into(), value.into())),
        );
        self
    }

    pub fn build(self) -> Result<ExecutionRequest, ExecutionError> {
        let working_directory = self
            .working_directory
            .as_ref()
            .map(|working_directory| {
                let project_root = fs::canonicalize(&self.project_root)
                    .map_err(|error| path_error("project root", &self.project_root, error))?;
                if !project_root.is_dir() {
                    return Err(invalid_request(
                        "project root must be an existing directory",
                    ));
                }
                let working_path = if working_directory.is_absolute() {
                    working_directory.clone()
                } else {
                    project_root.join(working_directory)
                };
                let working_directory = fs::canonicalize(&working_path)
                    .map_err(|error| path_error("working directory", &working_path, error))?;
                if !working_directory.is_dir() || !working_directory.starts_with(&project_root) {
                    return Err(invalid_request(
                        "working directory must be an existing directory beneath project root",
                    ));
                }
                Ok(working_directory)
            })
            .transpose()?;
        let trusted_node_runtime = self
            .trusted_node_runtime
            .as_deref()
            .map(TrustedNodeRuntime::checked)
            .transpose()?;
        let request = ExecutionRequest {
            #[cfg(target_os = "macos")]
            reserved_node: None,
            launcher: crate::LAUNCHER.get().cloned(),
            program: self.program,
            arguments: self.arguments,
            executable_search_paths: self.executable_search_paths,
            trusted_node_runtime,
            windows_verbatim_arguments: self.windows_verbatim_arguments,
            allow_process_memory_stats: self.allow_process_memory_stats,
            project_root: self.project_root,
            working_directory,
            policy: self.policy,
            environment: self.environment,
        };
        request.validate()?;
        Ok(request)
    }
}

pub(super) fn validate_executable_search_paths(
    paths: &[PathBuf],
) -> Result<OsString, ExecutionError> {
    if paths.len() > MAX_EXECUTABLE_SEARCH_PATH_COUNT {
        return Err(invalid_request(format!(
            "execution request exceeds {MAX_EXECUTABLE_SEARCH_PATH_COUNT} executable search directories"
        )));
    }

    // Bound and validate every value before any duplicate comparison. This keeps comparisons over
    // attacker-controlled native strings bounded even when an earlier value is repeated.
    for path in paths {
        validate_os_value(
            "executable search directory",
            path.as_os_str(),
            MAX_EXECUTABLE_SEARCH_PATH_UNITS,
        )?;
    }

    let joined = join_executable_search_paths(paths)?;
    let total_units = os_units(&joined)
        .checked_add(1)
        .ok_or_else(executable_search_path_too_large)?;
    if total_units > MAX_EXECUTABLE_SEARCH_PATHS_UNITS {
        return Err(executable_search_path_too_large());
    }

    let mut seen = BTreeSet::new();
    for path in paths {
        if !seen.insert(path.as_os_str()) {
            return Err(invalid_request(format!(
                "duplicate executable search directory: {}",
                path.display()
            )));
        }
    }
    Ok(joined)
}

pub(super) fn join_executable_search_paths(paths: &[PathBuf]) -> Result<OsString, ExecutionError> {
    let paths = {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            paths
                .iter()
                .map(|path| {
                    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
                    PathBuf::from(OsString::from_wide(&windows_environment_path_units(&units)))
                })
                .collect::<Vec<_>>()
        }
        #[cfg(not(windows))]
        {
            paths.to_vec()
        }
    };
    std::env::join_paths(paths).map_err(|error| {
        invalid_request(format!(
            "executable search directory cannot be joined into PATH: {error}"
        ))
    })
}

fn executable_search_path_too_large() -> ExecutionError {
    invalid_request(format!(
        "executable search path exceeds {MAX_EXECUTABLE_SEARCH_PATHS_UNITS} bytes/code units"
    ))
}

fn environment_block_too_large() -> ExecutionError {
    invalid_request(format!(
        "execution environment exceeds {MAX_ENVIRONMENT_BLOCK_UNITS} bytes/code units"
    ))
}

fn invalid_request(message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCategory::InvalidRequest, message)
}

#[cfg(any(windows, test))]
pub(super) fn serialize_windows_command_line_units(
    program: &[u16],
    arguments: &[Vec<u16>],
    verbatim_last: bool,
) -> Result<Vec<u16>, ExecutionError> {
    if program.is_empty() || program.contains(&0) || arguments.iter().any(|arg| arg.contains(&0)) {
        return Err(invalid_request(
            "Windows command line contains an empty program or embedded NUL",
        ));
    }
    if verbatim_last && arguments.is_empty() {
        return Err(invalid_request(
            "Windows verbatim command line requires a final command payload",
        ));
    }

    fn quote(argument: &[u16], output: &mut Vec<u16>) {
        let slash = u16::from(b'\\');
        let quote = u16::from(b'"');
        output.push(quote);
        let mut backslashes = 0usize;
        for &unit in argument {
            if unit == slash {
                backslashes += 1;
            } else if unit == quote {
                output.extend(std::iter::repeat_n(slash, backslashes * 2 + 1));
                output.push(quote);
                backslashes = 0;
            } else {
                output.extend(std::iter::repeat_n(slash, backslashes));
                output.push(unit);
                backslashes = 0;
            }
        }
        output.extend(std::iter::repeat_n(slash, backslashes * 2));
        output.push(quote);
    }

    let mut command_line = Vec::new();
    quote(program, &mut command_line);
    for argument in arguments {
        command_line.push(u16::from(b' '));
        if verbatim_last {
            // The checked verbatim boundary permits only /D /S /C and one
            // command payload. cmd.exe parses its own command line: quoting
            // these switches changes how it finds and strips payload quotes.
            command_line.extend_from_slice(argument);
        } else {
            quote(argument, &mut command_line);
        }
    }
    command_line.push(0);
    if command_line.len() > MAX_ARGV_UNITS {
        return Err(invalid_request(format!(
            "serialized Windows command line exceeds {MAX_ARGV_UNITS} UTF-16 code units"
        )));
    }
    Ok(command_line)
}

/// Returns a conservative upper bound rather than the exact Windows command-line serialization.
/// The program and every argument are assumed to need surrounding quotes, and every input code
/// unit is budgeted to double under worst-case quote/backslash escaping. Separators and the
/// mandatory terminating NUL are counted separately.
#[cfg(any(windows, test))]
pub(super) fn windows_command_line_units_upper_bound(
    program_units: usize,
    argument_units: impl IntoIterator<Item = usize>,
) -> Option<usize> {
    let mut units = program_units
        .checked_mul(2)?
        .checked_add(2)?
        .checked_add(1)?;
    for argument_units in argument_units {
        units = units
            .checked_add(1)?
            .checked_add(argument_units.checked_mul(2)?.checked_add(2)?)?;
    }
    Some(units)
}

#[cfg(any(windows, test))]
pub(super) fn validate_windows_command_line_units(
    program_units: usize,
    argument_units: impl IntoIterator<Item = usize>,
) -> Result<(), ExecutionError> {
    if windows_command_line_units_upper_bound(program_units, argument_units)
        .is_none_or(|units| units > MAX_ARGV_UNITS)
    {
        return Err(invalid_request(format!(
            "execution argv may exceed {MAX_ARGV_UNITS} UTF-16 code units after Windows command-line serialization"
        )));
    }
    Ok(())
}

#[cfg(any(windows, test))]
pub(super) fn windows_environment_block_units(
    environment: &BTreeMap<OsString, OsString>,
) -> Result<Vec<u16>, ExecutionError> {
    let mut entries = Vec::with_capacity(environment.len());
    for (name, value) in environment {
        let name = name.to_str().ok_or_else(|| {
            invalid_request("Windows environment variable names must be valid UTF-8")
        })?;
        validate_environment_name(name).map_err(|error| invalid_request(error.to_string()))?;
        let name_units = name.encode_utf16().collect::<Vec<_>>();
        let value_units = {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                value.encode_wide().collect::<Vec<_>>()
            }
            #[cfg(not(windows))]
            {
                value.to_string_lossy().encode_utf16().collect::<Vec<_>>()
            }
        };
        entries.push((name_units, value_units));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let local_app_data = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows AppContainer launch requires the runner's LOCALAPPDATA environment variable",
            )
        })?;
        let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
            ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows AppContainer launch requires the runner's SystemRoot environment variable",
            )
        })?;
        let local_app_data_units = local_app_data.encode_wide().collect::<Vec<_>>();
        let system_root_units = system_root.encode_wide().collect::<Vec<_>>();
        entries = windows_appcontainer_environment_entries(
            entries,
            &local_app_data_units,
            &system_root_units,
        );
    }
    assemble_windows_environment_block(entries)
}

#[cfg(any(windows, test))]
pub(super) fn windows_appcontainer_environment_entries(
    mut entries: Vec<(Vec<u16>, Vec<u16>)>,
    host_local_app_data: &[u16],
    host_system_root: &[u16],
) -> Vec<(Vec<u16>, Vec<u16>)> {
    for (name, value) in [
        ("LOCALAPPDATA", host_local_app_data),
        ("SYSTEMROOT", host_system_root),
    ] {
        let required_name = name.encode_utf16().collect::<Vec<_>>();
        entries
            .retain(|(candidate, _)| !windows_environment_names_equal(candidate, &required_name));
        entries.push((required_name, value.to_vec()));
    }
    entries
}

#[cfg(any(windows, test))]
fn windows_environment_names_equal(left: &[u16], right: &[u16]) -> bool {
    fn fold_ascii_case(unit: u16) -> u16 {
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
            unit + u16::from(b'a' - b'A')
        } else {
            unit
        }
    }
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| fold_ascii_case(left) == fold_ascii_case(right))
}

#[cfg(any(windows, test))]
pub(super) fn assemble_windows_environment_block(
    mut entries: Vec<(Vec<u16>, Vec<u16>)>,
) -> Result<Vec<u16>, ExecutionError> {
    fn fold_ascii_case(unit: u16) -> u16 {
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
            unit + u16::from(b'a' - b'A')
        } else {
            unit
        }
    }
    fn compare_names(left: &[u16], right: &[u16]) -> std::cmp::Ordering {
        left.iter()
            .map(|unit| fold_ascii_case(*unit))
            .cmp(right.iter().map(|unit| fold_ascii_case(*unit)))
    }

    for (name, value) in &entries {
        if name.is_empty()
            || name
                .iter()
                .any(|unit| *unit == 0 || *unit == u16::from(b'='))
        {
            return Err(invalid_request("invalid Windows environment variable name"));
        }
        if value.contains(&0) {
            return Err(invalid_request(
                "Windows environment value contains an embedded NUL",
            ));
        }
    }
    entries.sort_by(|left, right| compare_names(&left.0, &right.0));
    if entries
        .windows(2)
        .any(|pair| compare_names(&pair[0].0, &pair[1].0).is_eq())
    {
        return Err(invalid_request(
            "Windows environment contains case-insensitive duplicate names",
        ));
    }
    let mut units: usize = if entries.is_empty() { 2 } else { 1 };
    for (name, value) in &entries {
        units = units
            .checked_add(name.len())
            .and_then(|count| count.checked_add(1))
            .and_then(|count| count.checked_add(value.len()))
            .and_then(|count| count.checked_add(1))
            .ok_or_else(environment_block_too_large)?;
    }
    if units > MAX_ENVIRONMENT_BLOCK_UNITS {
        return Err(environment_block_too_large());
    }
    if entries.is_empty() {
        return Ok(vec![0, 0]);
    }

    let mut block = Vec::with_capacity(units);
    for (name, value) in entries {
        block.extend(name);
        block.push(u16::from(b'='));
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Mirrors `std::env::join_paths` on Windows so its separator quoting remains host-testable.
#[cfg(test)]
pub(super) fn join_windows_path_units<'a>(
    paths: impl IntoIterator<Item = &'a [u16]>,
) -> Result<Vec<u16>, ()> {
    const SEPARATOR: u16 = b';' as u16;
    const QUOTE: u16 = b'"' as u16;
    let mut joined = Vec::new();
    for (index, path) in paths.into_iter().enumerate() {
        if index != 0 {
            joined.push(SEPARATOR);
        }
        if path.contains(&QUOTE) {
            return Err(());
        }
        if path.contains(&SEPARATOR) {
            joined.push(QUOTE);
            joined.extend_from_slice(path);
            joined.push(QUOTE);
        } else {
            joined.extend_from_slice(path);
        }
    }
    Ok(joined)
}

#[cfg(any(windows, test))]
pub(super) fn windows_environment_path_units(path: &[u16]) -> Vec<u16> {
    const EXTENDED_PREFIX: [u16; 4] = [92, 92, 63, 92];
    const EXTENDED_UNC_PREFIX: [u16; 8] = [92, 92, 63, 92, 85, 78, 67, 92];
    if path.starts_with(&EXTENDED_UNC_PREFIX) {
        let mut ordinary = vec![92, 92];
        ordinary.extend_from_slice(&path[EXTENDED_UNC_PREFIX.len()..]);
        ordinary
    } else if path.starts_with(&EXTENDED_PREFIX) {
        path[EXTENDED_PREFIX.len()..].to_vec()
    } else {
        path.to_vec()
    }
}

#[cfg(any(windows, test))]
pub(super) fn windows_path_units_semantically_equal(left: &[u16], right: &[u16]) -> bool {
    fn fold_ascii_case(unit: u16) -> u16 {
        if unit >= u16::from(b'A') && unit <= u16::from(b'Z') {
            unit + u16::from(b'a' - b'A')
        } else {
            unit
        }
    }

    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| fold_ascii_case(left) == fold_ascii_case(right))
}

fn validate_os_value(kind: &str, value: &OsStr, maximum: usize) -> Result<usize, ExecutionError> {
    validate_platform_representation(kind, value)?;
    let units = os_units(value);
    if os_contains_nul(value) {
        return Err(invalid_request(format!("{kind} contains an embedded NUL")));
    }
    if units > maximum {
        return Err(invalid_request(format!(
            "{kind} exceeds {maximum} bytes/code units"
        )));
    }
    Ok(units)
}

#[cfg(any(unix, windows))]
fn validate_platform_representation(_kind: &str, _value: &OsStr) -> Result<(), ExecutionError> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_platform_representation(kind: &str, value: &OsStr) -> Result<(), ExecutionError> {
    if value.to_str().is_none() {
        return Err(invalid_request(format!(
            "{kind} cannot be represented on this platform"
        )));
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn os_units(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().len()
}

#[cfg(unix)]
fn os_contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().contains(&0)
}

#[cfg(windows)]
pub(super) fn os_units(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().count()
}

#[cfg(windows)]
fn os_contains_nul(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().any(|unit| unit == 0)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn os_units(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(not(any(unix, windows)))]
fn os_contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}
