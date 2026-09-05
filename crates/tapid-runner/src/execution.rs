use crate::config::SandboxPolicy;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

/// The platform backend's ability to enforce the requested containment policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContainmentSupport {
    Supported,
    Unsupported { platform: String, reason: String },
}

/// Evidence returned by an execution backend about actual enforcement.
///
/// No receipt is produced until a backend has performed execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementReceipt {
    support: ContainmentSupport,
    enforced: bool,
}

impl EnforcementReceipt {
    pub fn support(&self) -> &ContainmentSupport {
        &self.support
    }

    pub fn enforced(&self) -> bool {
        self.enforced
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
}

impl ExecutionRequest {
    pub fn builder(program: impl Into<OsString>) -> ExecutionRequestBuilder {
        ExecutionRequestBuilder {
            program: program.into(),
            arguments: Vec::new(),
            project_root: PathBuf::from("."),
            policy: SandboxPolicy::default(),
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
}

/// Builder for an [`ExecutionRequest`].
#[derive(Clone, Debug)]
pub struct ExecutionRequestBuilder {
    program: OsString,
    arguments: Vec<OsString>,
    project_root: PathBuf,
    policy: SandboxPolicy,
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
        Ok(ExecutionRequest {
            program: self.program,
            arguments: self.arguments,
            project_root: self.project_root,
            policy: self.policy,
        })
    }
}

/// Attempts execution through a private platform backend.
///
/// Platform backends are intentionally not implemented yet. This function performs
/// containment preflight and fails before spawning any child process.
pub fn execute(request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
    let support = platform_backend::containment_support(request);
    match support {
        ContainmentSupport::Unsupported { platform, reason } => Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            format!("sandbox containment is unavailable on {platform}: {reason}"),
        )),
        ContainmentSupport::Supported => Err(ExecutionError::new(
            ExecutionErrorCategory::Internal,
            "containment backend reported support but execution is not implemented",
        )),
    }
}

mod platform_backend {
    use super::{ContainmentSupport, ExecutionRequest};

    pub(super) fn containment_support(_request: &ExecutionRequest) -> ContainmentSupport {
        ContainmentSupport::Unsupported {
            platform: std::env::consts::OS.to_owned(),
            reason: "no platform execution backend is implemented".to_owned(),
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
            vec![],
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
            .build()
            .unwrap();

        assert_eq!(request.program(), "node");
        assert_eq!(request.arguments(), &["script.js"]);
        assert_eq!(request.project_root(), std::path::Path::new("/project"));
        assert_eq!(request.policy().mode(), SandboxMode::Required);
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
            let _: bool = receipt.enforced();
        }
        let _ = inspect;
    }
}
