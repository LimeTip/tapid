use crate::config::{ExecutionLimits, SandboxMode, SandboxPolicy, validate_environment_name};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
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
    subprocess_restriction: bool,
    descendant_lifecycle: bool,
    timeout: bool,
    output: bool,
    process_count: bool,
    memory: bool,
}

impl EnforcementDimensions {
    pub const fn none() -> Self {
        Self {
            filesystem_read: false,
            filesystem_write: false,
            network: false,
            environment_sanitization: false,
            subprocess_restriction: false,
            descendant_lifecycle: false,
            timeout: false,
            output: false,
            process_count: false,
            memory: false,
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
    pub fn subprocess_restriction(&self) -> bool {
        self.subprocess_restriction
    }
    pub fn descendant_lifecycle(&self) -> bool {
        self.descendant_lifecycle
    }
    pub fn timeout(&self) -> bool {
        self.timeout
    }
    pub fn output(&self) -> bool {
        self.output
    }
    pub fn process_count(&self) -> bool {
        self.process_count
    }
    pub fn memory(&self) -> bool {
        self.memory
    }
    /// Compatibility summary; prefer the individual resource accessors.
    pub fn resource_limits(&self) -> bool {
        self.timeout || self.output || self.process_count || self.memory
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
            subprocess_restriction: !policy.subprocess(),
            descendant_lifecycle: true,
            timeout: limits.timeout_seconds().is_some(),
            output: limits.max_output_bytes().is_some(),
            process_count: limits.max_processes().is_some(),
            memory: limits.max_memory_bytes().is_some(),
        }
    }

    #[allow(dead_code)] // Used by checked construction when a platform backend lands.
    fn contains(&self, required: &Self) -> bool {
        (!required.filesystem_read || self.filesystem_read)
            && (!required.filesystem_write || self.filesystem_write)
            && (!required.network || self.network)
            && (!required.environment_sanitization || self.environment_sanitization)
            && (!required.subprocess_restriction || self.subprocess_restriction)
            && (!required.descendant_lifecycle || self.descendant_lifecycle)
            && (!required.timeout || self.timeout)
            && (!required.output || self.output)
            && (!required.process_count || self.process_count)
            && (!required.memory || self.memory)
    }
}

/// Absolute filesystem paths granted by a backend after canonical preflight. Existing targets are
/// canonical; missing targets are bound to a canonical existing ancestor and validated suffix.
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum GrantResolution {
    ExistingCanonical,
    MissingTarget {
        canonical_ancestor: PathBuf,
        relative_target: PathBuf,
    },
    RuntimeCanonical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedGrant {
    path: PathBuf,
    resolution: GrantResolution,
}

/// Explicit backend runtime paths added to the declarative project policy.
/// Construction is private so additions are canonical before preflight succeeds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RuntimeFilesystemAdditions {
    read: Vec<ResolvedGrant>,
    write: Vec<ResolvedGrant>,
}

impl RuntimeFilesystemAdditions {
    #[allow(dead_code)] // Used by platform backends when runtime grants are required.
    fn checked(read: Vec<PathBuf>, write: Vec<PathBuf>) -> Result<Self, ExecutionError> {
        Ok(Self {
            read: read
                .into_iter()
                .map(resolve_runtime_grant)
                .collect::<Result<_, _>>()?,
            write: write
                .into_iter()
                .map(resolve_runtime_grant)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedSandboxPolicy {
    mode: SandboxMode,
    project_root: PathBuf,
    read: Vec<ResolvedGrant>,
    write: Vec<ResolvedGrant>,
    grants: ResolvedFilesystemGrants,
    limits: ExecutionLimits,
}

/// Private proof that support and the exact resolved policy passed pre-spawn checks.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedPreflight {
    support: ContainmentSupport,
    policy: ResolvedSandboxPolicy,
}

impl ResolvedSandboxPolicy {
    fn validate(&self) -> Result<(), ExecutionError> {
        let projected = ResolvedFilesystemGrants {
            read: self.read.iter().map(|grant| grant.path.clone()).collect(),
            write: self.write.iter().map(|grant| grant.path.clone()).collect(),
        };
        if projected != self.grants {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "resolved filesystem receipt grants omit or add preflight paths",
            ));
        }
        for grant in self.read.iter().chain(&self.write) {
            match &grant.resolution {
                GrantResolution::ExistingCanonical => {
                    validate_canonical_path(&grant.path)?;
                    if !grant.path.starts_with(&self.project_root) {
                        return Err(grant_confinement_error(&grant.path));
                    }
                }
                GrantResolution::MissingTarget {
                    canonical_ancestor,
                    relative_target,
                } => {
                    validate_canonical_path(canonical_ancestor)?;
                    if !canonical_ancestor.starts_with(&self.project_root)
                        || relative_target.is_absolute()
                        || relative_target
                            .components()
                            .any(|component| !matches!(component, std::path::Component::Normal(_)))
                        || canonical_ancestor.join(relative_target) != grant.path
                    {
                        return Err(grant_confinement_error(&grant.path));
                    }
                }
                GrantResolution::RuntimeCanonical => validate_canonical_path(&grant.path)?,
            }
        }
        Ok(())
    }
}

fn validate_canonical_path(path: &Path) -> Result<(), ExecutionError> {
    if !path.is_absolute() || canonical_path(path, "filesystem grant")? != path {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!(
                "filesystem grant is not absolute and canonical: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn grant_confinement_error(path: &Path) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::PolicyViolation,
        format!(
            "filesystem grant is neither project-confined nor an approved runtime addition: {}",
            path.display()
        ),
    )
}

/// The platform backend's ability to enforce the requested containment policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContainmentSupport {
    Supported {
        backend: BackendIdentity,
        requested: EnforcementDimensions,
        declared: EnforcementDimensions,
        observed: EnforcementDimensions,
    },
    Unsupported {
        backend: BackendIdentity,
        platform: String,
        reason: String,
        requested: EnforcementDimensions,
        declared: EnforcementDimensions,
        observed: EnforcementDimensions,
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

    /// Backend capabilities declared by its implementation.
    pub fn declared(&self) -> &EnforcementDimensions {
        match self {
            Self::Supported { declared, .. } | Self::Unsupported { declared, .. } => declared,
        }
    }

    /// Capabilities confirmed by runtime probes for this backend.
    pub fn observed(&self) -> &EnforcementDimensions {
        match self {
            Self::Supported { observed, .. } | Self::Unsupported { observed, .. } => observed,
        }
    }

    /// Compatibility alias for [`Self::declared`].
    pub fn enforceable(&self) -> &EnforcementDimensions {
        self.declared()
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
///
/// ```compile_fail
/// use tapid_runner::{ContainmentSupport, EnforcementDimensions, EnforcementReceipt,
///     ExecutionLimits, ResolvedFilesystemGrants};
/// fn forge(support: ContainmentSupport, enforced: EnforcementDimensions,
///     resolved_filesystem: ResolvedFilesystemGrants, configured_limits: ExecutionLimits) {
///     let _ = EnforcementReceipt { support, enforced, resolved_filesystem, configured_limits };
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementReceipt {
    support: ContainmentSupport,
    enforced: EnforcementDimensions,
    resolved_filesystem: ResolvedFilesystemGrants,
    configured_limits: ExecutionLimits,
}

impl EnforcementReceipt {
    #[allow(dead_code)] // The no-backend scaffold cannot produce receipts yet.
    fn checked(
        preflight: &ValidatedPreflight,
        enforced: EnforcementDimensions,
    ) -> Result<Self, ExecutionError> {
        let support = &preflight.support;
        if !matches!(support, ContainmentSupport::Supported { .. }) {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "cannot issue an enforcement receipt for unsupported containment",
            ));
        }
        let requested = support.requested();
        if preflight.policy.mode == SandboxMode::Disabled {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "disabled sandbox execution cannot issue an enforcement receipt",
            ));
        }
        if enforced != *requested {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "enforced dimensions must exactly match requested restrictions",
            ));
        }
        if !support.declared().contains(requested) || !support.observed().contains(requested) {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "requested restrictions lack declared or observed backend support",
            ));
        }
        let configured_limits = &preflight.policy.limits;
        if requested.timeout != configured_limits.timeout_seconds().is_some()
            || requested.output != configured_limits.max_output_bytes().is_some()
            || requested.process_count != configured_limits.max_processes().is_some()
            || requested.memory != configured_limits.max_memory_bytes().is_some()
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "configured limits do not match requested resource restrictions",
            ));
        }
        Ok(Self {
            support: support.clone(),
            enforced,
            resolved_filesystem: preflight.policy.grants.clone(),
            configured_limits: configured_limits.clone(),
        })
    }

    pub fn support(&self) -> &ContainmentSupport {
        &self.support
    }

    pub fn backend(&self) -> &BackendIdentity {
        self.support.backend()
    }

    pub fn requested(&self) -> &EnforcementDimensions {
        self.support.requested()
    }

    pub fn declared(&self) -> &EnforcementDimensions {
        self.support.declared()
    }

    pub fn observed(&self) -> &EnforcementDimensions {
        self.support.observed()
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
///
/// ```compile_fail
/// use tapid_runner::{EnforcementReceipt, ExecutionOutcome, Termination};
/// fn forge(enforcement: EnforcementReceipt) {
///     let _ = ExecutionOutcome {
///         termination: Termination::Exited(0), stdout: vec![], stderr: vec![], enforcement
///     };
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionOutcome {
    termination: Termination,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    enforcement: EnforcementReceipt,
}

impl ExecutionOutcome {
    #[allow(dead_code)] // The no-backend scaffold cannot produce outcomes yet.
    fn checked(
        termination: Termination,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        enforcement: EnforcementReceipt,
    ) -> Result<Self, ExecutionError> {
        if enforcement.enforced() != enforcement.requested() {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "execution outcome lacks complete enforcement evidence",
            ));
        }
        Ok(Self {
            termination,
            stdout,
            stderr,
            enforcement,
        })
    }

    fn validate_for_preflight(&self, preflight: &ValidatedPreflight) -> Result<(), ExecutionError> {
        let required = preflight.support.requested();
        let receipt = self.enforcement();
        if receipt.support() != &preflight.support
            || receipt.requested() != required
            || receipt.enforced() != required
            || !receipt.declared().contains(required)
            || !receipt.observed().contains(required)
            || receipt.configured_limits() != &preflight.policy.limits
            || receipt.resolved_filesystem() != &preflight.policy.grants
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "execution outcome does not match the exact validated preflight",
            ));
        }
        Ok(())
    }

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
    execute_with_backend(request, &platform_backend::PlatformBackend)
}

trait ExecutionBackend {
    fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport;

    fn runtime_filesystem_additions(
        &self,
        _request: &ExecutionRequest,
    ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
        Ok(RuntimeFilesystemAdditions::default())
    }

    fn spawn(
        &self,
        request: &ExecutionRequest,
        preflight: &ValidatedPreflight,
    ) -> Result<ExecutionOutcome, ExecutionError>;
}

fn execute_with_backend(
    request: &ExecutionRequest,
    backend: &impl ExecutionBackend,
) -> Result<ExecutionOutcome, ExecutionError> {
    if request.policy().mode() == SandboxMode::Disabled {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            "sandboxed execution does not accept disabled sandbox policies",
        ));
    }

    let support = backend.containment_support(request);
    validate_supported_evidence(request, &support)?;
    let additions = backend.runtime_filesystem_additions(request)?;
    let policy = resolve_policy(request, additions)?;
    policy.validate()?;
    let preflight = ValidatedPreflight { support, policy };
    let outcome = backend.spawn(request, &preflight)?;
    outcome.validate_for_preflight(&preflight)?;
    Ok(outcome)
}

fn validate_supported_evidence(
    request: &ExecutionRequest,
    support: &ContainmentSupport,
) -> Result<(), ExecutionError> {
    let required = EnforcementDimensions::requested_by(request.policy());
    match support {
        ContainmentSupport::Unsupported {
            platform, reason, ..
        } => Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            format!("sandbox containment is unavailable on {platform}: {reason}"),
        )),
        ContainmentSupport::Supported {
            requested,
            declared,
            observed,
            ..
        } => {
            if requested != &required {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::PolicyViolation,
                    "backend requested evidence does not match the execution policy",
                ));
            }
            if !declared.contains(&required) || !observed.contains(&required) {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "requested restrictions lack declared or observed backend support",
                ));
            }
            Ok(())
        }
    }
}

fn resolve_policy(
    request: &ExecutionRequest,
    additions: RuntimeFilesystemAdditions,
) -> Result<ResolvedSandboxPolicy, ExecutionError> {
    let project_root = canonical_path(request.project_root(), "project root")?;
    let filesystem = request.policy().filesystem();
    let mut read = filesystem
        .read()
        .iter()
        .map(|grant| resolve_project_grant(&project_root, grant))
        .collect::<Result<Vec<_>, _>>()?;
    let mut write = filesystem
        .write()
        .iter()
        .map(|grant| resolve_project_grant(&project_root, grant))
        .collect::<Result<Vec<_>, _>>()?;
    read.extend(additions.read);
    write.extend(additions.write);

    let grants = ResolvedFilesystemGrants {
        read: read.iter().map(|grant| grant.path.clone()).collect(),
        write: write.iter().map(|grant| grant.path.clone()).collect(),
    };
    Ok(ResolvedSandboxPolicy {
        mode: request.policy().mode(),
        project_root,
        read,
        write,
        grants,
        limits: request.policy().limits().clone(),
    })
}

fn resolve_project_grant(
    project_root: &Path,
    configured: &str,
) -> Result<ResolvedGrant, ExecutionError> {
    let target = project_root.join(configured);
    let mut ancestor = target.as_path();
    let mut missing = Vec::new();

    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor.file_name().ok_or_else(|| {
                    ExecutionError::new(
                        ExecutionErrorCategory::PolicyViolation,
                        format!("filesystem grant has no existing ancestor: {configured:?}"),
                    )
                })?;
                missing.push(component.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    ExecutionError::new(
                        ExecutionErrorCategory::PolicyViolation,
                        format!("filesystem grant has no existing ancestor: {configured:?}"),
                    )
                })?;
            }
            Err(error) => {
                return Err(path_error("inspect filesystem grant", &target, error));
            }
        }
    }

    let canonical_ancestor = canonical_path(ancestor, "filesystem grant ancestor")?;
    if !canonical_ancestor.starts_with(project_root) {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!("filesystem grant escapes the canonical project root: {configured:?}"),
        ));
    }
    if missing.is_empty() {
        return Ok(ResolvedGrant {
            path: canonical_ancestor,
            resolution: GrantResolution::ExistingCanonical,
        });
    }

    missing.reverse();
    let relative_target = missing.iter().collect::<PathBuf>();
    let path = canonical_ancestor.join(&relative_target);
    Ok(ResolvedGrant {
        path,
        resolution: GrantResolution::MissingTarget {
            canonical_ancestor,
            relative_target,
        },
    })
}

fn resolve_runtime_grant(path: PathBuf) -> Result<ResolvedGrant, ExecutionError> {
    if !path.is_absolute() {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!(
                "runtime filesystem grant must be absolute: {}",
                path.display()
            ),
        ));
    }
    let canonical = canonical_path(&path, "runtime filesystem grant")?;
    if canonical != path {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!(
                "runtime filesystem grant must already be canonical: {}",
                path.display()
            ),
        ));
    }
    Ok(ResolvedGrant {
        path: canonical,
        resolution: GrantResolution::RuntimeCanonical,
    })
}

fn canonical_path(path: &Path, kind: &str) -> Result<PathBuf, ExecutionError> {
    fs::canonicalize(path).map_err(|error| path_error(kind, path, error))
}

fn path_error(kind: &str, path: &Path, error: std::io::Error) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::PolicyViolation,
        format!("cannot resolve {kind} {}: {error}", path.display()),
    )
}

mod platform_backend {
    use super::{
        BackendIdentity, ContainmentSupport, EnforcementDimensions, ExecutionBackend,
        ExecutionError, ExecutionErrorCategory, ExecutionOutcome, ExecutionRequest,
        ValidatedPreflight,
    };

    pub(super) struct PlatformBackend;

    impl ExecutionBackend for PlatformBackend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            containment_support(request)
        }

        fn spawn(
            &self,
            _request: &ExecutionRequest,
            _preflight: &ValidatedPreflight,
        ) -> Result<ExecutionOutcome, ExecutionError> {
            Err(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "containment backend reported support but execution is not implemented",
            ))
        }
    }

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
            declared: EnforcementDimensions::none(),
            observed: EnforcementDimensions::none(),
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
    fn unsupported_preflight_makes_zero_spawn_attempts() {
        struct CountingBackend {
            spawn_attempts: std::cell::Cell<usize>,
        }

        impl ExecutionBackend for CountingBackend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                ContainmentSupport::Unsupported {
                    backend: BackendIdentity {
                        name: "test/unsupported".into(),
                        version: "1".into(),
                        deprecation: None,
                    },
                    platform: "test".into(),
                    reason: "deliberately unavailable".into(),
                    requested: EnforcementDimensions::requested_by(request.policy()),
                    declared: EnforcementDimensions::none(),
                    observed: EnforcementDimensions::none(),
                }
            }

            fn spawn(
                &self,
                _request: &ExecutionRequest,
                _preflight: &ValidatedPreflight,
            ) -> Result<ExecutionOutcome, ExecutionError> {
                self.spawn_attempts.set(self.spawn_attempts.get() + 1);
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    "must not be reached",
                ))
            }
        }

        let backend = CountingBackend {
            spawn_attempts: std::cell::Cell::new(0),
        };
        let request = ExecutionRequest::builder("node")
            .policy(required_policy())
            .build()
            .unwrap();
        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert_eq!(backend.spawn_attempts.get(), 0);
    }

    #[test]
    fn malformed_supported_preflight_makes_zero_spawn_attempts() {
        struct CountingBackend {
            support: ContainmentSupport,
            spawn_attempts: std::cell::Cell<usize>,
        }

        impl ExecutionBackend for CountingBackend {
            fn containment_support(&self, _request: &ExecutionRequest) -> ContainmentSupport {
                self.support.clone()
            }

            fn spawn(
                &self,
                _request: &ExecutionRequest,
                _preflight: &ValidatedPreflight,
            ) -> Result<ExecutionOutcome, ExecutionError> {
                self.spawn_attempts.set(self.spawn_attempts.get() + 1);
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    "must not be reached",
                ))
            }
        }

        let request = ExecutionRequest::builder("node")
            .policy(required_policy())
            .build()
            .unwrap();
        let required = EnforcementDimensions::requested_by(request.policy());
        let mut requested_mismatch = required.clone();
        requested_mismatch.network = false;
        let mut missing_declared = required.clone();
        missing_declared.filesystem_write = false;
        let mut missing_observed = required.clone();
        missing_observed.descendant_lifecycle = false;

        for support in [
            support_with_evidence(requested_mismatch, required.clone(), required.clone()),
            support_with_evidence(required.clone(), missing_declared, required.clone()),
            support_with_evidence(required.clone(), required.clone(), missing_observed),
        ] {
            let backend = CountingBackend {
                support,
                spawn_attempts: std::cell::Cell::new(0),
            };
            let error = execute_with_backend(&request, &backend).unwrap_err();
            assert!(
                matches!(
                    error.category(),
                    ExecutionErrorCategory::PolicyViolation
                        | ExecutionErrorCategory::UnsupportedContainment
                ),
                "{error}"
            );
            assert_eq!(backend.spawn_attempts.get(), 0);
        }
    }

    #[test]
    fn disabled_mode_makes_zero_spawn_attempts() {
        struct CountingBackend {
            spawn_attempts: std::cell::Cell<usize>,
        }

        impl ExecutionBackend for CountingBackend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }

            fn spawn(
                &self,
                _request: &ExecutionRequest,
                _preflight: &ValidatedPreflight,
            ) -> Result<ExecutionOutcome, ExecutionError> {
                self.spawn_attempts.set(self.spawn_attempts.get() + 1);
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    "must not be reached",
                ))
            }
        }

        let disabled = SandboxPolicy::new(
            SandboxMode::Disabled,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let request = ExecutionRequest::builder("node")
            .policy(disabled)
            .build()
            .unwrap();
        let backend = CountingBackend {
            spawn_attempts: std::cell::Cell::new(0),
        };

        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        assert_eq!(backend.spawn_attempts.get(), 0);
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
            let _: &EnforcementDimensions = receipt.declared();
            let _: &EnforcementDimensions = receipt.observed();
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
    fn requested_evidence_distinguishes_subprocess_denial_from_descendant_lifecycle() {
        let denied = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            false,
            ExecutionLimits::default(),
        )
        .unwrap();
        let allowed = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();

        let denied = EnforcementDimensions::requested_by(&denied);
        let allowed = EnforcementDimensions::requested_by(&allowed);
        assert!(denied.subprocess_restriction());
        assert!(!allowed.subprocess_restriction());
        assert!(denied.descendant_lifecycle());
        assert!(allowed.descendant_lifecycle());
    }

    #[test]
    fn requested_resource_evidence_reports_each_configured_limit_independently() {
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(1), None, Some(2), None).unwrap(),
        )
        .unwrap();
        let requested = EnforcementDimensions::requested_by(&policy);
        assert!(requested.timeout());
        assert!(!requested.output());
        assert!(requested.process_count());
        assert!(!requested.memory());
    }

    fn support_with_evidence(
        requested: EnforcementDimensions,
        declared: EnforcementDimensions,
        observed: EnforcementDimensions,
    ) -> ContainmentSupport {
        ContainmentSupport::Supported {
            backend: BackendIdentity {
                name: "test".into(),
                version: "1".into(),
                deprecation: None,
            },
            requested,
            declared,
            observed,
        }
    }

    fn preflight_for(policy: SandboxPolicy, support: ContainmentSupport) -> ValidatedPreflight {
        let request = ExecutionRequest::builder("node")
            .policy(policy)
            .build()
            .unwrap();
        ValidatedPreflight {
            support,
            policy: resolve_policy(&request, RuntimeFilesystemAdditions::default()).unwrap(),
        }
    }

    fn temporary_directory(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "tapid-runner-{prefix}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn resolved_preflight_is_the_only_source_of_receipt_filesystem_grants() {
        struct Backend {
            runtime: PathBuf,
        }

        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }

            fn runtime_filesystem_additions(
                &self,
                _request: &ExecutionRequest,
            ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
                RuntimeFilesystemAdditions::checked(vec![self.runtime.clone()], vec![])
            }

            fn spawn(
                &self,
                _request: &ExecutionRequest,
                preflight: &ValidatedPreflight,
            ) -> Result<ExecutionOutcome, ExecutionError> {
                assert_eq!(preflight.policy.read.len(), 2);
                assert_eq!(preflight.policy.write.len(), 1);
                assert!(matches!(
                    preflight.policy.write[0].resolution,
                    GrantResolution::MissingTarget { .. }
                ));
                let enforced = preflight.support.requested().clone();
                let receipt = EnforcementReceipt::checked(preflight, enforced)?;
                ExecutionOutcome::checked(Termination::Exited(0), vec![], vec![], receipt)
            }
        }

        let root = temporary_directory("resolved-root");
        let runtime = temporary_directory("resolved-runtime");
        fs::create_dir(root.join("existing")).unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        let canonical_runtime = fs::canonicalize(&runtime).unwrap();
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(
                vec!["existing".into()],
                vec!["generated/nested/output.txt".into()],
            )
            .unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let request = ExecutionRequest::builder("node")
            .project_root(&root)
            .policy(policy)
            .build()
            .unwrap();

        let outcome = execute_with_backend(
            &request,
            &Backend {
                runtime: canonical_runtime.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            outcome.enforcement().resolved_filesystem().read(),
            &[canonical_root.join("existing"), canonical_runtime]
        );
        assert_eq!(
            outcome.enforcement().resolved_filesystem().write(),
            &[canonical_root.join("generated/nested/output.txt")]
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(runtime).unwrap();
    }

    #[test]
    fn resolved_policy_rejects_omitted_or_extra_receipt_grants() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let mut preflight = preflight_for(policy, support);

        preflight.policy.grants.read.clear();
        assert!(preflight.policy.validate().is_err());
        preflight.policy.grants.read.push(PathBuf::from("/extra"));
        assert!(preflight.policy.validate().is_err());
    }

    #[test]
    fn noncanonical_runtime_additions_fail_before_spawn() {
        assert!(
            RuntimeFilesystemAdditions::checked(vec![PathBuf::from("relative")], vec![]).is_err()
        );

        let root = temporary_directory("runtime-canonical");
        let canonical = fs::canonicalize(&root).unwrap();
        let noncanonical = canonical.join("..").join(canonical.file_name().unwrap());
        assert!(RuntimeFilesystemAdditions::checked(vec![noncanonical], vec![]).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_in_project_grant_fails_before_spawn() {
        use std::os::unix::fs::symlink;

        struct Backend {
            spawn_attempts: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn spawn(
                &self,
                _request: &ExecutionRequest,
                _preflight: &ValidatedPreflight,
            ) -> Result<ExecutionOutcome, ExecutionError> {
                self.spawn_attempts.set(self.spawn_attempts.get() + 1);
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    "must not be reached",
                ))
            }
        }

        let root = temporary_directory("symlink-root");
        let outside = temporary_directory("symlink-outside");
        symlink(&outside, root.join("escape")).unwrap();
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec!["escape".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let request = ExecutionRequest::builder("node")
            .project_root(&root)
            .policy(policy)
            .build()
            .unwrap();
        let backend = Backend {
            spawn_attempts: std::cell::Cell::new(0),
        };

        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        assert_eq!(backend.spawn_attempts.get(), 0);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn support_models_declared_and_observed_evidence_separately() {
        let requested = EnforcementDimensions::requested_by(&required_policy());
        let mut declared = requested.clone();
        declared.timeout = true;
        let observed = requested.clone();
        let support = support_with_evidence(requested, declared.clone(), observed.clone());

        assert_eq!(support.declared(), &declared);
        assert_eq!(support.observed(), &observed);
        assert_eq!(support.enforceable(), &declared);
    }

    #[test]
    fn checked_receipt_rejects_extra_or_missing_enforcement_dimensions() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);

        let mut extra = requested.clone();
        extra.timeout = true;
        assert!(EnforcementReceipt::checked(&preflight, extra).is_err());

        let mut missing = requested.clone();
        missing.network = false;
        assert!(EnforcementReceipt::checked(&preflight, missing).is_err());
    }

    #[test]
    fn checked_receipt_requires_declared_and_observed_evidence() {
        let limits = ExecutionLimits::new(Some(1), Some(2), Some(3), Some(4)).unwrap();
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            limits.clone(),
        )
        .unwrap();
        let requested = EnforcementDimensions::requested_by(&policy);

        let mut not_observed = requested.clone();
        not_observed.memory = false;
        let support = support_with_evidence(requested.clone(), requested.clone(), not_observed);
        let preflight = preflight_for(policy, support);
        assert!(EnforcementReceipt::checked(&preflight, requested).is_err());
    }

    #[test]
    fn checked_receipt_rejects_disabled_sandbox_policy() {
        let policy = SandboxPolicy::new(
            SandboxMode::Disabled,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let none = EnforcementDimensions::none();
        let support = support_with_evidence(none.clone(), none.clone(), none.clone());
        let preflight = preflight_for(policy, support);

        assert!(EnforcementReceipt::checked(&preflight, none).is_err());
    }

    #[test]
    fn checked_outcome_accepts_success_only_with_a_complete_receipt() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let receipt = EnforcementReceipt::checked(&preflight, requested).unwrap();
        let outcome =
            ExecutionOutcome::checked(Termination::Exited(0), vec![], vec![], receipt).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
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
