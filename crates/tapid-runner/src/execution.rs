use crate::config::{
    AssuranceLevel, ExecutionLimits, SandboxMode, SandboxPolicy, validate_environment_name,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Maximum UTF-8 byte length of each backend identity field.
pub const MAX_BACKEND_IDENTITY_BYTES: usize = 255;
/// Maximum program length in bytes on Unix or UTF-16 code units on Windows.
pub const MAX_PROGRAM_UNITS: usize = 4_096;
/// Maximum number of arguments, excluding the program.
pub const MAX_ARGUMENT_COUNT: usize = 4_096;
/// Maximum argument length in bytes on Unix or UTF-16 code units on Windows.
pub const MAX_ARGUMENT_UNITS: usize = 16_384;
/// Maximum cumulative argv payload. On Windows this bounds a conservative upper estimate of the
/// serialized `CreateProcessW` command line, including quoting, separators, and its terminating NUL.
pub const MAX_ARGV_UNITS: usize = 32_767;
/// Maximum environment value length in bytes on Unix or UTF-16 code units on Windows.
pub const MAX_ENVIRONMENT_VALUE_UNITS: usize = 32_767;
/// Maximum cumulative child environment block, including separators and terminators.
pub const MAX_ENVIRONMENT_BLOCK_UNITS: usize = 32_767;
/// Maximum project-root length in bytes on Unix or UTF-16 code units on Windows.
pub const MAX_PROJECT_ROOT_UNITS: usize = 32_767;
/// Maximum number of explicit executable search directories.
pub const MAX_EXECUTABLE_SEARCH_PATH_COUNT: usize = 256;
/// Maximum length of one executable search directory in native platform units.
pub const MAX_EXECUTABLE_SEARCH_PATH_UNITS: usize = 4_096;
/// Maximum serialized executable search path, including separators and its terminating NUL.
pub const MAX_EXECUTABLE_SEARCH_PATHS_UNITS: usize = 32_767;

/// Identity and lifecycle status of a containment backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendIdentity {
    name: String,
    version: String,
    deprecation: Option<String>,
}

impl BackendIdentity {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        deprecation: Option<String>,
    ) -> Result<Self, ExecutionError> {
        let name = name.into();
        let version = version.into();
        validate_identity_field("backend name", &name)?;
        validate_identity_field("backend version", &version)?;
        if let Some(message) = &deprecation {
            validate_identity_field("backend deprecation", message)?;
        }
        Ok(Self {
            name,
            version,
            deprecation,
        })
    }

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

fn validate_identity_field(kind: &str, value: &str) -> Result<(), ExecutionError> {
    if value.is_empty()
        || value.len() > MAX_BACKEND_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::InvalidRequest,
            format!(
                "{kind} must be non-empty, at most {MAX_BACKEND_IDENTITY_BYTES} UTF-8 bytes, and contain no control characters"
            ),
        ));
    }
    Ok(())
}

/// Independently reportable containment dimensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementDimensions {
    filesystem_read: bool,
    filesystem_write: bool,
    network: bool,
    environment_sanitization: bool,
    descriptor_hygiene: bool,
    subprocess_restriction: bool,
    descendant_authority_propagation: bool,
    descendant_lifecycle: bool,
    process_tree_membership: bool,
    complete_cleanup: bool,
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
            descriptor_hygiene: false,
            subprocess_restriction: false,
            descendant_authority_propagation: false,
            descendant_lifecycle: false,
            process_tree_membership: false,
            complete_cleanup: false,
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
    pub fn descriptor_hygiene(&self) -> bool {
        self.descriptor_hygiene
    }
    pub fn subprocess_restriction(&self) -> bool {
        self.subprocess_restriction
    }
    pub fn descendant_authority_propagation(&self) -> bool {
        self.descendant_authority_propagation
    }
    pub fn descendant_lifecycle(&self) -> bool {
        self.descendant_lifecycle
    }
    pub fn process_tree_membership(&self) -> bool {
        self.process_tree_membership
    }
    pub fn complete_cleanup(&self) -> bool {
        self.complete_cleanup
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
        let managed_tree = policy.assurance() == AssuranceLevel::ManagedTree;
        Self {
            filesystem_read: true,
            filesystem_write: true,
            network: true,
            environment_sanitization: true,
            descriptor_hygiene: true,
            subprocess_restriction: !policy.subprocess(),
            descendant_authority_propagation: true,
            descendant_lifecycle: managed_tree,
            process_tree_membership: managed_tree,
            complete_cleanup: managed_tree,
            timeout: limits.timeout_seconds().is_some(),
            output: limits.max_output_bytes().is_some(),
            process_count: limits.max_processes().is_some(),
            memory: limits.max_memory_bytes().is_some(),
        }
    }

    fn completion_required(requested: &Self) -> Self {
        Self {
            descendant_lifecycle: requested.descendant_lifecycle,
            process_tree_membership: requested.process_tree_membership,
            complete_cleanup: requested.complete_cleanup,
            ..Self::none()
        }
    }

    #[allow(dead_code)] // Used by checked construction when a platform backend lands.
    fn contains(&self, required: &Self) -> bool {
        (!required.filesystem_read || self.filesystem_read)
            && (!required.filesystem_write || self.filesystem_write)
            && (!required.network || self.network)
            && (!required.environment_sanitization || self.environment_sanitization)
            && (!required.descriptor_hygiene || self.descriptor_hygiene)
            && (!required.subprocess_restriction || self.subprocess_restriction)
            && (!required.descendant_authority_propagation || self.descendant_authority_propagation)
            && (!required.descendant_lifecycle || self.descendant_lifecycle)
            && (!required.process_tree_membership || self.process_tree_membership)
            && (!required.complete_cleanup || self.complete_cleanup)
            && (!required.timeout || self.timeout)
            && (!required.output || self.output)
            && (!required.process_count || self.process_count)
            && (!required.memory || self.memory)
    }
}

/// One independently reportable portable enforcement dimension.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum EnforcementDimension {
    FilesystemRead,
    FilesystemWrite,
    Network,
    EnvironmentSanitization,
    DescriptorHygiene,
    SubprocessRestriction,
    DescendantAuthorityPropagation,
    DescendantLifecycle,
    ProcessTreeMembership,
    CompleteCleanup,
    Timeout,
    Output,
    ProcessCount,
    Memory,
}

/// The processes to which one dimension applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EnforcementScope {
    LaunchProcess,
    DescendantTree,
    ManagedTree,
}

/// Backend-specific evidence for one dimension. Its private fields prevent forgery by callers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DimensionEvidence {
    dimension: EnforcementDimension,
    scope: EnforcementScope,
    mechanism: String,
    limitations: Vec<String>,
}

impl DimensionEvidence {
    fn new(
        dimension: EnforcementDimension,
        scope: EnforcementScope,
        mechanism: impl Into<String>,
        limitations: Vec<String>,
    ) -> Self {
        Self {
            dimension,
            scope,
            mechanism: mechanism.into(),
            limitations,
        }
    }

    pub fn dimension(&self) -> EnforcementDimension {
        self.dimension
    }
    pub fn scope(&self) -> EnforcementScope {
        self.scope
    }
    pub fn mechanism(&self) -> &str {
        &self.mechanism
    }
    pub fn limitations(&self) -> &[String] {
        &self.limitations
    }
}

fn evidence_for_dimensions(
    dimensions: &EnforcementDimensions,
    mechanism: &str,
    limitations: &[&str],
) -> Vec<DimensionEvidence> {
    let managed = dimensions.process_tree_membership;
    [
        (
            EnforcementDimension::FilesystemRead,
            dimensions.filesystem_read,
        ),
        (
            EnforcementDimension::FilesystemWrite,
            dimensions.filesystem_write,
        ),
        (EnforcementDimension::Network, dimensions.network),
        (
            EnforcementDimension::EnvironmentSanitization,
            dimensions.environment_sanitization,
        ),
        (
            EnforcementDimension::DescriptorHygiene,
            dimensions.descriptor_hygiene,
        ),
        (
            EnforcementDimension::SubprocessRestriction,
            dimensions.subprocess_restriction,
        ),
        (
            EnforcementDimension::DescendantAuthorityPropagation,
            dimensions.descendant_authority_propagation,
        ),
        (
            EnforcementDimension::DescendantLifecycle,
            dimensions.descendant_lifecycle,
        ),
        (
            EnforcementDimension::ProcessTreeMembership,
            dimensions.process_tree_membership,
        ),
        (
            EnforcementDimension::CompleteCleanup,
            dimensions.complete_cleanup,
        ),
        (EnforcementDimension::Timeout, dimensions.timeout),
        (EnforcementDimension::Output, dimensions.output),
        (EnforcementDimension::ProcessCount, dimensions.process_count),
        (EnforcementDimension::Memory, dimensions.memory),
    ]
    .into_iter()
    .filter(|(_, enabled)| *enabled)
    .map(|(dimension, _)| {
        let scope = match dimension {
            EnforcementDimension::EnvironmentSanitization
            | EnforcementDimension::DescriptorHygiene => EnforcementScope::LaunchProcess,
            EnforcementDimension::DescendantLifecycle
            | EnforcementDimension::ProcessTreeMembership
            | EnforcementDimension::CompleteCleanup => EnforcementScope::ManagedTree,
            EnforcementDimension::Timeout
            | EnforcementDimension::Output
            | EnforcementDimension::ProcessCount
            | EnforcementDimension::Memory
                if managed =>
            {
                EnforcementScope::ManagedTree
            }
            _ => EnforcementScope::DescendantTree,
        };
        DimensionEvidence::new(
            dimension,
            scope,
            mechanism,
            limitations
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        )
    })
    .collect()
}

fn validate_dimension_evidence(
    dimensions: &EnforcementDimensions,
    evidence: &[DimensionEvidence],
) -> Result<(), ExecutionError> {
    let expected = evidence_for_dimensions(dimensions, "expected", &[]);
    if evidence.len() != expected.len()
        || expected.iter().any(|required| {
            evidence
                .iter()
                .filter(|candidate| candidate.dimension == required.dimension)
                .count()
                != 1
                || !evidence.iter().any(|candidate| {
                    candidate.dimension == required.dimension
                        && candidate.scope == required.scope
                        && !candidate.mechanism.is_empty()
                        && candidate.mechanism.len() <= MAX_BACKEND_IDENTITY_BYTES
                        && !candidate.mechanism.chars().any(char::is_control)
                        && candidate.limitations.iter().all(|limitation| {
                            !limitation.is_empty()
                                && limitation.len() <= MAX_BACKEND_IDENTITY_BYTES
                                && !limitation.chars().any(char::is_control)
                        })
                })
        })
    {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            "backend dimension metadata is incomplete, duplicated, invalid, or has the wrong scope",
        ));
    }
    Ok(())
}

/// Access associated with one effective filesystem grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemAccess {
    Read,
    Write,
}

/// The policy semantics declared for a target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemGrantKind {
    ExactFile,
    DirectorySubtree,
}

/// Where an effective grant originated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemGrantSource {
    ProjectPolicy,
    BackendRuntime,
}

/// Evidence used to bind a grant immediately before native setup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemBindingMode {
    /// A held native handle and stable object identity back this grant.
    NativeObject,
    /// A freshly re-resolved canonical path backs this grant. This mode assumes a trusted host
    /// does not concurrently replace path components before native sandbox installation.
    CanonicalPath,
}

/// One typed, effective filesystem grant reported by a receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFilesystemGrant {
    path: PathBuf,
    access: FilesystemAccess,
    kind: FilesystemGrantKind,
    source: FilesystemGrantSource,
    binding: FilesystemBindingMode,
}

impl ResolvedFilesystemGrant {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn access(&self) -> FilesystemAccess {
        self.access
    }
    pub fn kind(&self) -> FilesystemGrantKind {
        self.kind
    }
    pub fn source(&self) -> FilesystemGrantSource {
        self.source
    }
    pub fn binding(&self) -> FilesystemBindingMode {
        self.binding
    }
}

/// Every effective project and backend runtime grant, without collapsing origins or kinds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFilesystemGrants {
    grants: Vec<ResolvedFilesystemGrant>,
}

impl ResolvedFilesystemGrants {
    pub fn grants(&self) -> &[ResolvedFilesystemGrant] {
        &self.grants
    }
    pub fn read(&self) -> impl Iterator<Item = &ResolvedFilesystemGrant> {
        self.grants
            .iter()
            .filter(|grant| grant.access == FilesystemAccess::Read)
    }
    pub fn write(&self) -> impl Iterator<Item = &ResolvedFilesystemGrant> {
        self.grants
            .iter()
            .filter(|grant| grant.access == FilesystemAccess::Write)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum GrantResolution {
    ExistingCanonical,
    MissingWriteDirectory {
        canonical_ancestor: PathBuf,
        relative_target: PathBuf,
    },
    RuntimeCanonical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedGrant {
    path: PathBuf,
    access: FilesystemAccess,
    kind: FilesystemGrantKind,
    source: FilesystemGrantSource,
    resolution: GrantResolution,
}

/// Explicit, typed backend baseline/runtime paths added to project policy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RuntimeFilesystemAdditions {
    read: Vec<ResolvedGrant>,
    write: Vec<ResolvedGrant>,
}

impl RuntimeFilesystemAdditions {
    #[allow(dead_code)]
    fn checked(read: Vec<PathBuf>, write: Vec<PathBuf>) -> Result<Self, ExecutionError> {
        Ok(Self {
            read: read
                .into_iter()
                .map(|path| resolve_runtime_grant(path, FilesystemAccess::Read))
                .collect::<Result<_, _>>()?,
            write: write
                .into_iter()
                .map(|path| resolve_runtime_grant(path, FilesystemAccess::Write))
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
    limits: ExecutionLimits,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeIdentity {
    device: u64,
    inode: u64,
}

struct BoundFilesystemGrant {
    receipt: ResolvedFilesystemGrant,
    held: Option<fs::File>,
    #[cfg(unix)]
    native_identity: Option<NativeIdentity>,
}

struct FilesystemBindings {
    grants: Vec<BoundFilesystemGrant>,
}

impl FilesystemBindings {
    fn canonical_path(policy: &ResolvedSandboxPolicy) -> Result<Self, ExecutionError> {
        let grants = policy
            .read
            .iter()
            .chain(&policy.write)
            .map(bind_canonical)
            .collect::<Result<_, _>>()?;
        Ok(Self { grants })
    }

    #[cfg(unix)]
    #[allow(dead_code)]
    fn native_objects(policy: &ResolvedSandboxPolicy) -> Result<Self, ExecutionError> {
        use std::os::unix::fs::MetadataExt;
        let grants = policy
            .read
            .iter()
            .chain(&policy.write)
            .map(|grant| {
                let canonical = canonical_path(&grant.path, "native filesystem binding")?;
                if canonical != grant.path || filesystem_kind(&canonical)? != grant.kind {
                    return Err(binding_mismatch());
                }
                let held = fs::File::open(&canonical).map_err(|error| {
                    path_error("open native filesystem binding", &canonical, error)
                })?;
                let metadata = held.metadata().map_err(|error| {
                    path_error("inspect native filesystem binding", &canonical, error)
                })?;
                Ok(BoundFilesystemGrant {
                    receipt: grant_receipt(grant, FilesystemBindingMode::NativeObject),
                    held: Some(held),
                    native_identity: Some(NativeIdentity {
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    }),
                })
            })
            .collect::<Result<_, ExecutionError>>()?;
        Ok(Self { grants })
    }

    fn validate(&self, policy: &ResolvedSandboxPolicy) -> Result<(), ExecutionError> {
        let expected: Vec<_> = policy.read.iter().chain(&policy.write).collect();
        if expected.len() != self.grants.len() {
            return Err(binding_mismatch());
        }
        for (expected, bound) in expected.into_iter().zip(&self.grants) {
            if bound.receipt.path != expected.path
                || bound.receipt.access != expected.access
                || bound.receipt.kind != expected.kind
                || bound.receipt.source != expected.source
            {
                return Err(binding_mismatch());
            }
            let canonical = canonical_path(&expected.path, "filesystem binding revalidation")?;
            if canonical != expected.path || filesystem_kind(&canonical)? != expected.kind {
                return Err(binding_mismatch());
            }
            match bound.receipt.binding {
                FilesystemBindingMode::CanonicalPath => {
                    if bound.held.is_some() {
                        return Err(binding_mismatch());
                    }
                }
                FilesystemBindingMode::NativeObject => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        let held = bound.held.as_ref().ok_or_else(binding_mismatch)?;
                        let held_metadata = held.metadata().map_err(|error| {
                            path_error(
                                "inspect held native filesystem binding",
                                &expected.path,
                                error,
                            )
                        })?;
                        let current = fs::metadata(&canonical).map_err(|error| {
                            path_error(
                                "inspect current native filesystem binding",
                                &canonical,
                                error,
                            )
                        })?;
                        let identity = NativeIdentity {
                            device: held_metadata.dev(),
                            inode: held_metadata.ino(),
                        };
                        let current_identity = NativeIdentity {
                            device: current.dev(),
                            inode: current.ino(),
                        };
                        if Some(identity) != bound.native_identity || identity != current_identity {
                            return Err(binding_mismatch());
                        }
                    }
                    #[cfg(not(unix))]
                    return Err(ExecutionError::new(
                        ExecutionErrorCategory::UnsupportedContainment,
                        "native object identity is unavailable on this adapter scaffold",
                    ));
                }
            }
        }
        Ok(())
    }

    fn receipt(&self) -> ResolvedFilesystemGrants {
        ResolvedFilesystemGrants {
            grants: self
                .grants
                .iter()
                .map(|grant| grant.receipt.clone())
                .collect(),
        }
    }
}

fn bind_canonical(grant: &ResolvedGrant) -> Result<BoundFilesystemGrant, ExecutionError> {
    let canonical =
        canonical_path(&grant.path, "canonical filesystem binding").map_err(|error| {
            if matches!(
                grant.resolution,
                GrantResolution::MissingWriteDirectory { .. }
            ) {
                ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    format!("backend did not securely materialize missing write subtree: {error}"),
                )
            } else {
                error
            }
        })?;
    if canonical != grant.path || filesystem_kind(&canonical)? != grant.kind {
        return Err(binding_mismatch());
    }
    Ok(BoundFilesystemGrant {
        receipt: grant_receipt(grant, FilesystemBindingMode::CanonicalPath),
        held: None,
        #[cfg(unix)]
        native_identity: None,
    })
}

fn grant_receipt(grant: &ResolvedGrant, binding: FilesystemBindingMode) -> ResolvedFilesystemGrant {
    ResolvedFilesystemGrant {
        path: grant.path.clone(),
        access: grant.access,
        kind: grant.kind,
        source: grant.source,
        binding,
    }
}

fn binding_mismatch() -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::PolicyViolation,
        "backend filesystem bindings do not match the exact validated preflight",
    )
}

/// Private proof that support, policy, bindings, and limits passed pre-spawn checks.
struct ValidatedPreflight {
    support: ContainmentSupport,
    policy: ResolvedSandboxPolicy,
    bindings: FilesystemBindings,
    child_environment: BTreeMap<OsString, OsString>,
}

impl ResolvedSandboxPolicy {
    fn validate(&self) -> Result<(), ExecutionError> {
        for grant in self.read.iter().chain(&self.write) {
            match &grant.resolution {
                GrantResolution::ExistingCanonical => {
                    validate_canonical_path(&grant.path)?;
                    if !grant.path.starts_with(&self.project_root) {
                        return Err(grant_confinement_error(&grant.path));
                    }
                }
                GrantResolution::MissingWriteDirectory {
                    canonical_ancestor,
                    relative_target,
                } => {
                    validate_canonical_path(canonical_ancestor)?;
                    if grant.access != FilesystemAccess::Write
                        || grant.kind != FilesystemGrantKind::DirectorySubtree
                        || !canonical_ancestor.starts_with(&self.project_root)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SupportAvailability {
    Supported,
    Unsupported,
}

/// The platform backend's dimensioned ability to enforce the requested containment policy.
///
/// Callers can inspect this report but cannot manufacture one.
///
/// ```
/// use tapid_runner::ContainmentSupport;
/// fn inspect(report: &ContainmentSupport) {
///     let _: bool = report.is_supported();
///     let _: Option<&str> = report.platform();
/// }
/// ```
///
/// ```compile_fail
/// use tapid_runner::{BackendIdentity, ContainmentSupport, EnforcementDimensions};
/// let none = EnforcementDimensions::none();
/// let _ = ContainmentSupport::Supported {
///     backend: BackendIdentity::new("fake", "1", None).unwrap(),
///     requested: none.clone(), declared: none.clone(), observed: none,
///     declared_evidence: vec![], observed_evidence: vec![],
/// };
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainmentSupport {
    availability: SupportAvailability,
    backend: BackendIdentity,
    platform: Option<String>,
    reason: Option<String>,
    requested: EnforcementDimensions,
    declared: EnforcementDimensions,
    observed: EnforcementDimensions,
    declared_evidence: Vec<DimensionEvidence>,
    observed_evidence: Vec<DimensionEvidence>,
}

impl ContainmentSupport {
    #[allow(dead_code)] // Reserved for a platform backend; tests exercise the contract now.
    fn supported(
        backend: BackendIdentity,
        requested: EnforcementDimensions,
        declared: EnforcementDimensions,
        observed: EnforcementDimensions,
        declared_evidence: Vec<DimensionEvidence>,
        observed_evidence: Vec<DimensionEvidence>,
    ) -> Self {
        Self {
            availability: SupportAvailability::Supported,
            backend,
            platform: Some(std::env::consts::OS.to_owned()),
            reason: None,
            requested,
            declared,
            observed,
            declared_evidence,
            observed_evidence,
        }
    }

    fn unsupported(
        backend: BackendIdentity,
        platform: impl Into<String>,
        reason: impl Into<String>,
        requested: EnforcementDimensions,
        declared: EnforcementDimensions,
        observed: EnforcementDimensions,
    ) -> Self {
        Self {
            availability: SupportAvailability::Unsupported,
            backend,
            platform: Some(platform.into()),
            reason: Some(reason.into()),
            requested,
            declared,
            observed,
            declared_evidence: Vec::new(),
            observed_evidence: Vec::new(),
        }
    }

    pub fn is_supported(&self) -> bool {
        self.availability == SupportAvailability::Supported
    }

    /// Platform named by the backend report. The report remains non-forgeable because all fields
    /// and constructors are private while these accessors make its claims inspectable.
    pub fn platform(&self) -> Option<&str> {
        self.platform.as_deref()
    }

    pub fn backend(&self) -> &BackendIdentity {
        &self.backend
    }

    pub fn requested(&self) -> &EnforcementDimensions {
        &self.requested
    }

    /// Compatibility summary. Prefer [`Self::declared_evidence`].
    pub fn declared(&self) -> &EnforcementDimensions {
        &self.declared
    }

    /// Compatibility summary. Prefer [`Self::observed_evidence`].
    pub fn observed(&self) -> &EnforcementDimensions {
        &self.observed
    }

    pub fn declared_evidence(&self) -> &[DimensionEvidence] {
        &self.declared_evidence
    }

    pub fn observed_evidence(&self) -> &[DimensionEvidence] {
        &self.observed_evidence
    }

    /// Compatibility alias for [`Self::declared`].
    pub fn enforceable(&self) -> &EnforcementDimensions {
        self.declared()
    }

    pub fn unsupported_reason(&self) -> Option<&str> {
        self.reason.as_deref()
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
    established_evidence: Vec<DimensionEvidence>,
    resolved_filesystem: ResolvedFilesystemGrants,
    configured_limits: ExecutionLimits,
}

impl EnforcementReceipt {
    #[allow(dead_code)] // The no-backend scaffold cannot produce receipts yet.
    fn checked(
        preflight: &ValidatedPreflight,
        enforced: EnforcementDimensions,
        established_evidence: Vec<DimensionEvidence>,
    ) -> Result<Self, ExecutionError> {
        let support = &preflight.support;
        if !support.is_supported() {
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
        if &enforced != requested {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "established dimensions do not exactly match the requested restrictions",
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
        validate_dimension_evidence(&enforced, &established_evidence)?;
        Ok(Self {
            support: support.clone(),
            enforced,
            established_evidence,
            resolved_filesystem: preflight.bindings.receipt(),
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

    /// Launch-time evidence that the backend established each reported restriction before spawn.
    pub fn established_evidence(&self) -> &[DimensionEvidence] {
        &self.established_evidence
    }

    pub fn resolved_filesystem(&self) -> &ResolvedFilesystemGrants {
        &self.resolved_filesystem
    }

    pub fn configured_limits(&self) -> &ExecutionLimits {
        &self.configured_limits
    }
}

/// Confidence that process cleanup was observed at execution completion.
///
/// ```compile_fail
/// use tapid_runner::CleanupConfidence;
/// fn exhaustive(value: CleanupConfidence) {
///     match value {
///         CleanupConfidence::NotGuaranteed => {}
///         CleanupConfidence::BestEffortObserved => {}
///         CleanupConfidence::KernelOwnedComplete => {}
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CleanupConfidence {
    /// Cleanup is outside the portable restricted-authority contract.
    NotGuaranteed,
    /// Cleanup was attempted and observed without complete descendant ownership.
    BestEffortObserved,
    /// A kernel or VM owned the complete process-tree cleanup boundary.
    KernelOwnedComplete,
}

/// Completion-time evidence, distinct from launch-time enforcement evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionEvidence {
    confirmed: EnforcementDimensions,
    evidence: Vec<DimensionEvidence>,
    cleanup_confidence: CleanupConfidence,
}

impl CompletionEvidence {
    #[allow(dead_code)] // The no-backend scaffold cannot produce completion evidence yet.
    fn checked(
        preflight: &ValidatedPreflight,
        confirmed: EnforcementDimensions,
        evidence: Vec<DimensionEvidence>,
        cleanup_confidence: CleanupConfidence,
    ) -> Result<Self, ExecutionError> {
        let required = EnforcementDimensions::completion_required(preflight.support.requested());
        if confirmed != required {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "completion evidence does not exactly match completion-observable dimensions",
            ));
        }
        validate_dimension_evidence(&confirmed, &evidence)?;
        Ok(Self {
            evidence,
            confirmed,
            cleanup_confidence,
        })
    }

    pub fn confirmed(&self) -> &EnforcementDimensions {
        &self.confirmed
    }

    pub fn evidence(&self) -> &[DimensionEvidence] {
        &self.evidence
    }

    pub fn cleanup_confidence(&self) -> CleanupConfidence {
        self.cleanup_confidence
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
/// Post-spawn validation checked-adds the byte lengths of stdout and stderr; their combined size
/// may not exceed the exact preflight `max_output_bytes`. Resource-limit termination variants are
/// accepted only when the corresponding limit was present in that exact preflight.
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
    completion: CompletionEvidence,
}

impl ExecutionOutcome {
    #[allow(dead_code)] // The no-backend scaffold cannot produce outcomes yet.
    fn checked(
        termination: Termination,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        enforcement: EnforcementReceipt,
        completion: CompletionEvidence,
    ) -> Result<Self, ExecutionError> {
        if enforcement.enforced() != enforcement.requested() {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "execution outcome lacks complete enforcement evidence",
            ));
        }
        let completion_required =
            EnforcementDimensions::completion_required(enforcement.requested());
        if completion.confirmed() != &completion_required {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "execution outcome lacks complete completion evidence",
            ));
        }
        if enforcement.requested().process_tree_membership()
            && completion.cleanup_confidence() != CleanupConfidence::KernelOwnedComplete
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "managed-tree execution requires kernel-owned complete cleanup evidence",
            ));
        }
        Ok(Self {
            termination,
            stdout,
            stderr,
            enforcement,
            completion,
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
            || receipt.resolved_filesystem() != &preflight.bindings.receipt()
            || self.completion.confirmed() != &EnforcementDimensions::completion_required(required)
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "execution outcome does not match the exact validated preflight",
            ));
        }
        let captured_bytes = checked_captured_output_bytes(self.stdout.len(), self.stderr.len())?;
        if preflight
            .policy
            .limits
            .max_output_bytes()
            .is_some_and(|limit| {
                u64::try_from(captured_bytes).map_or(true, |captured| captured > limit)
            })
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "captured stdout and stderr exceed the exact preflight output limit",
            ));
        }
        if matches!(self.termination, Termination::TimedOut)
            && preflight.policy.limits.timeout_seconds().is_none()
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "timeout termination requires an exact preflight timeout limit",
            ));
        }
        if matches!(self.termination, Termination::OutputLimitExceeded)
            && preflight.policy.limits.max_output_bytes().is_none()
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "output-limit termination requires an exact preflight output limit",
            ));
        }
        if matches!(self.termination, Termination::ProcessLimitExceeded)
            && preflight.policy.limits.max_processes().is_none()
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "process-limit termination requires an exact preflight process limit",
            ));
        }
        if matches!(self.termination, Termination::MemoryLimitExceeded)
            && preflight.policy.limits.max_memory_bytes().is_none()
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "memory-limit termination requires an exact preflight memory limit",
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
    pub fn completion(&self) -> &CompletionEvidence {
        &self.completion
    }
}

fn checked_captured_output_bytes(stdout: usize, stderr: usize) -> Result<usize, ExecutionError> {
    stdout.checked_add(stderr).ok_or_else(|| {
        ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            "captured stdout and stderr byte count overflowed",
        )
    })
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
    completion: Option<CompletionEvidence>,
}

impl ExecutionError {
    fn new(category: ExecutionErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            completion: None,
        }
    }

    fn with_completion(mut self, completion: CompletionEvidence) -> Self {
        self.completion = Some(completion);
        self
    }

    pub fn category(&self) -> ExecutionErrorCategory {
        self.category
    }

    /// Checked cleanup disposition for a failure after process creation; absent before spawn.
    pub fn completion(&self) -> Option<&CompletionEvidence> {
        self.completion.as_ref()
    }
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ExecutionError {}

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
struct TrustedNodeRuntime {
    path: PathBuf,
    #[cfg(unix)]
    identity: NativeIdentity,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
}

impl TrustedNodeRuntime {
    fn checked(path: &Path) -> Result<Self, ExecutionError> {
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

    fn validate(&self, search_paths: &[PathBuf]) -> Result<(), ExecutionError> {
        if search_paths
            .first()
            .and_then(|path| fs::canonicalize(path).ok())
            .as_deref()
            != self.path.parent()
        {
            return Err(invalid_request(
                "trusted Node runtime directory must be the first executable search path",
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    program: OsString,
    arguments: Vec<OsString>,
    executable_search_paths: Vec<PathBuf>,
    trusted_node_runtime: Option<TrustedNodeRuntime>,
    windows_verbatim_arguments: bool,
    project_root: PathBuf,
    policy: SandboxPolicy,
    environment: BTreeMap<OsString, OsString>,
}

impl ExecutionRequest {
    pub fn builder(program: impl Into<OsString>) -> ExecutionRequestBuilder {
        ExecutionRequestBuilder {
            program: program.into(),
            arguments: Vec::new(),
            executable_search_paths: Vec::new(),
            trusted_node_runtime: None,
            windows_verbatim_arguments: false,
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
    /// Whether a Windows adapter must pass arguments without MSVCRT re-quoting.
    pub fn uses_windows_verbatim_arguments(&self) -> bool {
        self.windows_verbatim_arguments
    }
    /// Ordered executable search directories for private platform adapters.
    fn executable_search_paths(&self) -> &[PathBuf] {
        &self.executable_search_paths
    }
    #[cfg(all(test, unix))]
    fn trusted_node_runtime(&self) -> &Path {
        self.trusted_node_runtime
            .as_ref()
            .expect("trusted Node runtime was requested")
            .path
            .as_path()
    }
    fn validate_trusted_node_runtime(&self) -> Result<(), ExecutionError> {
        if let Some(runtime) = &self.trusted_node_runtime {
            runtime.validate(&self.executable_search_paths)?;
        }
        Ok(())
    }
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
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
    fn child_environment(&self) -> BTreeMap<OsString, OsString> {
        let mut environment = self.environment.clone();
        let path = join_executable_search_paths(&self.executable_search_paths)
            .expect("validated executable search paths must remain joinable");
        environment.insert(OsString::from("PATH"), path);
        environment
    }

    fn validate_windows_verbatim_boundary(&self) -> Result<(), ExecutionError> {
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

    fn validate(&self) -> Result<(), ExecutionError> {
        self.validate_windows_verbatim_boundary()?;
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
        validate_windows_command_line_units(program_units, argument_units.iter().copied())?;
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
        let trusted_node_runtime = self
            .trusted_node_runtime
            .as_deref()
            .map(TrustedNodeRuntime::checked)
            .transpose()?;
        let request = ExecutionRequest {
            program: self.program,
            arguments: self.arguments,
            executable_search_paths: self.executable_search_paths,
            trusted_node_runtime,
            windows_verbatim_arguments: self.windows_verbatim_arguments,
            project_root: self.project_root,
            policy: self.policy,
            environment: self.environment,
        };
        request.validate()?;
        Ok(request)
    }
}

fn validate_executable_search_paths(paths: &[PathBuf]) -> Result<OsString, ExecutionError> {
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

fn join_executable_search_paths(paths: &[PathBuf]) -> Result<OsString, ExecutionError> {
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

/// Returns a conservative upper bound rather than the exact Windows command-line serialization.
/// The program and every argument are assumed to need surrounding quotes, and every input code
/// unit is budgeted to double under worst-case quote/backslash escaping. Separators and the
/// mandatory terminating NUL are counted separately.
#[cfg(any(windows, test))]
fn windows_command_line_units_upper_bound(
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
fn validate_windows_command_line_units(
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

/// Mirrors `std::env::join_paths` on Windows so its separator quoting remains host-testable.
#[cfg(test)]
fn join_windows_path_units<'a>(paths: impl IntoIterator<Item = &'a [u16]>) -> Result<Vec<u16>, ()> {
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
fn windows_path_units_semantically_equal(left: &[u16], right: &[u16]) -> bool {
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
fn os_units(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().len()
}

#[cfg(unix)]
fn os_contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().contains(&0)
}

#[cfg(windows)]
fn os_units(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().count()
}

#[cfg(windows)]
fn os_contains_nul(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().any(|unit| unit == 0)
}

#[cfg(not(any(unix, windows)))]
fn os_units(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(not(any(unix, windows)))]
fn os_contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

/// Attempts execution through a private platform backend.
///
/// Platform backends are intentionally not implemented yet. This function performs
/// containment preflight and fails before spawning any child process.
pub fn execute(request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
    execute_with_backend(request, &platform_backend::PlatformBackend)
}

/// A prepared backend lifecycle. The owner exists before `execute` may create a process.
trait ExecutionLifecycle {
    /// Create the native process and drive it through completion.
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError>;
    fn cleanup(&mut self) -> CompletionEvidence;
}

/// A preparation failure raised before an owned attempt can create a process.
struct PreparationError(ExecutionError);

impl From<ExecutionError> for PreparationError {
    fn from(error: ExecutionError) -> Self {
        Self(error)
    }
}

/// Owns the lifecycle before native process creation. Explicit finish validates every returned
/// disposition and cleans up every execution error. Drop can only attempt fallback cleanup:
/// because it cannot report validation errors, invalid evidence panics instead of being represented
/// as a checked disposition.
struct OwnedExecutionAttempt<'a> {
    preflight: &'a ValidatedPreflight,
    lifecycle: Box<dyn ExecutionLifecycle + 'a>,
    finished: bool,
}

impl<'a> OwnedExecutionAttempt<'a> {
    #[allow(dead_code)] // Reserved for platform backends; test backends exercise owned attempts.
    fn new(preflight: &'a ValidatedPreflight, lifecycle: Box<dyn ExecutionLifecycle + 'a>) -> Self {
        Self {
            preflight,
            lifecycle,
            finished: false,
        }
    }

    fn finish(mut self) -> Result<ExecutionOutcome, ExecutionError> {
        match self.lifecycle.execute() {
            Ok(outcome) => {
                if let Err(error) = outcome.validate_for_preflight(self.preflight) {
                    return Err(self.checked_fallback(error));
                }
                self.finished = true;
                Ok(*outcome)
            }
            Err(error) => Err(self.checked_fallback(error)),
        }
    }

    fn checked_fallback(&mut self, error: ExecutionError) -> ExecutionError {
        // Prevent a validation panic from causing a second cleanup attempt during unwinding.
        self.finished = true;
        let completion = self.lifecycle.cleanup();
        validate_completion_for_preflight(&completion, self.preflight)
            .expect("post-spawn fallback cleanup did not satisfy the checked contract");
        error.with_completion(completion)
    }
}

impl Drop for OwnedExecutionAttempt<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let completion = self.lifecycle.cleanup();
        validate_completion_for_preflight(&completion, self.preflight)
            .expect("dropped post-spawn attempt did not satisfy the checked cleanup contract");
        self.finished = true;
    }
}

fn validate_completion_for_preflight(
    completion: &CompletionEvidence,
    preflight: &ValidatedPreflight,
) -> Result<(), ExecutionError> {
    let required = EnforcementDimensions::completion_required(preflight.support.requested());
    if completion.confirmed() != &required {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            "post-spawn completion does not match completion-observable dimensions",
        ));
    }
    validate_dimension_evidence(&required, completion.evidence())?;
    if required.process_tree_membership()
        && completion.cleanup_confidence() != CleanupConfidence::KernelOwnedComplete
    {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            "managed-tree completion lacks kernel-owned complete cleanup",
        ));
    }
    Ok(())
}

trait ExecutionBackend {
    fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport;

    fn runtime_filesystem_additions(
        &self,
        _request: &ExecutionRequest,
    ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
        Ok(RuntimeFilesystemAdditions::default())
    }

    /// Materialize missing write subtrees, re-resolve every path, and bind native identity
    /// immediately before native sandbox setup. The default path-only implementation fails
    /// closed when a missing write subtree was not materialized.
    fn bind_filesystem(
        &self,
        _request: &ExecutionRequest,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<FilesystemBindings, ExecutionError> {
        FilesystemBindings::canonical_path(policy)
    }

    /// Perform preparation that cannot create a native process, then return its lifecycle owner.
    fn prepare<'a>(
        &'a self,
        request: &ExecutionRequest,
        preflight: &'a ValidatedPreflight,
    ) -> Result<OwnedExecutionAttempt<'a>, PreparationError>;
}

fn execute_with_backend(
    request: &ExecutionRequest,
    backend: &impl ExecutionBackend,
) -> Result<ExecutionOutcome, ExecutionError> {
    request.validate()?;
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
    let bindings = backend.bind_filesystem(request, &policy)?;
    bindings.validate(&policy)?;
    let preflight = ValidatedPreflight {
        support,
        policy,
        bindings,
        child_environment: request.child_environment(),
    };
    // This is deliberately the final generic operation before backend preparation. Native process
    // creation is reachable only through the owned lifecycle returned by `prepare`.
    preflight.bindings.validate(&preflight.policy)?;
    if !preflight.child_environment.contains_key(OsStr::new("PATH")) {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::Internal,
            "validated child environment is missing mandatory PATH",
        ));
    }
    request.validate_trusted_node_runtime()?;
    backend
        .prepare(request, &preflight)
        .map_err(|error| error.0)?
        .finish()
}

fn validate_supported_evidence(
    request: &ExecutionRequest,
    support: &ContainmentSupport,
) -> Result<(), ExecutionError> {
    let required = EnforcementDimensions::requested_by(request.policy());
    let backend = support.backend();
    for (kind, value) in [
        ("backend name", backend.name()),
        ("backend version", backend.version()),
    ] {
        validate_identity_field(kind, value).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "backend returned an invalid identity",
            )
        })?;
    }
    if let Some(deprecation) = backend.deprecation() {
        validate_identity_field("backend deprecation", deprecation).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "backend returned an invalid identity",
            )
        })?;
    }
    if !support.is_supported() {
        let platform = support.platform.as_deref().unwrap_or("unknown platform");
        let reason = support.reason.as_deref().unwrap_or("no reason reported");
        return Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            format!("sandbox containment is unavailable on {platform}: {reason}"),
        ));
    }
    if support.requested != required {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            "backend requested evidence does not match the execution policy",
        ));
    }
    if !support.declared.contains(&required) || !support.observed.contains(&required) {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            "requested restrictions lack declared or observed backend support",
        ));
    }
    validate_dimension_evidence(&support.declared, support.declared_evidence())?;
    validate_dimension_evidence(&support.observed, support.observed_evidence())?;
    Ok(())
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
        .map(|grant| resolve_project_grant(&project_root, grant, FilesystemAccess::Read))
        .collect::<Result<Vec<_>, _>>()?;
    let mut write = filesystem
        .write()
        .iter()
        .map(|grant| resolve_project_grant(&project_root, grant, FilesystemAccess::Write))
        .collect::<Result<Vec<_>, _>>()?;
    let executable_search_paths = request
        .executable_search_paths()
        .iter()
        .map(|path| resolve_executable_search_directory(path))
        .collect::<Result<Vec<_>, _>>()?;
    validate_resolved_executable_search_paths(
        request.executable_search_paths(),
        &executable_search_paths,
    )?;
    read.extend(executable_search_paths);
    read.extend(additions.read);
    write.extend(additions.write);

    Ok(ResolvedSandboxPolicy {
        mode: request.policy().mode(),
        project_root,
        read,
        write,
        limits: request.policy().limits().clone(),
    })
}

fn resolve_project_grant(
    project_root: &Path,
    configured: &str,
    access: FilesystemAccess,
) -> Result<ResolvedGrant, ExecutionError> {
    let target = project_root.join(configured);
    let mut ancestor = target.as_path();
    let mut missing = Vec::new();

    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if access == FilesystemAccess::Read {
                    return Err(ExecutionError::new(
                        ExecutionErrorCategory::PolicyViolation,
                        format!("read grant target does not exist: {configured:?}"),
                    ));
                }
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
            Err(error) => return Err(path_error("inspect filesystem grant", &target, error)),
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
            kind: filesystem_kind(&canonical_ancestor)?,
            path: canonical_ancestor,
            access,
            source: FilesystemGrantSource::ProjectPolicy,
            resolution: GrantResolution::ExistingCanonical,
        });
    }

    missing.reverse();
    let relative_target = missing.iter().collect::<PathBuf>();
    let path = canonical_ancestor.join(&relative_target);
    Ok(ResolvedGrant {
        path,
        access,
        kind: FilesystemGrantKind::DirectorySubtree,
        source: FilesystemGrantSource::ProjectPolicy,
        resolution: GrantResolution::MissingWriteDirectory {
            canonical_ancestor,
            relative_target,
        },
    })
}

fn resolve_executable_search_directory(path: &Path) -> Result<ResolvedGrant, ExecutionError> {
    if !path.is_absolute() {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!(
                "executable search directory must be absolute: {}",
                path.display()
            ),
        ));
    }
    let canonical = canonical_path(path, "executable search directory")?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| path_error("executable search directory", &canonical, error))?;
    if !metadata.is_dir() {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!(
                "executable search path is not a directory: {}",
                path.display()
            ),
        ));
    }
    Ok(ResolvedGrant {
        path: canonical,
        access: FilesystemAccess::Read,
        kind: FilesystemGrantKind::DirectorySubtree,
        source: FilesystemGrantSource::BackendRuntime,
        resolution: GrantResolution::RuntimeCanonical,
    })
}

fn validate_resolved_executable_search_paths(
    requested: &[PathBuf],
    resolved: &[ResolvedGrant],
) -> Result<(), ExecutionError> {
    for (index, grant) in resolved.iter().enumerate() {
        if resolved[..index]
            .iter()
            .any(|prior| platform_paths_semantically_equal(&prior.path, &grant.path))
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                format!(
                    "executable search directories resolve to the same platform alias: {}",
                    requested[index].display()
                ),
            ));
        }
    }

    for (requested, resolved) in requested.iter().zip(resolved) {
        if requested != &resolved.path {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                format!(
                    "executable search directory must already be canonical: {}",
                    requested.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn platform_paths_semantically_equal(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn platform_paths_semantically_equal(left: &Path, right: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let left = left.as_os_str().encode_wide().collect::<Vec<_>>();
    let right = right.as_os_str().encode_wide().collect::<Vec<_>>();
    windows_path_units_semantically_equal(&left, &right)
}

fn resolve_runtime_grant(
    path: PathBuf,
    access: FilesystemAccess,
) -> Result<ResolvedGrant, ExecutionError> {
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
        kind: filesystem_kind(&canonical)?,
        path: canonical,
        access,
        source: FilesystemGrantSource::BackendRuntime,
        resolution: GrantResolution::RuntimeCanonical,
    })
}

fn filesystem_kind(path: &Path) -> Result<FilesystemGrantKind, ExecutionError> {
    let metadata = fs::metadata(path)
        .map_err(|error| path_error("inspect filesystem grant kind", path, error))?;
    if metadata.is_file() {
        Ok(FilesystemGrantKind::ExactFile)
    } else if metadata.is_dir() {
        Ok(FilesystemGrantKind::DirectorySubtree)
    } else {
        Err(ExecutionError::new(
            ExecutionErrorCategory::PolicyViolation,
            format!(
                "filesystem grant is neither a regular file nor directory: {}",
                path.display()
            ),
        ))
    }
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
        ExecutionError, ExecutionErrorCategory, ExecutionRequest, OwnedExecutionAttempt,
        PreparationError, ValidatedPreflight,
    };

    pub(super) struct PlatformBackend;

    impl ExecutionBackend for PlatformBackend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            containment_support(request)
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            _preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            Err(PreparationError::from(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "containment backend reported support but execution is not implemented",
            )))
        }
    }

    pub(super) fn containment_support(request: &ExecutionRequest) -> ContainmentSupport {
        ContainmentSupport::unsupported(
            BackendIdentity::new("tapid-runner/no-backend", env!("CARGO_PKG_VERSION"), None)
                .expect("static backend identity must satisfy the checked contract"),
            std::env::consts::OS,
            "no platform execution backend is implemented",
            EnforcementDimensions::requested_by(request.policy()),
            EnforcementDimensions::none(),
            EnforcementDimensions::none(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AssuranceLevel, ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy,
    };
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
    fn restricted_separates_authority_containment_from_managed_tree_ownership() {
        let restricted = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::Restricted,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let restricted = EnforcementDimensions::requested_by(&restricted);
        assert!(restricted.descriptor_hygiene());
        assert!(restricted.descendant_authority_propagation());
        assert!(!restricted.process_tree_membership());
        assert!(!restricted.complete_cleanup());

        let managed = EnforcementDimensions::requested_by(&required_policy());
        assert!(managed.process_tree_membership());
        assert!(managed.complete_cleanup());
    }

    #[test]
    fn managed_tree_reports_lifecycle_ownership_as_an_independent_dimension() {
        let managed = required_policy();
        let managed_requested = EnforcementDimensions::requested_by(&managed);
        let managed_evidence = evidence_for_dimensions(&managed_requested, "test", &[]);
        assert!(
            managed_evidence
                .iter()
                .any(|evidence| evidence.dimension() == EnforcementDimension::DescendantLifecycle)
        );

        let restricted = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::Restricted,
            managed.filesystem().clone(),
            managed.network(),
            managed.environment().to_vec(),
            managed.subprocess(),
            managed.limits().clone(),
        )
        .unwrap();
        let restricted_evidence = evidence_for_dimensions(
            &EnforcementDimensions::requested_by(&restricted),
            "test",
            &[],
        );
        assert!(
            !restricted_evidence
                .iter()
                .any(|evidence| evidence.dimension() == EnforcementDimension::DescendantLifecycle)
        );
    }

    #[test]
    fn support_reports_each_dimension_with_scope_mechanism_and_limitations() {
        let requested = EnforcementDimensions::requested_by(&required_policy());
        let support = support_with_evidence(requested.clone(), requested.clone(), requested);

        let filesystem = support
            .declared_evidence()
            .iter()
            .find(|evidence| evidence.dimension() == EnforcementDimension::FilesystemRead)
            .unwrap();
        assert_eq!(filesystem.scope(), EnforcementScope::DescendantTree);
        assert_eq!(filesystem.mechanism(), "test mechanism");
        assert_eq!(filesystem.limitations(), &["test evidence only"]);
        assert_eq!(
            support.observed_evidence().len(),
            support.declared_evidence().len()
        );
    }

    #[test]
    fn missing_dimension_metadata_fails_before_spawn() {
        struct Backend {
            support: ContainmentSupport,
            preparations: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, _request: &ExecutionRequest) -> ContainmentSupport {
                self.support.clone()
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.preparations.set(self.preparations.get() + 1);
                unreachable!()
            }
        }

        let request = ExecutionRequest::builder("node").build().unwrap();
        let required = EnforcementDimensions::requested_by(request.policy());
        let mut support = support_with_evidence(required.clone(), required.clone(), required);
        support
            .declared_evidence
            .retain(|evidence| evidence.dimension() != EnforcementDimension::DescriptorHygiene);
        let backend = Backend {
            support,
            preparations: std::cell::Cell::new(0),
        };
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert_eq!(backend.preparations.get(), 0);
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

    #[cfg(unix)]
    #[test]
    fn trusted_node_runtime_identity_is_retained_and_revalidated() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_directory("trusted-node-runtime");
        let runtime = root.join("node");
        fs::write(&runtime, b"first").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = fs::canonicalize(&runtime).unwrap();
        let runtime_dir = fs::canonicalize(&root).unwrap();
        let request = ExecutionRequest::builder("/bin/sh")
            .trusted_node_runtime(&runtime)
            .executable_search_path(&runtime_dir)
            .build()
            .unwrap();

        assert_eq!(
            request.trusted_node_runtime(),
            fs::canonicalize(&runtime).unwrap()
        );
        fs::remove_file(&runtime).unwrap();
        fs::write(&runtime, b"replacement").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();

        let error = request.validate_trusted_node_runtime().unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn request_builder_preserves_windows_verbatim_boundary_contract() {
        let request = ExecutionRequest::builder("cmd.exe")
            .args(["/D", "/S", "/C", "echo ok"])
            .windows_verbatim_arguments(true)
            .build()
            .unwrap();

        assert!(request.uses_windows_verbatim_arguments());
    }

    #[test]
    fn windows_verbatim_boundary_rejects_non_cmd_shapes_and_line_injection() {
        for request in [
            ExecutionRequest::builder("powershell.exe").args(["/D", "/S", "/C", "echo ok"]),
            ExecutionRequest::builder("cmd.exe").args(["/C", "echo ok"]),
            ExecutionRequest::builder("cmd.exe").args(["/D", "/S", "/C", "echo\rbreak"]),
            ExecutionRequest::builder("cmd.exe").args(["/D", "/S", "/C", "echo\nbreak"]),
        ] {
            assert!(request.windows_verbatim_arguments(true).build().is_err());
        }
    }

    #[test]
    fn request_builder_preserves_ordered_executable_search_paths() {
        let request = ExecutionRequest::builder("node")
            .executable_search_paths(["/project/node_modules/.bin", "/runtime/bin"])
            .build()
            .unwrap();

        assert_eq!(
            request.executable_search_paths(),
            [
                PathBuf::from("/project/node_modules/.bin"),
                PathBuf::from("/runtime/bin")
            ]
        );
    }

    #[test]
    fn executable_search_path_payload_bounds_and_duplicates_are_rejected() {
        let at_count = (0..MAX_EXECUTABLE_SEARCH_PATH_COUNT)
            .map(|index| PathBuf::from(format!("/{index}")))
            .collect::<Vec<_>>();
        assert!(
            ExecutionRequest::builder("node")
                .executable_search_paths(at_count.clone())
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("node")
                .executable_search_paths(at_count.into_iter().chain([PathBuf::from("/over")]))
                .build()
                .is_err()
        );

        let at_path_limit = format!("/{}", "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS - 1));
        assert!(
            ExecutionRequest::builder("node")
                .executable_search_path(&at_path_limit)
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("node")
                .executable_search_path(format!("{at_path_limit}x"))
                .build()
                .is_err()
        );

        let mut at_total = (0..7)
            .map(|index| {
                let prefix = format!("/{index}/");
                PathBuf::from(format!(
                    "{prefix}{}",
                    "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS - prefix.len())
                ))
            })
            .collect::<Vec<_>>();
        let used = 7 * MAX_EXECUTABLE_SEARCH_PATH_UNITS + 7 + 1;
        let remainder = MAX_EXECUTABLE_SEARCH_PATHS_UNITS - used;
        at_total.push(PathBuf::from(format!(
            "/last/{}",
            "x".repeat(remainder - "/last/".len())
        )));
        assert!(validate_executable_search_paths(&at_total).is_ok());
        assert!(
            ExecutionRequest::builder("node")
                .executable_search_paths(at_total.clone())
                .build()
                .is_err()
        );
        at_total.last_mut().unwrap().as_mut_os_string().push("x");
        assert!(validate_executable_search_paths(&at_total).is_err());

        assert!(
            ExecutionRequest::builder("node")
                .executable_search_paths(["/same", "/same"])
                .build()
                .is_err()
        );
        assert!(
            ExecutionRequest::builder("node")
                .executable_search_path("/bad\0path")
                .build()
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_search_directory_must_match_native_join_paths_semantics() {
        let paths = [PathBuf::from("/runtime/with:colon")];
        assert!(std::env::join_paths(&paths).is_err());

        let error = ExecutionRequest::builder("node")
            .executable_search_paths(paths)
            .build()
            .unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::InvalidRequest);
        assert!(error.to_string().contains("cannot be joined into PATH"));
    }

    #[test]
    fn windows_join_paths_helper_matches_quoted_separator_semantics() {
        let first: Vec<u16> = r"C:\runtime\bin".encode_utf16().collect();
        let with_separator: Vec<u16> = r"C:\runtime;tools\bin".encode_utf16().collect();
        let joined =
            join_windows_path_units([first.as_slice(), with_separator.as_slice()]).unwrap();
        assert_eq!(
            String::from_utf16(&joined).unwrap(),
            r#"C:\runtime\bin;"C:\runtime;tools\bin""#
        );

        let with_quote: Vec<u16> = "C:\\bad\"path".encode_utf16().collect();
        assert!(join_windows_path_units([with_quote.as_slice()]).is_err());
    }

    #[test]
    fn every_search_directory_is_bounded_and_joinable_before_duplicate_comparison() {
        let oversized = PathBuf::from(format!("/{}", "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS)));
        let error = ExecutionRequest::builder("node")
            .executable_search_paths([oversized.clone(), oversized])
            .build()
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
        assert!(!error.to_string().contains("duplicate"), "{error}");

        #[cfg(unix)]
        {
            let unjoinable = PathBuf::from("/bad:entry");
            let error = ExecutionRequest::builder("node")
                .executable_search_paths([unjoinable.clone(), unjoinable])
                .build()
                .unwrap_err();
            assert!(
                error.to_string().contains("cannot be joined into PATH"),
                "{error}"
            );
            assert!(!error.to_string().contains("duplicate"), "{error}");
        }
    }

    #[test]
    fn windows_alias_helper_rejects_ascii_case_equivalent_paths() {
        let upper: Vec<u16> = r"C:\Runtime\BIN".encode_utf16().collect();
        let lower: Vec<u16> = r"c:\runtime\bin".encode_utf16().collect();
        assert!(windows_path_units_semantically_equal(&upper, &lower));
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
    fn unsupported_preflight_makes_zero_prepare_attempts() {
        struct CountingBackend {
            prepare_attempts: std::cell::Cell<usize>,
        }

        impl ExecutionBackend for CountingBackend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                ContainmentSupport::unsupported(
                    BackendIdentity {
                        name: "test/unsupported".into(),
                        version: "1".into(),
                        deprecation: None,
                    },
                    "test",
                    "deliberately unavailable",
                    EnforcementDimensions::requested_by(request.policy()),
                    EnforcementDimensions::none(),
                    EnforcementDimensions::none(),
                )
            }

            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.prepare_attempts.set(self.prepare_attempts.get() + 1);
                Err(PreparationError::from(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "must not be reached",
                )))
            }
        }

        let backend = CountingBackend {
            prepare_attempts: std::cell::Cell::new(0),
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
        assert_eq!(backend.prepare_attempts.get(), 0);
    }

    #[test]
    fn malformed_supported_preflight_makes_zero_prepare_attempts() {
        struct CountingBackend {
            support: ContainmentSupport,
            prepare_attempts: std::cell::Cell<usize>,
        }

        impl ExecutionBackend for CountingBackend {
            fn containment_support(&self, _request: &ExecutionRequest) -> ContainmentSupport {
                self.support.clone()
            }

            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.prepare_attempts.set(self.prepare_attempts.get() + 1);
                Err(PreparationError::from(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "must not be reached",
                )))
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
                prepare_attempts: std::cell::Cell::new(0),
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
            assert_eq!(backend.prepare_attempts.get(), 0);
        }
    }

    #[test]
    fn disabled_mode_makes_zero_prepare_attempts() {
        struct CountingBackend {
            prepare_attempts: std::cell::Cell<usize>,
        }

        impl ExecutionBackend for CountingBackend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }

            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.prepare_attempts.set(self.prepare_attempts.get() + 1);
                Err(PreparationError::from(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "must not be reached",
                )))
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
            prepare_attempts: std::cell::Cell::new(0),
        };

        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        assert_eq!(backend.prepare_attempts.get(), 0);
    }

    #[test]
    fn process_creation_error_cannot_escape_owned_cleanup() {
        struct Backend {
            process_creations: std::rc::Rc<std::cell::Cell<usize>>,
            cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
        }
        struct Lifecycle {
            process_creations: std::rc::Rc<std::cell::Cell<usize>>,
            cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
            cleanup_completion: CompletionEvidence,
        }
        impl ExecutionLifecycle for Lifecycle {
            fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
                self.process_creations.set(self.process_creations.get() + 1);
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    "native process creation failed after creating a child",
                ))
            }
            fn cleanup(&mut self) -> CompletionEvidence {
                self.cleanup_checks.set(self.cleanup_checks.get() + 1);
                self.cleanup_completion.clone()
            }
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                Ok(OwnedExecutionAttempt::new(
                    preflight,
                    Box::new(Lifecycle {
                        process_creations: self.process_creations.clone(),
                        cleanup_checks: self.cleanup_checks.clone(),
                        cleanup_completion: completion_for(preflight),
                    }),
                ))
            }
        }

        let process_creations = std::rc::Rc::new(std::cell::Cell::new(0));
        let cleanup_checks = std::rc::Rc::new(std::cell::Cell::new(0));
        let request = ExecutionRequest::builder("node")
            .policy(required_policy())
            .build()
            .unwrap();
        let error = execute_with_backend(
            &request,
            &Backend {
                process_creations: process_creations.clone(),
                cleanup_checks: cleanup_checks.clone(),
            },
        )
        .unwrap_err();

        assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
        assert_eq!(process_creations.get(), 1);
        assert_eq!(cleanup_checks.get(), 1);
    }

    #[test]
    fn post_spawn_failure_returns_only_after_checked_cleanup() {
        struct Backend {
            cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
        }
        struct Lifecycle {
            cleanup_completion: Option<CompletionEvidence>,
            cleanup_checks: std::rc::Rc<std::cell::Cell<usize>>,
        }
        impl ExecutionLifecycle for Lifecycle {
            fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    "wait failed",
                ))
            }
            fn cleanup(&mut self) -> CompletionEvidence {
                self.cleanup_checks.set(self.cleanup_checks.get() + 1);
                self.cleanup_completion.take().unwrap()
            }
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                let dimensions =
                    EnforcementDimensions::completion_required(preflight.support.requested());
                let evidence = evidence_for_dimensions(&dimensions, "cleanup observation", &[]);
                let cleanup_completion = CompletionEvidence::checked(
                    preflight,
                    dimensions,
                    evidence,
                    CleanupConfidence::KernelOwnedComplete,
                )
                .unwrap();
                Ok(OwnedExecutionAttempt::new(
                    preflight,
                    Box::new(Lifecycle {
                        cleanup_completion: Some(cleanup_completion),
                        cleanup_checks: self.cleanup_checks.clone(),
                    }),
                ))
            }
        }

        let cleanup_checks = std::rc::Rc::new(std::cell::Cell::new(0));
        let request = ExecutionRequest::builder("node")
            .policy(required_policy())
            .build()
            .unwrap();
        let error = execute_with_backend(
            &request,
            &Backend {
                cleanup_checks: cleanup_checks.clone(),
            },
        )
        .unwrap_err();

        assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
        assert_eq!(cleanup_checks.get(), 1);
        assert_eq!(
            error.completion().unwrap().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
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
        let declared_evidence =
            evidence_for_dimensions(&declared, "test mechanism", &["test evidence only"]);
        let observed_evidence =
            evidence_for_dimensions(&observed, "test mechanism", &["test evidence only"]);
        ContainmentSupport::supported(
            BackendIdentity {
                name: "test".into(),
                version: "1".into(),
                deprecation: None,
            },
            requested,
            declared,
            observed,
            declared_evidence,
            observed_evidence,
        )
    }

    fn receipt_for(
        preflight: &ValidatedPreflight,
        enforced: EnforcementDimensions,
    ) -> Result<EnforcementReceipt, ExecutionError> {
        let evidence = evidence_for_dimensions(&enforced, "test launch establishment", &[]);
        EnforcementReceipt::checked(preflight, enforced, evidence)
    }

    fn completion_for(preflight: &ValidatedPreflight) -> CompletionEvidence {
        let requested = preflight.support.requested().clone();
        let completion_required = EnforcementDimensions::completion_required(&requested);
        let confidence = if requested.process_tree_membership() {
            CleanupConfidence::KernelOwnedComplete
        } else {
            CleanupConfidence::NotGuaranteed
        };
        let evidence =
            evidence_for_dimensions(&completion_required, "test completion observation", &[]);
        CompletionEvidence::checked(preflight, completion_required, evidence, confidence).unwrap()
    }

    struct FinishedLifecycle {
        result: Option<Result<Box<ExecutionOutcome>, ExecutionError>>,
        cleanup: CompletionEvidence,
    }

    impl ExecutionLifecycle for FinishedLifecycle {
        fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
            self.result.take().expect("test lifecycle finishes once")
        }

        fn cleanup(&mut self) -> CompletionEvidence {
            self.cleanup.clone()
        }
    }

    fn finished_attempt<'a>(
        preflight: &'a ValidatedPreflight,
        outcome: ExecutionOutcome,
    ) -> OwnedExecutionAttempt<'a> {
        let cleanup = outcome.completion().clone();
        OwnedExecutionAttempt::new(
            preflight,
            Box::new(FinishedLifecycle {
                result: Some(Ok(Box::new(outcome))),
                cleanup,
            }),
        )
    }

    struct OutcomeBackend {
        termination: Termination,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        preparations: std::cell::Cell<usize>,
    }

    impl ExecutionBackend for OutcomeBackend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            let requested = EnforcementDimensions::requested_by(request.policy());
            support_with_evidence(requested.clone(), requested.clone(), requested)
        }

        fn prepare<'a>(
            &'a self,
            _request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            self.preparations.set(self.preparations.get() + 1);
            let receipt = receipt_for(preflight, preflight.support.requested().clone()).unwrap();
            let outcome = ExecutionOutcome::checked(
                self.termination.clone(),
                self.stdout.clone(),
                self.stderr.clone(),
                receipt,
                completion_for(preflight),
            )
            .unwrap();
            Ok(finished_attempt(preflight, outcome))
        }
    }

    fn policy_with_limits(limits: ExecutionLimits) -> SandboxPolicy {
        SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            limits,
        )
        .unwrap()
    }

    fn assert_outcome_rejected_after_spawn(policy: SandboxPolicy, backend: OutcomeBackend) {
        let request = ExecutionRequest::builder("node")
            .policy(policy)
            .build()
            .unwrap();
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::PolicyViolation
        );
        assert_eq!(backend.preparations.get(), 1);
    }

    #[test]
    fn backend_output_over_exact_preflight_limit_is_rejected_after_spawn() {
        assert_outcome_rejected_after_spawn(
            policy_with_limits(ExecutionLimits::new(None, Some(3), None, None).unwrap()),
            OutcomeBackend {
                termination: Termination::Exited(0),
                stdout: vec![1, 2],
                stderr: vec![3, 4],
                preparations: std::cell::Cell::new(0),
            },
        );
    }

    #[test]
    fn backend_timeout_without_exact_preflight_limit_is_rejected_after_spawn() {
        assert_outcome_rejected_after_spawn(
            policy_with_limits(ExecutionLimits::default()),
            OutcomeBackend {
                termination: Termination::TimedOut,
                stdout: vec![],
                stderr: vec![],
                preparations: std::cell::Cell::new(0),
            },
        );
    }

    #[test]
    fn backend_output_termination_without_exact_preflight_limit_is_rejected_after_spawn() {
        assert_outcome_rejected_after_spawn(
            policy_with_limits(ExecutionLimits::default()),
            OutcomeBackend {
                termination: Termination::OutputLimitExceeded,
                stdout: vec![],
                stderr: vec![],
                preparations: std::cell::Cell::new(0),
            },
        );
    }

    #[test]
    fn backend_process_termination_without_exact_preflight_limit_is_rejected_after_spawn() {
        assert_outcome_rejected_after_spawn(
            policy_with_limits(ExecutionLimits::default()),
            OutcomeBackend {
                termination: Termination::ProcessLimitExceeded,
                stdout: vec![],
                stderr: vec![],
                preparations: std::cell::Cell::new(0),
            },
        );
    }

    #[test]
    fn backend_memory_termination_without_exact_preflight_limit_is_rejected_after_spawn() {
        assert_outcome_rejected_after_spawn(
            policy_with_limits(ExecutionLimits::default()),
            OutcomeBackend {
                termination: Termination::MemoryLimitExceeded,
                stdout: vec![],
                stderr: vec![],
                preparations: std::cell::Cell::new(0),
            },
        );
    }

    #[test]
    fn backend_terminations_coherent_with_exact_preflight_limits_are_accepted() {
        let cases = [
            (
                Termination::Exited(0),
                ExecutionLimits::new(None, Some(3), None, None).unwrap(),
                vec![1, 2],
                vec![3],
            ),
            (
                Termination::TimedOut,
                ExecutionLimits::new(Some(1), None, None, None).unwrap(),
                vec![],
                vec![],
            ),
            (
                Termination::OutputLimitExceeded,
                ExecutionLimits::new(None, Some(1), None, None).unwrap(),
                vec![1],
                vec![],
            ),
            (
                Termination::ProcessLimitExceeded,
                ExecutionLimits::new(None, None, Some(1), None).unwrap(),
                vec![],
                vec![],
            ),
            (
                Termination::MemoryLimitExceeded,
                ExecutionLimits::new(None, None, None, Some(1)).unwrap(),
                vec![],
                vec![],
            ),
        ];

        for (termination, limits, stdout, stderr) in cases {
            let backend = OutcomeBackend {
                termination: termination.clone(),
                stdout,
                stderr,
                preparations: std::cell::Cell::new(0),
            };
            let request = ExecutionRequest::builder("node")
                .policy(policy_with_limits(limits))
                .build()
                .unwrap();
            let outcome = execute_with_backend(&request, &backend).unwrap();
            assert_eq!(outcome.termination(), &termination);
            assert_eq!(backend.preparations.get(), 1);
        }
    }

    fn preflight_for(policy: SandboxPolicy, support: ContainmentSupport) -> ValidatedPreflight {
        let request = ExecutionRequest::builder("node")
            .policy(policy)
            .build()
            .unwrap();
        let policy = resolve_policy(&request, RuntimeFilesystemAdditions::default()).unwrap();
        let bindings = FilesystemBindings::canonical_path(&policy).unwrap();
        ValidatedPreflight {
            support,
            policy,
            bindings,
            child_environment: request.child_environment(),
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
    fn invalid_search_directories_fail_generic_preflight_before_spawn() {
        let root = temporary_directory("invalid-search-paths");
        let directory = root.join("directory");
        let file = root.join("file");
        fs::create_dir(&directory).unwrap();
        fs::write(&file, b"not a directory").unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        let canonical_directory = fs::canonicalize(&directory).unwrap();
        let noncanonical = canonical_directory
            .join("..")
            .join(canonical_directory.file_name().unwrap());
        let invalid = [
            PathBuf::from("relative"),
            canonical_root.join("missing"),
            fs::canonicalize(file).unwrap(),
            noncanonical,
        ];

        for path in invalid {
            let backend = OutcomeBackend {
                termination: Termination::Exited(0),
                stdout: vec![],
                stderr: vec![],
                preparations: std::cell::Cell::new(0),
            };
            let request = ExecutionRequest::builder("node")
                .project_root(&canonical_root)
                .executable_search_path(path)
                .build()
                .unwrap();
            assert_eq!(
                execute_with_backend(&request, &backend)
                    .unwrap_err()
                    .category(),
                ExecutionErrorCategory::PolicyViolation
            );
            assert_eq!(backend.preparations.get(), 0);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_directories_are_ordered_backend_runtime_receipt_grants() {
        struct Backend;
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                request: &ExecutionRequest,
                preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                assert_eq!(
                    request.executable_search_paths(),
                    [
                        preflight.policy.read[1].path.clone(),
                        preflight.policy.read[2].path.clone()
                    ]
                );
                assert_eq!(
                    preflight.child_environment.get(OsStr::new("PATH")),
                    Some(
                        &std::env::join_paths(request.executable_search_paths())
                            .expect("validated search paths must join")
                    )
                );
                let receipt =
                    receipt_for(preflight, preflight.support.requested().clone()).unwrap();
                let outcome = ExecutionOutcome::checked(
                    Termination::Exited(0),
                    vec![],
                    vec![],
                    receipt,
                    completion_for(preflight),
                )
                .unwrap();
                Ok(finished_attempt(preflight, outcome))
            }
        }

        let root = temporary_directory("search-path-root");
        let managed_bin = root.join("node_modules/.bin");
        let node_bin = root.join("selected-node/bin");
        fs::create_dir_all(&managed_bin).unwrap();
        fs::create_dir_all(&node_bin).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let managed_bin = fs::canonicalize(managed_bin).unwrap();
        let node_bin = fs::canonicalize(node_bin).unwrap();
        let request = ExecutionRequest::builder("node")
            .project_root(&root)
            .executable_search_paths([managed_bin.clone(), node_bin.clone()])
            .build()
            .unwrap();

        let outcome = execute_with_backend(&request, &Backend).unwrap();
        let runtime_grants = outcome
            .enforcement()
            .resolved_filesystem()
            .grants()
            .iter()
            .filter(|grant| grant.source() == FilesystemGrantSource::BackendRuntime)
            .collect::<Vec<_>>();
        assert_eq!(runtime_grants.len(), 2);
        assert_eq!(runtime_grants[0].path(), managed_bin);
        assert_eq!(runtime_grants[1].path(), node_bin);
        assert!(runtime_grants.iter().all(|grant| {
            grant.access() == FilesystemAccess::Read
                && grant.kind() == FilesystemGrantKind::DirectorySubtree
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonical_resolution_aliases_are_rejected_deterministically() {
        use std::os::unix::fs::symlink;

        struct Backend {
            probes: std::cell::Cell<usize>,
            preparations: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                self.probes.set(self.probes.get() + 1);
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.preparations.set(self.preparations.get() + 1);
                unreachable!()
            }
        }

        let root = temporary_directory("search-path-alias");
        let target = root.join("target");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&target).unwrap();
        symlink(&target, &first).unwrap();
        symlink(&target, &second).unwrap();
        let request = ExecutionRequest::builder("node")
            .project_root(fs::canonicalize(&root).unwrap())
            .executable_search_paths([first, second])
            .build()
            .unwrap();
        let backend = Backend {
            probes: std::cell::Cell::new(0),
            preparations: std::cell::Cell::new(0),
        };

        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        assert!(error.to_string().contains("alias"), "{error}");
        assert_eq!(backend.probes.get(), 1);
        assert_eq!(backend.preparations.get(), 0);
        fs::remove_dir_all(root).unwrap();
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

            fn bind_filesystem(
                &self,
                _request: &ExecutionRequest,
                policy: &ResolvedSandboxPolicy,
            ) -> Result<FilesystemBindings, ExecutionError> {
                for grant in &policy.write {
                    if matches!(
                        grant.resolution,
                        GrantResolution::MissingWriteDirectory { .. }
                    ) {
                        fs::create_dir_all(&grant.path).map_err(|error| {
                            path_error("materialize missing write subtree", &grant.path, error)
                        })?;
                    }
                }
                FilesystemBindings::canonical_path(policy)
            }

            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                assert_eq!(preflight.policy.read.len(), 2);
                assert_eq!(preflight.policy.write.len(), 1);
                assert!(matches!(
                    preflight.policy.write[0].resolution,
                    GrantResolution::MissingWriteDirectory { .. }
                ));
                let enforced = preflight.support.requested().clone();
                let receipt = receipt_for(preflight, enforced).unwrap();
                let outcome = ExecutionOutcome::checked(
                    Termination::Exited(0),
                    vec![],
                    vec![],
                    receipt,
                    completion_for(preflight),
                )
                .unwrap();
                Ok(finished_attempt(preflight, outcome))
            }
        }

        let root = temporary_directory("resolved-root");
        let runtime_root = temporary_directory("resolved-runtime");
        let runtime = runtime_root.join("node");
        fs::write(&runtime, b"runtime executable").unwrap();
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
        let grants = outcome.enforcement().resolved_filesystem().grants();
        assert_eq!(grants.len(), 3);
        assert_eq!(grants[0].path(), canonical_root.join("existing"));
        assert_eq!(grants[0].kind(), FilesystemGrantKind::DirectorySubtree);
        assert_eq!(grants[0].source(), FilesystemGrantSource::ProjectPolicy);
        assert_eq!(grants[1].path(), canonical_runtime);
        assert_eq!(grants[1].source(), FilesystemGrantSource::BackendRuntime);
        assert_eq!(grants[1].kind(), FilesystemGrantKind::ExactFile);
        assert_eq!(
            grants[2].path(),
            canonical_root.join("generated/nested/output.txt")
        );
        assert_eq!(grants[2].access(), FilesystemAccess::Write);
        assert_eq!(grants[2].kind(), FilesystemGrantKind::DirectorySubtree);
        assert!(
            grants
                .iter()
                .all(|grant| grant.binding() == FilesystemBindingMode::CanonicalPath)
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(runtime_root).unwrap();
    }

    #[test]
    fn resolved_policy_rejects_omitted_or_extra_receipt_grants() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let mut preflight = preflight_for(policy, support);

        let original = preflight.bindings.grants.pop().unwrap();
        assert!(preflight.bindings.validate(&preflight.policy).is_err());
        preflight.bindings.grants.push(BoundFilesystemGrant {
            receipt: ResolvedFilesystemGrant {
                path: PathBuf::from("/extra"),
                access: FilesystemAccess::Read,
                kind: FilesystemGrantKind::DirectorySubtree,
                source: FilesystemGrantSource::ProjectPolicy,
                binding: FilesystemBindingMode::CanonicalPath,
            },
            held: None,
            #[cfg(unix)]
            native_identity: None,
        });
        assert!(preflight.bindings.validate(&preflight.policy).is_err());
        preflight.bindings.grants.clear();
        preflight.bindings.grants.push(original);
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
            prepare_attempts: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.prepare_attempts.set(self.prepare_attempts.get() + 1);
                Err(PreparationError::from(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "must not be reached",
                )))
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
            prepare_attempts: std::cell::Cell::new(0),
        };

        let error = execute_with_backend(&request, &backend).unwrap_err();
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        assert_eq!(backend.prepare_attempts.get(), 0);
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
    fn checked_receipt_rejects_every_extra_or_missing_dimension() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);

        let mut declared = requested.clone();
        declared.timeout = true;
        let support = support_with_evidence(requested.clone(), declared.clone(), declared);
        let preflight = preflight_for(policy.clone(), support);
        let mut extra_configured_by_neither_policy_nor_request = requested.clone();
        extra_configured_by_neither_policy_nor_request.timeout = true;
        assert!(receipt_for(&preflight, extra_configured_by_neither_policy_nor_request).is_err());

        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let mut extra_unsupported = requested.clone();
        extra_unsupported.timeout = true;
        assert!(receipt_for(&preflight, extra_unsupported).is_err());

        let mut missing = requested;
        missing.network = false;
        assert!(receipt_for(&preflight, missing).is_err());
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
        assert!(receipt_for(&preflight, requested).is_err());
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

        assert!(receipt_for(&preflight, none).is_err());
    }

    #[test]
    fn receipt_keeps_launch_establishment_evidence_distinct_from_support_observation() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let launch = evidence_for_dimensions(&requested, "launch establishment", &[]);

        let receipt = EnforcementReceipt::checked(&preflight, requested, launch).unwrap();

        assert!(
            receipt
                .established_evidence()
                .iter()
                .all(|evidence| evidence.mechanism() == "launch establishment")
        );
        assert_ne!(
            receipt.established_evidence(),
            receipt.support().observed_evidence()
        );
    }

    #[test]
    fn completion_evidence_is_distinct_and_managed_tree_requires_complete_cleanup() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let receipt = receipt_for(&preflight, requested.clone()).unwrap();
        assert!(!receipt.established_evidence().is_empty());

        let incomplete_dimensions = EnforcementDimensions::completion_required(&requested);
        let incomplete_evidence = evidence_for_dimensions(
            &incomplete_dimensions,
            "incomplete cleanup observation",
            &[],
        );
        let incomplete = CompletionEvidence::checked(
            &preflight,
            incomplete_dimensions,
            incomplete_evidence,
            CleanupConfidence::NotGuaranteed,
        )
        .unwrap();
        assert!(
            ExecutionOutcome::checked(
                Termination::Exited(0),
                vec![],
                vec![],
                receipt.clone(),
                incomplete,
            )
            .is_err()
        );

        let complete_dimensions = EnforcementDimensions::completion_required(&requested);
        let complete_evidence =
            evidence_for_dimensions(&complete_dimensions, "complete cleanup observation", &[]);
        let complete = CompletionEvidence::checked(
            &preflight,
            complete_dimensions,
            complete_evidence,
            CleanupConfidence::KernelOwnedComplete,
        )
        .unwrap();
        let outcome =
            ExecutionOutcome::checked(Termination::Exited(0), vec![], vec![], receipt, complete)
                .unwrap();
        assert_eq!(
            outcome.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
    }

    #[test]
    fn restricted_completion_does_not_reconfirm_launch_dimensions() {
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::Restricted,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);

        let completion = CompletionEvidence::checked(
            &preflight,
            EnforcementDimensions::none(),
            vec![],
            CleanupConfidence::BestEffortObserved,
        )
        .unwrap();

        assert_eq!(completion.confirmed(), &EnforcementDimensions::none());
        assert!(completion.evidence().is_empty());
        assert_eq!(
            completion.cleanup_confidence(),
            CleanupConfidence::BestEffortObserved
        );
    }

    #[test]
    fn managed_completion_confirms_only_lifecycle_and_cleanup_dimensions() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let completion = completion_for(&preflight);

        assert!(completion.confirmed().descendant_lifecycle());
        assert!(completion.confirmed().process_tree_membership());
        assert!(completion.confirmed().complete_cleanup());
        assert!(!completion.confirmed().filesystem_read());
        assert!(!completion.confirmed().environment_sanitization());
        assert!(!completion.confirmed().descriptor_hygiene());
        assert_eq!(completion.evidence().len(), 3);
    }

    #[test]
    fn completion_keeps_backend_observation_evidence() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let confirmed = EnforcementDimensions::completion_required(&requested);
        let observed = evidence_for_dimensions(&confirmed, "completion observation", &[]);

        let completion = CompletionEvidence::checked(
            &preflight,
            confirmed,
            observed,
            CleanupConfidence::KernelOwnedComplete,
        )
        .unwrap();

        assert!(
            completion
                .evidence()
                .iter()
                .all(|evidence| evidence.mechanism() == "completion observation")
        );
    }

    #[test]
    fn checked_outcome_accepts_success_only_with_a_complete_receipt() {
        let policy = required_policy();
        let requested = EnforcementDimensions::requested_by(&policy);
        let support =
            support_with_evidence(requested.clone(), requested.clone(), requested.clone());
        let preflight = preflight_for(policy, support);
        let receipt = receipt_for(&preflight, requested).unwrap();
        let completion = completion_for(&preflight);
        let outcome =
            ExecutionOutcome::checked(Termination::Exited(0), vec![], vec![], receipt, completion)
                .unwrap();
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

    #[test]
    fn backend_identity_is_checked_and_bounded() {
        let at_limit = "x".repeat(MAX_BACKEND_IDENTITY_BYTES);
        assert!(BackendIdentity::new(at_limit.clone(), at_limit.clone(), Some(at_limit)).is_ok());
        for invalid in ["", "bad\0value", "bad\nvalue"] {
            assert_eq!(
                BackendIdentity::new(invalid, "1", None)
                    .unwrap_err()
                    .category(),
                ExecutionErrorCategory::InvalidRequest
            );
            assert!(BackendIdentity::new("backend", invalid, None).is_err());
            assert!(BackendIdentity::new("backend", "1", Some(invalid.into())).is_err());
        }
        let oversized = "x".repeat(MAX_BACKEND_IDENTITY_BYTES + 1);
        assert!(BackendIdentity::new(&oversized, "1", None).is_err());
        assert!(BackendIdentity::new("backend", &oversized, None).is_err());
        assert!(BackendIdentity::new("backend", "1", Some(oversized)).is_err());
    }

    #[test]
    fn request_payload_limits_and_nul_are_enforced() {
        assert!(
            ExecutionRequest::builder("x".repeat(MAX_PROGRAM_UNITS))
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("x".repeat(MAX_PROGRAM_UNITS + 1))
                .build()
                .is_err()
        );
        assert!(ExecutionRequest::builder("bad\0program").build().is_err());
        assert!(
            ExecutionRequest::builder("node")
                .args(std::iter::repeat_n("x", MAX_ARGUMENT_COUNT))
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("node")
                .args(std::iter::repeat_n("x", MAX_ARGUMENT_COUNT + 1))
                .build()
                .is_err()
        );
        assert!(
            ExecutionRequest::builder("node")
                .arg("x".repeat(MAX_ARGUMENT_UNITS + 1))
                .build()
                .is_err()
        );
        assert!(
            ExecutionRequest::builder("node")
                .arg("bad\0arg")
                .build()
                .is_err()
        );
        #[cfg(not(windows))]
        {
            let exact_argv = [
                "x".repeat(MAX_ARGUMENT_UNITS),
                "y".repeat(MAX_ARGV_UNITS - MAX_ARGUMENT_UNITS - 4),
            ];
            assert!(
                ExecutionRequest::builder("p")
                    .args(exact_argv.clone())
                    .build()
                    .is_ok()
            );
            let mut oversized_argv = exact_argv;
            oversized_argv[1].push('y');
            assert!(
                ExecutionRequest::builder("p")
                    .args(oversized_argv)
                    .build()
                    .is_err()
            );
            let cumulative = std::iter::repeat_n("x".repeat(MAX_ARGUMENT_UNITS), 2);
            assert!(
                ExecutionRequest::builder("node")
                    .args(cumulative)
                    .build()
                    .is_err()
            );
        }

        let policy = required_policy();
        assert!(
            ExecutionRequest::builder("node")
                .policy(policy.clone())
                .env("NODE_ENV", "x".repeat(MAX_ENVIRONMENT_VALUE_UNITS + 1))
                .build()
                .is_err()
        );
        assert!(
            ExecutionRequest::builder("node")
                .policy(policy.clone())
                .env("NODE_ENV", "bad\0value")
                .build()
                .is_err()
        );
        assert!(
            ExecutionRequest::builder("node")
                .policy(policy)
                .env("PATH", "/untrusted")
                .build()
                .is_err()
        );
    }

    #[test]
    fn project_root_limit_and_nul_are_rejected_before_support_probing() {
        struct Backend {
            probes: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                self.probes.set(self.probes.get() + 1);
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                unreachable!()
            }
        }

        assert!(
            ExecutionRequest::builder("node")
                .project_root("x".repeat(MAX_PROJECT_ROOT_UNITS))
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("node")
                .project_root("x".repeat(MAX_PROJECT_ROOT_UNITS + 1))
                .build()
                .is_err()
        );
        assert!(
            ExecutionRequest::builder("node")
                .project_root("bad\0root")
                .build()
                .is_err()
        );

        for invalid_root in [
            OsString::from("x".repeat(MAX_PROJECT_ROOT_UNITS + 1)),
            OsString::from("bad\0root"),
        ] {
            let backend = Backend {
                probes: std::cell::Cell::new(0),
            };
            let mut request = ExecutionRequest::builder("node").build().unwrap();
            request.project_root = PathBuf::from(invalid_root);
            assert_eq!(
                execute_with_backend(&request, &backend)
                    .unwrap_err()
                    .category(),
                ExecutionErrorCategory::InvalidRequest
            );
            assert_eq!(backend.probes.get(), 0);
        }
    }

    #[test]
    fn windows_command_line_bound_rejects_the_prior_raw_unit_limit() {
        let prior_raw_limit = 1 + 1 + 16_382 + 1 + 16_381 + 1;
        assert_eq!(prior_raw_limit, MAX_ARGV_UNITS);
        assert!(validate_windows_command_line_units(1, [16_382, 16_381]).is_err());
    }

    #[test]
    fn windows_command_line_bound_accepts_at_limit_conservative_cases() {
        assert_eq!(
            windows_command_line_units_upper_bound(4_096, [12_284]),
            Some(MAX_ARGV_UNITS - 1)
        );
        assert!(validate_windows_command_line_units(4_096, [12_284]).is_ok());
        assert_eq!(
            windows_command_line_units_upper_bound(1, [8_190, 8_188]),
            Some(MAX_ARGV_UNITS)
        );
        assert!(validate_windows_command_line_units(1, [8_190, 8_188]).is_ok());
        assert!(validate_windows_command_line_units(1, [8_190, 8_189]).is_err());
    }

    #[test]
    fn windows_command_line_bound_counts_full_worst_case_serialization() {
        // Quoted program + separator-delimited quoted arguments + terminating NUL, with every
        // input unit budgeted for worst-case quote/backslash expansion.
        assert_eq!(
            windows_command_line_units_upper_bound(3, [4, 5]),
            Some((2 * 3 + 2) + 1 + (2 * 4 + 2) + 1 + (2 * 5 + 2) + 1)
        );
    }

    #[test]
    fn captured_output_accounting_rejects_length_overflow() {
        assert!(checked_captured_output_bytes(usize::MAX, 1).is_err());
        assert_eq!(checked_captured_output_bytes(2, 3).unwrap(), 5);
    }

    #[cfg(unix)]
    #[test]
    fn unix_request_preserves_non_utf8_values_but_rejects_nul() {
        use std::os::unix::ffi::OsStringExt;
        let opaque_program = OsString::from_vec(vec![b'.', b'/', 0xff]);
        let opaque_argument = OsString::from_vec(vec![0xfe, b'x']);
        let opaque_root = OsString::from_vec(vec![b'.', b'/', 0xfd]);
        let opaque_environment = OsString::from_vec(vec![0xfc, b'x']);
        let request = ExecutionRequest::builder(opaque_program.clone())
            .arg(opaque_argument.clone())
            .project_root(PathBuf::from(opaque_root.clone()))
            .policy(required_policy())
            .env("NODE_ENV", opaque_environment.clone())
            .build()
            .unwrap();
        assert_eq!(request.program(), opaque_program);
        assert_eq!(request.arguments(), &[opaque_argument]);
        assert_eq!(request.project_root(), Path::new(&opaque_root));
        assert_eq!(
            request.environment().get(OsStr::new("NODE_ENV")),
            Some(&opaque_environment)
        );
        assert!(
            ExecutionRequest::builder("node")
                .arg(OsString::from_vec(vec![b'x', 0, b'y']))
                .build()
                .is_err()
        );
    }

    #[test]
    fn receipt_grants_expose_declared_kind_source_access_and_binding() {
        fn inspect(grant: &ResolvedFilesystemGrant) {
            let _: &Path = grant.path();
            let _: FilesystemAccess = grant.access();
            let _: FilesystemGrantKind = grant.kind();
            let _: FilesystemGrantSource = grant.source();
            let _: FilesystemBindingMode = grant.binding();
        }
        let _ = inspect;
    }

    #[test]
    fn missing_read_grant_fails_before_backend_binding_or_spawn() {
        struct Backend {
            bind_attempts: std::cell::Cell<usize>,
            prepare_attempts: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn bind_filesystem(
                &self,
                _request: &ExecutionRequest,
                _policy: &ResolvedSandboxPolicy,
            ) -> Result<FilesystemBindings, ExecutionError> {
                self.bind_attempts.set(self.bind_attempts.get() + 1);
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "must not bind",
                ))
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.prepare_attempts.set(self.prepare_attempts.get() + 1);
                Err(PreparationError::from(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "must not prepare",
                )))
            }
        }
        let root = temporary_directory("missing-read");
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec!["absent".into()], vec![]).unwrap(),
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
            bind_attempts: std::cell::Cell::new(0),
            prepare_attempts: std::cell::Cell::new(0),
        };
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::PolicyViolation
        );
        assert_eq!(backend.bind_attempts.get(), 0);
        assert_eq!(backend.prepare_attempts.get(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_search_path_payload_is_rejected_before_support_probing() {
        struct Backend {
            probes: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                self.probes.set(self.probes.get() + 1);
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                unreachable!()
            }
        }

        let mut malformed = vec![
            vec![PathBuf::from(format!(
                "/{}",
                "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS)
            ))],
            vec![PathBuf::from("/bad\0path")],
            vec![PathBuf::from("/same"), PathBuf::from("/same")],
            (0..=MAX_EXECUTABLE_SEARCH_PATH_COUNT)
                .map(|index| PathBuf::from(format!("/{index}")))
                .collect(),
            (0..9)
                .map(|index| {
                    let prefix = format!("/{index}/");
                    PathBuf::from(format!(
                        "{prefix}{}",
                        "x".repeat(MAX_EXECUTABLE_SEARCH_PATH_UNITS - prefix.len())
                    ))
                })
                .collect(),
        ];
        #[cfg(unix)]
        malformed.push(vec![PathBuf::from("/bad:entry")]);
        #[cfg(windows)]
        malformed.push(vec![PathBuf::from("C:\\bad\"entry")]);
        for executable_search_paths in malformed {
            let backend = Backend {
                probes: std::cell::Cell::new(0),
            };
            let mut request = ExecutionRequest::builder("node").build().unwrap();
            request.executable_search_paths = executable_search_paths;
            assert_eq!(
                execute_with_backend(&request, &backend)
                    .unwrap_err()
                    .category(),
                ExecutionErrorCategory::InvalidRequest
            );
            assert_eq!(backend.probes.get(), 0);
        }
    }

    #[test]
    fn malformed_request_is_rejected_before_support_probing() {
        struct Backend {
            probes: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                self.probes.set(self.probes.get() + 1);
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                unreachable!()
            }
        }

        let backend = Backend {
            probes: std::cell::Cell::new(0),
        };
        let mut request = ExecutionRequest::builder("node").build().unwrap();
        request.arguments.push(OsString::from("bad\0argument"));

        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::InvalidRequest
        );
        assert_eq!(backend.probes.get(), 0);
    }

    #[test]
    fn invalid_backend_identity_prevents_runtime_additions_and_spawn() {
        struct Backend {
            additions: std::cell::Cell<usize>,
            preparations: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                let mut support =
                    support_with_evidence(requested.clone(), requested.clone(), requested);
                support.backend.name.clear();
                support
            }
            fn runtime_filesystem_additions(
                &self,
                _request: &ExecutionRequest,
            ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
                self.additions.set(self.additions.get() + 1);
                Ok(RuntimeFilesystemAdditions::default())
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.preparations.set(self.preparations.get() + 1);
                unreachable!()
            }
        }
        let backend = Backend {
            additions: std::cell::Cell::new(0),
            preparations: std::cell::Cell::new(0),
        };
        let request = ExecutionRequest::builder("node").build().unwrap();
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::PolicyViolation
        );
        assert_eq!(backend.additions.get(), 0);
        assert_eq!(backend.preparations.get(), 0);
    }

    #[test]
    fn missing_write_requires_adapter_materialization_and_never_prepares_by_default() {
        struct Backend {
            preparations: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                self.preparations.set(self.preparations.get() + 1);
                unreachable!()
            }
        }
        let root = temporary_directory("missing-write-unsupported");
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec!["generated/output.txt".into()]).unwrap(),
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
            preparations: std::cell::Cell::new(0),
        };
        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert_eq!(backend.preparations.get(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_object_binding_holds_identity_and_receipt_reports_it() {
        let root = temporary_directory("native-binding");
        let request = ExecutionRequest::builder("node")
            .project_root(&root)
            .build()
            .unwrap();
        let policy = resolve_policy(&request, RuntimeFilesystemAdditions::default()).unwrap();
        let bindings = FilesystemBindings::native_objects(&policy).unwrap();
        bindings.validate(&policy).unwrap();
        let receipt = bindings.receipt();
        assert_eq!(receipt.grants().len(), 1);
        assert_eq!(
            receipt.grants()[0].binding(),
            FilesystemBindingMode::NativeObject
        );
        assert!(bindings.grants[0].held.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_environment_block_limit_counts_mandatory_path() {
        let exact_policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec!["ONE".into()],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let final_block_terminator = 1;
        let empty_path_entry = "PATH".len() + 1 + 1;
        let explicit_entry_framing = "ONE".len() + 1 + 1;
        let exact_value_units = MAX_ENVIRONMENT_BLOCK_UNITS
            - final_block_terminator
            - empty_path_entry
            - explicit_entry_framing;
        assert!(
            ExecutionRequest::builder("node")
                .policy(exact_policy.clone())
                .env("ONE", "x".repeat(exact_value_units))
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("node")
                .policy(exact_policy)
                .env("ONE", "x".repeat(exact_value_units + 1))
                .build()
                .is_err()
        );

        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec!["ONE".into(), "TWO".into()],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let request = ExecutionRequest::builder("node")
            .policy(policy)
            .env("ONE", "x".repeat(MAX_ENVIRONMENT_VALUE_UNITS / 2))
            .env("TWO", "y".repeat(MAX_ENVIRONMENT_VALUE_UNITS / 2));
        assert!(request.build().is_err());
    }

    #[test]
    fn complete_environment_combined_bound_includes_joined_path_and_has_no_off_by_one() {
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec!["ONE".into()],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let path = PathBuf::from("/bin");
        let path_units = os_units(path.as_os_str());
        let path_entry_units = "PATH".len() + 1 + path_units + 1;
        let explicit_entry_framing = "ONE".len() + 1 + 1;
        let exact_value_units =
            MAX_ENVIRONMENT_BLOCK_UNITS - 1 - path_entry_units - explicit_entry_framing;

        assert!(
            ExecutionRequest::builder("node")
                .policy(policy.clone())
                .executable_search_path(&path)
                .env("ONE", "x".repeat(exact_value_units))
                .build()
                .is_ok()
        );
        assert!(
            ExecutionRequest::builder("node")
                .policy(policy)
                .executable_search_path(path)
                .env("ONE", "x".repeat(exact_value_units + 1))
                .build()
                .is_err()
        );
    }

    #[test]
    fn empty_search_list_produces_an_explicitly_empty_child_path() {
        struct Backend;
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }

            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                assert_eq!(
                    preflight.child_environment.get(OsStr::new("PATH")),
                    Some(&OsString::new())
                );
                assert_eq!(preflight.child_environment.len(), 1);
                let receipt =
                    receipt_for(preflight, preflight.support.requested().clone()).unwrap();
                let outcome = ExecutionOutcome::checked(
                    Termination::Exited(0),
                    vec![],
                    vec![],
                    receipt,
                    completion_for(preflight),
                )
                .unwrap();
                Ok(finished_attempt(preflight, outcome))
            }
        }

        let request = ExecutionRequest::builder("node").build().unwrap();
        execute_with_backend(&request, &Backend).unwrap();
    }

    #[test]
    fn over_limit_complete_environment_is_rejected_before_support_probing() {
        struct Backend {
            probes: std::cell::Cell<usize>,
        }
        impl ExecutionBackend for Backend {
            fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
                self.probes.set(self.probes.get() + 1);
                let requested = EnforcementDimensions::requested_by(request.policy());
                support_with_evidence(requested.clone(), requested.clone(), requested)
            }
            fn prepare<'a>(
                &'a self,
                _request: &ExecutionRequest,
                _preflight: &'a ValidatedPreflight,
            ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
                unreachable!()
            }
        }

        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec!["ONE".into()],
            true,
            ExecutionLimits::default(),
        )
        .unwrap();
        let mut request = ExecutionRequest::builder("node")
            .policy(policy)
            .env("ONE", "ok")
            .build()
            .unwrap();
        request.environment.insert(
            OsString::from("ONE"),
            OsString::from("x".repeat(MAX_ENVIRONMENT_BLOCK_UNITS)),
        );
        let backend = Backend {
            probes: std::cell::Cell::new(0),
        };

        assert_eq!(
            execute_with_backend(&request, &backend)
                .unwrap_err()
                .category(),
            ExecutionErrorCategory::InvalidRequest
        );
        assert_eq!(backend.probes.get(), 0);
    }
}
