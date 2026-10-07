mod request;
#[cfg(target_os = "macos")]
use request::TrustedNodeRuntime;
#[cfg(any(windows, test))]
use request::windows_path_units_semantically_equal;
pub use request::{ExecutionRequest, ExecutionRequestBuilder};
#[cfg(test)]
use request::{
    assemble_windows_environment_block, join_executable_search_paths,
    windows_appcontainer_environment_entries, windows_environment_path_units,
};
#[cfg(test)]
use request::{
    join_windows_path_units, os_units, validate_executable_search_paths,
    validate_windows_command_line_units, windows_command_line_units_upper_bound,
};
#[cfg(any(windows, test))]
use request::{serialize_windows_command_line_units, windows_environment_block_units};

mod supervision;
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
use supervision::ExecutionLifecycle;
use supervision::{
    ExecutionBackend, OwnedExecutionAttempt, PreparationError, execute_with_backend,
};

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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
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
    /// Data only, excluding implicit metadata access.
    ReadData,
    /// Metadata only, without file contents or directory listings.
    ReadMetadata,
    Read,
    Write,
}

/// The policy semantics declared for a target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemGrantKind {
    /// Only the directory itself, excluding descendants.
    ExactDirectory,
    /// An explicit backend-owned character device, with identity revalidation.
    CharacterDevice,
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
        self.grants.iter().filter(|grant| {
            matches!(
                grant.access,
                FilesystemAccess::Read
                    | FilesystemAccess::ReadData
                    | FilesystemAccess::ReadMetadata
            )
        })
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
    #[cfg(target_os = "macos")]
    RuntimeDevice {
        device: u64,
        inode: u64,
        rdev: u64,
    },
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
    assurance: AssuranceLevel,
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
                if canonical != grant.path || !grant_kind_matches(grant, &canonical)? {
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
            if canonical != expected.path || !grant_kind_matches(expected, &canonical)? {
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
    if canonical != grant.path || !grant_kind_matches(grant, &canonical)? {
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
                #[cfg(target_os = "macos")]
                GrantResolution::RuntimeDevice { .. } => {
                    validate_canonical_path(&grant.path)?;
                    if !grant_kind_matches(grant, &grant.path)? {
                        return Err(binding_mismatch());
                    }
                }
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
    executable_resolution: Option<ExecutableResolutionEvidence>,
    assurance: AssuranceLevel,
    support: ContainmentSupport,
    enforced: EnforcementDimensions,
    established_evidence: Vec<DimensionEvidence>,
    resolved_filesystem: ResolvedFilesystemGrants,
    configured_limits: ExecutionLimits,
}

/// Exact Unix bytes for PATH and each entry, preserving non-UTF-8 paths.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ExecutableResolutionEvidence {
    pub path: Vec<u8>,
    pub path_order: Vec<Vec<u8>>,
    pub caller_path_inherited: bool,
    pub reserved_node: Option<ReservedExecutableEvidence>,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ReservedExecutableEvidence {
    pub command: String,
    pub private_path: Vec<u8>,
    pub trusted_runtime: Vec<u8>,
    pub device: u64,
    pub inode: u64,
    pub mechanism: String,
    pub identity_checks: String,
    /// Whether the private binding was removed, not whether descendants were cleaned.
    /// macOS retains it when subprocess-enabled targets may have executed.
    pub cleanup_observed: bool,
    pub limitations: String,
}

impl EnforcementReceipt {
    pub fn executable_resolution(&self) -> Option<&ExecutableResolutionEvidence> {
        self.executable_resolution.as_ref()
    }

    #[allow(dead_code)] // Non-macOS backends cannot produce receipts.
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
            executable_resolution: None,
            assurance: preflight.policy.assurance,
            support: support.clone(),
            enforced,
            established_evidence,
            resolved_filesystem: preflight.bindings.receipt(),
            configured_limits: configured_limits.clone(),
        })
    }

    pub fn assurance(&self) -> AssuranceLevel {
        self.assurance
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
    #[allow(dead_code)] // Non-macOS backends cannot produce completion evidence.
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
    Cancelled,
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
    process_memory_stats_hint: bool,
    enforcement: EnforcementReceipt,
    completion: CompletionEvidence,
}

impl ExecutionOutcome {
    #[allow(dead_code)] // Non-macOS backends cannot produce outcomes.
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
            process_memory_stats_hint: false,
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
    /// Whether stderr matched the known libuv process-memory-stat permission failure.
    pub fn process_memory_stats_hint(&self) -> bool {
        self.process_memory_stats_hint
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn with_process_memory_stats_hint(mut self, hint: bool) -> Self {
        self.process_memory_stats_hint = hint;
        self
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

/// Attempts execution through a private platform backend.
///
/// Experimental macOS Restricted execution requires early private-launcher initialization.
/// Unsupported platforms and required dimensions fail before target spawn.
pub fn execute(request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
    #[cfg(target_os = "macos")]
    return platform_backend::execute_reserved(request);
    #[cfg(not(target_os = "macos"))]
    execute_with_backend(request, &platform_backend::PlatformBackend)
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
        assurance: request.policy().assurance(),
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
    #[cfg(target_os = "macos")]
    if matches!(
        canonical.to_str(),
        Some("/dev/null" | "/dev/random" | "/dev/urandom")
    ) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = fs::metadata(&canonical).map_err(|_| binding_mismatch())?;
        if !metadata.file_type().is_char_device() {
            return Err(binding_mismatch());
        }
        return Ok(ResolvedGrant {
            path: canonical,
            access,
            kind: FilesystemGrantKind::CharacterDevice,
            source: FilesystemGrantSource::BackendRuntime,
            resolution: GrantResolution::RuntimeDevice {
                device: metadata.dev(),
                inode: metadata.ino(),
                rdev: metadata.rdev(),
            },
        });
    }
    Ok(ResolvedGrant {
        kind: filesystem_kind(&canonical)?,
        path: canonical,
        access,
        source: FilesystemGrantSource::BackendRuntime,
        resolution: GrantResolution::RuntimeCanonical,
    })
}

fn grant_kind_matches(grant: &ResolvedGrant, path: &Path) -> Result<bool, ExecutionError> {
    #[cfg(target_os = "macos")]
    if let GrantResolution::RuntimeDevice {
        device,
        inode,
        rdev,
    } = grant.resolution
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = fs::metadata(path).map_err(|_| binding_mismatch())?;
        return Ok(grant.source == FilesystemGrantSource::BackendRuntime
            && grant.kind == FilesystemGrantKind::CharacterDevice
            && metadata.file_type().is_char_device()
            && (metadata.dev(), metadata.ino(), metadata.rdev()) == (device, inode, rdev));
    }
    let actual = filesystem_kind(path)?;
    Ok(actual == grant.kind
        || (actual == FilesystemGrantKind::DirectorySubtree
            && grant.kind == FilesystemGrantKind::ExactDirectory))
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

#[cfg(windows)]
#[path = "windows_execution.rs"]
mod platform_backend;
#[cfg(windows)]
#[path = "windows_cancellation.rs"]
mod windows_cancellation;
#[cfg(windows)]
#[path = "windows_job.rs"]
mod windows_job;

#[cfg(target_os = "macos")]
#[path = "macos_restricted.rs"]
mod platform_backend;

#[cfg(target_os = "linux")]
#[path = "linux_restricted.rs"]
mod platform_backend;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use platform_backend::dispatch_private_launcher;

#[cfg(test)]
#[path = "execution/tests.rs"]
mod tests;
