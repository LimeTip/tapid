use crate::config::{ExecutionLimits, SandboxMode, SandboxPolicy, validate_environment_name};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

/// Identity and lifecycle status of a containment backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendIdentity {
    name: String,
    version: String,
    deprecation: Option<String>,
}

impl BackendIdentity {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn deprecation(&self) -> Option<&str> {
        self.deprecation.as_deref()
    }
}

/// Independently reportable containment dimensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementDimensions {
    filesystem_read: bool,
    filesystem_write: bool,
    network: bool,
    environment_sanitization: bool,
    descendant_lifecycle: bool,
    resource_limits: bool,
}

impl EnforcementDimensions {
    pub const fn none() -> Self {
        Self {
            filesystem_read: false,
            filesystem_write: false,
            network: false,
            environment_sanitization: false,
            descendant_lifecycle: false,
            resource_limits: false,
        }
    }

    pub fn filesystem_read(&self) -> bool {
        self.filesystem_read
    }
    pub fn filesystem_write(&self) -> bool {
        self.filesystem_write
    }
    pub fn network(&self) -> bool {
        self.network
    }
    pub fn environment_sanitization(&self) -> bool {
        self.environment_sanitization
    }
    pub fn descendant_lifecycle(&self) -> bool {
        self.descendant_lifecycle
    }
    pub fn resource_limits(&self) -> bool {
        self.resource_limits
    }

    fn requested_by(policy: &SandboxPolicy) -> Self {
        if policy.mode() == SandboxMode::Disabled {
            return Self::none();
        }
        let limits = policy.limits();
        Self {
            filesystem_read: true,
            filesystem_write: true,
            network: true,
            environment_sanitization: true,
            descendant_lifecycle: true,
            resource_limits: limits.timeout_seconds().is_some()
                || limits.max_output_bytes().is_some()
                || limits.max_processes().is_some()
                || limits.max_memory_bytes().is_some(),
        }
    }
}

/// Canonical filesystem paths actually granted by a backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFilesystemGrants {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

impl ResolvedFilesystemGrants {
    pub fn read(&self) -> &[PathBuf] {
        &self.read
    }

    pub fn write(&self) -> &[PathBuf] {
        &self.write
    }
}

/// The platform backend's ability to enforce the requested containment policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContainmentSupport {
    Supported {
        backend: BackendIdentity,
        requested: EnforcementDimensions,
        enforceable: EnforcementDimensions,
    },
    Unsupported {
        backend: BackendIdentity,
        platform: String,
        reason: String,
        requested: EnforcementDimensions,
        enforceable: EnforcementDimensions,
    },
}

impl ContainmentSupport {
    pub fn backend(&self) -> &BackendIdentity {
        match self {
            Self::Supported { backend, .. } | Self::Unsupported { backend, .. } => backend,
        }
    }

    pub fn requested(&self) -> &EnforcementDimensions {
        match self {
            Self::Supported { requested, .. } | Self::Unsupported { requested, .. } => requested,
        }
    }

    pub fn enforceable(&self) -> &EnforcementDimensions {
        match self {
            Self::Supported { enforceable, .. } | Self::Unsupported { enforceable, .. } => {
                enforceable
            }
        }
    }

    pub fn unsupported_reason(&self) -> Option<&str> {
        match self {
            Self::Supported { .. } => None,
            Self::Unsupported { reason, .. } => Some(reason),
        }
    }
}

/// Evidence returned by an execution backend about actual enforcement.
///
/// Its private fields prevent callers from manufacturing enforcement claims. No receipt is
/// produced until a backend has performed execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementReceipt {
    support: ContainmentSupport,
    enforced: EnforcementDimensions,
    resolved_filesystem: ResolvedFilesystemGrants,
    configured_limits: ExecutionLimits,
}

impl EnforcementReceipt {
    pub fn support(&self) -> &ContainmentSupport {
        &self.support
    }

    pub fn backend(&self) -> &BackendIdentity {
        self.support.backend()
    }

    pub fn requested(&self) -> &EnforcementDimensions {
        self.support.requested()
    }

    pub fn enforced(&self) -> &EnforcementDimensions {
        &self.enforced
    }

    pub fn resolved_filesystem(&self) -> &ResolvedFilesystemGrants {
        &self.resolved_filesystem
    }

    pub fn configured_limits(&self) -> &ExecutionLimits {
        &self.configured_limits
    }
}

/// How execution ended after a backend accepted a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Termination {
    Exited(i32),
    Signaled(i32),
    TimedOut,
    OutputLimitExceeded,
    ProcessLimitExceeded,
    MemoryLimitExceeded,
}

/// Captured execution result paired with an enforcement receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionOutcome {
    termination: Termination,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    enforcement: EnforcementReceipt,
}

impl ExecutionOutcome {
    pub fn termination(&self) -> &Termination {
        &self.termination
    }
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
    pub fn enforcement(&self) -> &EnforcementReceipt {
        &self.enforcement
    }
}

/// Stable high-level execution failure categories for callers and machine output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExecutionErrorCategory {
    InvalidRequest,
    UnsupportedContainment,
    PolicyViolation,
    Spawn,
    Timeout,
    OutputLimit,
    ProcessLimit,
    MemoryLimit,
    Internal,
}

/// An execution failure with a stable category and human-readable context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionError {
    category: ExecutionErrorCategory,
    message: String,
}

impl ExecutionError {
    fn new(category: ExecutionErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    pub fn category(&self) -> ExecutionErrorCategory {
        self.category
    }
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ExecutionError {}

/// Platform-neutral, validated request passed to a private execution backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    program: OsString,
    arguments: Vec<OsString>,
    project_root: PathBuf,
    policy: SandboxPolicy,
    environment: BTreeMap<OsString, OsString>,
}

impl ExecutionRequest {
    pub fn builder(program: impl Into<OsString>) -> ExecutionRequestBuilder {
        ExecutionRequestBuilder {
            program: program.into(),
            arguments: Vec::new(),
            project_root: PathBuf::from("."),
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
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Explicit child environment. Backends must not add ambient variables.
    pub fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }
}

/// Builder for an [`ExecutionRequest`].
#[derive(Clone, Debug)]
pub struct ExecutionRequestBuilder {
    program: OsString,
    arguments: Vec<OsString>,
    project_root: PathBuf,
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

    pub fn project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        self.project_root = project_root.into();
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
        if self.program.is_empty() {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::InvalidRequest,
                "execution program must not be empty",
            ));
        }
        if self.project_root.as_os_str().is_empty() {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::InvalidRequest,
                "project root must not be empty",
            ));
        }
        for name in self.environment.keys() {
            let Some(name) = name.to_str() else {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::InvalidRequest,
                    "environment variable names must be valid UTF-8",
                ));
            };
            if validate_environment_name(name).is_err() {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::InvalidRequest,
                    format!("invalid environment variable name: {name:?}"),
                ));
            }
            if !self
                .policy
                .environment()
                .iter()
                .any(|allowed| allowed == name)
            {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::InvalidRequest,
                    format!("environment variable is not allowlisted by policy: {name}"),
                ));
            }
        }
        Ok(ExecutionRequest {
            program: self.program,
            arguments: self.arguments,
            project_root: self.project_root,
            policy: self.policy,
            environment: self.environment,
        })
    }
}

/// Attempts execution through a private platform backend.
///
/// Platform backends are intentionally not implemented yet. This function performs
/// containment preflight and fails before spawning any child process.
pub fn execute(request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
    match platform_backend::containment_support(request) {
        ContainmentSupport::Unsupported {
            platform, reason, ..
        } => Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            format!("sandbox containment is unavailable on {platform}: {reason}"),
        )),
        ContainmentSupport::Supported { .. } => Err(ExecutionError::new(
            ExecutionErrorCategory::Internal,
            "containment backend reported support but execution is not implemented",
        )),
    }
}

mod platform_backend {
    use super::{BackendIdentity, ContainmentSupport, EnforcementDimensions, ExecutionRequest};

    pub(super) fn containment_support(request: &ExecutionRequest) -> ContainmentSupport {
        ContainmentSupport::Unsupported {
            backend: BackendIdentity {
                name: "tapid-runner/no-backend".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                deprecation: None,
            },
            platform: std::env::consts::OS.to_owned(),
            reason: "no platform execution backend is implemented".to_owned(),
            requested: EnforcementDimensions::requested_by(request.policy()),
            enforceable: EnforcementDimensions::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn required_policy() -> SandboxPolicy {
        SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec!["NODE_ENV".into()],
            true,
            ExecutionLimits::new(None, None, None, None).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn request_builder_preserves_an_explicit_validated_contract() {
        let request = ExecutionRequest::builder("node")
            .arg("script.js")
            .project_root("/project")
            .policy(required_policy())
            .env("NODE_ENV", "test")
            .build()
            .unwrap();

        assert_eq!(request.program(), "node");
        assert_eq!(request.arguments(), &["script.js"]);
        assert_eq!(request.project_root(), std::path::Path::new("/project"));
        assert_eq!(request.policy().mode(), SandboxMode::Required);
        assert_eq!(
            request.environment().get(std::ffi::OsStr::new("NODE_ENV")),
            Some(&std::ffi::OsString::from("test"))
        );
    }

    #[test]
    fn required_mode_preflight_fails_before_a_child_can_create_a_marker() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "tapid-runner-preflight-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("child-spawned");

        #[cfg(unix)]
        let request = ExecutionRequest::builder("/bin/sh")
            .args(["-c".into(), format!("touch {}", marker.display())])
            .project_root(&directory)
            .policy(required_policy())
            .build()
            .unwrap();
        #[cfg(windows)]
        let request = ExecutionRequest::builder("cmd.exe")
            .args(["/C".into(), format!("type nul > \"{}\"", marker.display())])
            .project_root(&directory)
            .policy(required_policy())
            .build()
            .unwrap();

        let error = execute(&request).unwrap_err();
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert!(!marker.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn public_outcome_contract_distinguishes_termination_and_enforcement() {
        fn inspect(outcome: &ExecutionOutcome) {
            let _: &Termination = outcome.termination();
            let _: &[u8] = outcome.stdout();
            let _: &[u8] = outcome.stderr();
            let receipt: &EnforcementReceipt = outcome.enforcement();
            let _: &ContainmentSupport = receipt.support();
            let _: &BackendIdentity = receipt.backend();
            let _: &EnforcementDimensions = receipt.requested();
            let _: &EnforcementDimensions = receipt.enforced();
            let _: &ResolvedFilesystemGrants = receipt.resolved_filesystem();
            let _: &ExecutionLimits = receipt.configured_limits();
        }
        let _ = inspect;
    }

    #[test]
    fn containment_support_reports_backend_capabilities_without_claiming_execution() {
        let request = ExecutionRequest::builder("node")
            .policy(required_policy())
            .build()
            .unwrap();

        let support = platform_backend::containment_support(&request);
        assert!(!support.backend().name().is_empty());
        assert!(!support.backend().version().is_empty());
        assert!(support.backend().deprecation().is_none());
        assert_eq!(support.enforceable(), &EnforcementDimensions::none());
        assert!(support.requested().filesystem_read());
        assert!(support.requested().filesystem_write());
        assert!(support.requested().network());
        assert!(support.requested().environment_sanitization());
        assert!(support.requested().descendant_lifecycle());
        assert!(!support.requested().resource_limits());
        assert!(support.unsupported_reason().is_some());
    }

    #[test]
    fn explicit_environment_must_be_named_and_allowlisted_by_policy() {
        let invalid = ExecutionRequest::builder("node")
            .env("BAD-NAME", "value")
            .build()
            .unwrap_err();
        assert_eq!(invalid.category(), ExecutionErrorCategory::InvalidRequest);

        let denied = ExecutionRequest::builder("node")
            .env("NODE_ENV", "test")
            .build()
            .unwrap_err();
        assert_eq!(denied.category(), ExecutionErrorCategory::InvalidRequest);

        let request = ExecutionRequest::builder("node")
            .policy(required_policy())
            .env("NODE_ENV", "test")
            .build()
            .unwrap();
        assert_eq!(request.environment().len(), 1);
    }
}
