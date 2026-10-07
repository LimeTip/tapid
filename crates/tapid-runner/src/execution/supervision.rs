//! Preflight validation and owned process lifecycle with checked fallback cleanup.

use super::*;

/// A prepared backend lifecycle. The owner exists before `execute` may create a process.
pub(super) trait ExecutionLifecycle {
    /// Create the native process and drive it through completion.
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError>;
    /// Report checked cleanup evidence, or no evidence if no process was created.
    fn cleanup(&mut self) -> Result<Option<CompletionEvidence>, ExecutionError>;
}

/// A preparation failure raised before an owned attempt can create a process.
pub(super) struct PreparationError(pub(super) ExecutionError);

impl From<ExecutionError> for PreparationError {
    fn from(error: ExecutionError) -> Self {
        Self(error)
    }
}

/// Owns the lifecycle before native process creation. Explicit finish validates every returned
/// disposition and checks cleanup after every execution error. Drop makes a best-effort cleanup
/// attempt because it cannot report a cleanup failure to the caller.
pub(super) struct OwnedExecutionAttempt<'a> {
    preflight: &'a ValidatedPreflight,
    lifecycle: Box<dyn ExecutionLifecycle + 'a>,
    finished: bool,
}

impl<'a> OwnedExecutionAttempt<'a> {
    #[allow(dead_code)] // Reserved for platform backends; test backends exercise owned attempts.
    pub(super) fn new(
        preflight: &'a ValidatedPreflight,
        lifecycle: Box<dyn ExecutionLifecycle + 'a>,
    ) -> Self {
        Self {
            preflight,
            lifecycle,
            finished: false,
        }
    }

    pub(super) fn finish(mut self) -> Result<ExecutionOutcome, ExecutionError> {
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
        // Prevent a panic during cleanup validation from causing a second cleanup attempt while
        // unwinding this explicit finish path.
        self.finished = true;
        match self.lifecycle.cleanup() {
            Ok(None) => error,
            Ok(Some(completion)) => {
                if let Err(cleanup_error) =
                    validate_completion_for_preflight(&completion, self.preflight)
                {
                    cleanup_failed(error, cleanup_error)
                } else {
                    error.with_completion(completion)
                }
            }
            Err(cleanup_error) => cleanup_failed(error, cleanup_error),
        }
    }
}

impl Drop for OwnedExecutionAttempt<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // Drop cannot report cleanup errors to the caller. Explicit finish handles and returns
        // them; this fallback must not panic or forge completion evidence.
        let _ = self.lifecycle.cleanup();
        self.finished = true;
    }
}

fn cleanup_failed(error: ExecutionError, cleanup_error: ExecutionError) -> ExecutionError {
    ExecutionError::new(
        error.category,
        format!(
            "{}; cleanup could not be confirmed: {}",
            error.message, cleanup_error.message
        ),
    )
}

pub(super) fn validate_completion_for_preflight(
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

pub(super) trait ExecutionBackend {
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

pub(super) fn execute_with_backend(
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
