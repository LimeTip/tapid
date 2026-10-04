#![cfg(target_os = "windows")]
use super::{
    BackendIdentity, CleanupConfidence, CompletionEvidence, ContainmentSupport,
    EnforcementDimensions, EnforcementReceipt, ExecutionBackend, ExecutionError,
    ExecutionErrorCategory, ExecutionLifecycle, ExecutionOutcome, ExecutionRequest,
    FilesystemBindings, OwnedExecutionAttempt, PreparationError, ResolvedSandboxPolicy,
    Termination, ValidatedPreflight, evidence_for_dimensions,
};
use crate::config::ExecutionLimits;
use crate::execution::{
    serialize_windows_command_line_units, windows_environment_block_units,
    windows_job::{
        WindowsAppContainer, WindowsChildTermination, WindowsFilesystemGrants, WindowsJob,
        WindowsOutputCapture, WindowsStdioPipes, WindowsSuspendedChild,
    },
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INTERNAL_OUTPUT_CEILING: usize = 16 * 1024 * 1024;
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn add_node_appcontainer_options(environment: &mut std::collections::BTreeMap<OsString, OsString>) {
    const REQUIRED: [&str; 2] = ["--preserve-symlinks", "--preserve-symlinks-main"];
    let existing_key = environment
        .keys()
        .find(|name| name.to_string_lossy().eq_ignore_ascii_case("NODE_OPTIONS"))
        .cloned();
    let key = existing_key.unwrap_or_else(|| OsString::from("NODE_OPTIONS"));
    let mut options = environment.get(&key).cloned().unwrap_or_default();
    let existing_options = options.to_string_lossy().into_owned();
    for required in REQUIRED {
        if !existing_options
            .split_whitespace()
            .any(|option| option == required)
        {
            if !options.is_empty() {
                options.push(" ");
            }
            options.push(required);
        }
    }
    environment.insert(key, options);
}

pub(super) struct PlatformBackend;

impl ExecutionBackend for PlatformBackend {
    fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
        containment_support(request)
    }

    fn prepare<'a>(
        &'a self,
        request: &ExecutionRequest,
        preflight: &'a ValidatedPreflight,
    ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
        let mut lifecycle = WindowsExecutionLifecycle::new(request.clone(), preflight);
        lifecycle.prepare()?;
        Ok(OwnedExecutionAttempt::new(preflight, Box::new(lifecycle)))
    }

    fn bind_filesystem(
        &self,
        _request: &ExecutionRequest,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<FilesystemBindings, ExecutionError> {
        FilesystemBindings::canonical_path(policy)
    }
}

pub(super) fn containment_support(request: &ExecutionRequest) -> ContainmentSupport {
    let requested = EnforcementDimensions::requested_by(request.policy());
    let backend = BackendIdentity::new(
        "tapid-runner/windows-appcontainer-job",
        env!("CARGO_PKG_VERSION"),
        None,
    )
    .expect("static backend identity must satisfy the checked contract");
    if !request.policy().filesystem().write().is_empty() {
        return ContainmentSupport::unsupported(
            backend,
            "windows",
            "project write policies remain unsupported until declared writes and ACL revocation are natively verified",
            requested,
            EnforcementDimensions::none(),
            EnforcementDimensions::none(),
        );
    }
    if request.policy().network() {
        return ContainmentSupport::unsupported(
            backend,
            "windows",
            "network-enabled profiles are unsupported until AppContainer network capabilities are implemented",
            requested,
            EnforcementDimensions::none(),
            EnforcementDimensions::none(),
        );
    }
    let evidence = evidence_for_dimensions(
        &requested,
        "AppContainer token without network capabilities, explicit path ACL grants, suspended Job Object assignment, and bounded standard-handle capture",
        &[],
    );
    ContainmentSupport::supported(
        backend,
        requested.clone(),
        requested.clone(),
        requested,
        evidence.clone(),
        evidence,
    )
}

struct WindowsExecutionLifecycle<'a> {
    request: ExecutionRequest,
    preflight: &'a ValidatedPreflight,
    appcontainer: Option<WindowsAppContainer>,
    grants: Option<WindowsFilesystemGrants>,
    job: Option<WindowsJob>,
    child: Option<WindowsSuspendedChild>,
    capture: Option<WindowsOutputCapture>,
    cleanup_error: Option<ExecutionError>,
}

impl<'a> WindowsExecutionLifecycle<'a> {
    fn new(request: ExecutionRequest, preflight: &'a ValidatedPreflight) -> Self {
        Self {
            request,
            preflight,
            appcontainer: None,
            grants: None,
            job: None,
            child: None,
            capture: None,
            cleanup_error: None,
        }
    }

    fn prepare(&mut self) -> Result<(), PreparationError> {
        self.appcontainer = Some(WindowsAppContainer::create()?);
        let appcontainer = self.appcontainer.as_ref().expect("AppContainer prepared");
        self.grants = Some(WindowsFilesystemGrants::apply(
            appcontainer.sid(),
            &self.preflight.bindings.receipt().grants,
        )?);
        self.job = Some(WindowsJob::new(
            &self.preflight.policy.limits,
            !self.request.policy().subprocess(),
        )?);
        let executable = resolve_application(&self.request, self.preflight)?;
        let arguments = self
            .request
            .arguments()
            .iter()
            .map(|arg| arg.encode_wide().collect())
            .collect::<Vec<_>>();
        let application = wide(executable.as_os_str());
        use std::os::windows::ffi::OsStrExt;
        let command_line = serialize_windows_command_line_units(
            &executable.as_os_str().encode_wide().collect::<Vec<_>>(),
            &arguments,
            self.request.uses_windows_verbatim_arguments(),
        )?;
        let mut child_environment = self.preflight.child_environment.clone();
        if self.request.trusted_node_runtime.is_some() {
            add_node_appcontainer_options(&mut child_environment);
        }
        let environment = windows_environment_block_units(&child_environment)?;
        let working_directory = process_working_directory(&self.preflight.policy.project_root)?;
        let mut pipes = WindowsStdioPipes::new()?;
        self.child = Some(WindowsSuspendedChild::create_with_stdio(
            appcontainer,
            &application,
            &command_line,
            &environment,
            &working_directory,
            &mut pipes,
        )?);
        self.job
            .as_ref()
            .expect("Job Object prepared")
            .assign_suspended_process(
                self.child.as_ref().expect("child created").process_handle(),
            )?;
        let readers = pipes.into_parent_readers()?;
        self.capture = Some(WindowsOutputCapture::start(
            readers,
            Some(
                u64::try_from(
                    self.preflight
                        .policy
                        .limits
                        .max_output_bytes()
                        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
                        .unwrap_or(INTERNAL_OUTPUT_CEILING)
                        .min(INTERNAL_OUTPUT_CEILING),
                )
                .unwrap_or(u64::MAX),
            ),
        ));
        Ok(())
    }

    fn cleanup_resources(&mut self) -> CompletionEvidence {
        let mut cleanup_error = None;
        if let Some(job) = self.job.as_ref() {
            // Keep the Job and grants alive until either the kernel's active-process count reaches
            // zero or the associated completion port delivers ACTIVE_PROCESS_ZERO. If a bounded
            // notification wait fails, re-terminate and retry; no failed query is treated as proof.
            loop {
                if matches!(job.active_process_count(), Ok(0)) {
                    break;
                }
                let _ = job.terminate_all();
                let wait_ms = u32::try_from(CLEANUP_POLL_INTERVAL.as_millis()).unwrap_or(u32::MAX);
                if job.wait_until_empty(wait_ms.max(1)).is_ok() {
                    break;
                }
                std::thread::sleep(CLEANUP_POLL_INTERVAL);
            }
        }
        self.child.take();
        self.job.take(); // The tree is confirmed empty before the final kernel-owned handle closes.
        if let Some(capture) = self.capture.take() {
            if let Err(error) = capture.finish() {
                cleanup_error.get_or_insert(error);
            }
        }
        if let Some(grants) = self.grants.as_mut() {
            match grants.restore() {
                Ok(()) => {
                    self.grants = None;
                }
                Err(error) => {
                    cleanup_error.get_or_insert(error);
                }
            }
        }
        if let Some(appcontainer) = self.appcontainer.as_mut() {
            match appcontainer.cleanup() {
                Ok(()) => {
                    self.appcontainer = None;
                }
                Err(error) => {
                    cleanup_error.get_or_insert(error);
                }
            }
        }
        self.cleanup_error = cleanup_error;
        let confirmed =
            EnforcementDimensions::completion_required(self.preflight.support.requested());
        let evidence = evidence_for_dimensions(
            &confirmed,
            "Windows Job Object active-process count reached zero before filesystem grants were restored; kill-on-close remains the final kernel-owned boundary",
            &[],
        );
        let confidence = if self.preflight.support.requested().process_tree_membership() {
            CleanupConfidence::KernelOwnedComplete
        } else {
            CleanupConfidence::BestEffortObserved
        };
        CompletionEvidence::checked(self.preflight, confirmed, evidence, confidence)
            .expect("Windows completion evidence must exactly match the validated request")
    }
}

impl ExecutionLifecycle for WindowsExecutionLifecycle<'_> {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        let timeout_ms = timeout_milliseconds(&self.preflight.policy.limits)?;
        let output_limit_exceeded = self
            .capture
            .as_ref()
            .expect("prepared capture")
            .output_limit_exceeded();
        let child = self.child.as_mut().expect("prepared suspended child");
        let job = self.job.as_ref().expect("prepared Job Object");
        let termination =
            child.resume_and_wait_for_status(job, timeout_ms, output_limit_exceeded)?;
        let (stdout, stderr) = self.capture.take().expect("prepared capture").finish()?;
        let completion = self.cleanup_resources();
        if let Some(error) = self.cleanup_error.clone() {
            return Err(error.with_completion(completion));
        }
        let termination = match termination {
            WindowsChildTermination::Exited(code) => Termination::Exited(code as i32),
            WindowsChildTermination::TimedOut => Termination::TimedOut,
            WindowsChildTermination::OutputLimitExceeded
                if self.preflight.policy.limits.max_output_bytes().is_some() =>
            {
                Termination::OutputLimitExceeded
            }
            WindowsChildTermination::OutputLimitExceeded => {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::OutputLimit,
                    "Windows child exceeded Tapid's internal output safety ceiling",
                )
                .with_completion(completion));
            }
        };
        let enforced = self.preflight.support.requested().clone();
        let established = evidence_for_dimensions(
            &enforced,
            "AppContainer token and SID verified before resume; filesystem ACL grants applied; Job Object membership and limits verified; stdout/stderr drained with a combined byte cap",
            &[],
        );
        let receipt = EnforcementReceipt::checked(self.preflight, enforced, established)?;
        Ok(Box::new(ExecutionOutcome::checked(
            termination,
            stdout,
            stderr,
            receipt,
            completion,
        )?))
    }

    fn cleanup(&mut self) -> CompletionEvidence {
        self.cleanup_resources()
    }
}

impl Drop for WindowsExecutionLifecycle<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup_resources();
    }
}

fn timeout_milliseconds(limits: &ExecutionLimits) -> Result<u32, ExecutionError> {
    match limits.timeout_seconds() {
        Some(seconds) => seconds
            .checked_mul(1000)
            .and_then(|ms| u32::try_from(ms).ok())
            .ok_or_else(|| {
                ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows Job wait API cannot represent the configured timeout",
                )
            }),
        None => Ok(u32::MAX),
    }
}

fn resolve_application(
    request: &ExecutionRequest,
    preflight: &ValidatedPreflight,
) -> Result<PathBuf, ExecutionError> {
    let program = Path::new(request.program());
    let mut candidates = Vec::new();
    if program.is_absolute() {
        candidates.push(program.to_path_buf());
    } else if program.components().count() > 1 {
        candidates.push(preflight.policy.project_root.join(program));
    } else {
        for directory in request.executable_search_paths() {
            candidates.push(directory.join(program));
        }
    }
    for candidate in candidates {
        let mut variants = vec![candidate.clone()];
        if candidate.extension().is_none() {
            let mut with_extension = candidate.clone().into_os_string();
            with_extension.push(".exe");
            variants.push(PathBuf::from(with_extension));
        }
        for variant in variants {
            let Ok(resolved) = fs::canonicalize(&variant) else {
                continue;
            };
            let Ok(metadata) = fs::metadata(&resolved) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            if !preflight
                .policy
                .read
                .iter()
                .any(|grant| resolved == grant.path || resolved.starts_with(&grant.path))
            {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows executable path is not covered by a validated read grant",
                ));
            }
            return Ok(resolved);
        }
    }
    Err(ExecutionError::new(
        ExecutionErrorCategory::Spawn,
        "Windows executable was not found in the request's explicit executable search paths",
    ))
}

fn process_working_directory(path: &Path) -> Result<Vec<u16>, ExecutionError> {
    let mut units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    const EXTENDED_PREFIX: [u16; 4] = [92, 92, 63, 92];
    const EXTENDED_UNC_PREFIX: [u16; 8] = [92, 92, 63, 92, 85, 78, 67, 92];
    if units.starts_with(&EXTENDED_UNC_PREFIX) {
        units.splice(..EXTENDED_UNC_PREFIX.len(), [92, 92]);
    } else if units.starts_with(&EXTENDED_PREFIX) {
        units.drain(..EXTENDED_PREFIX.len());
    }
    if units.is_empty() || units.contains(&0) {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            "Windows working directory cannot be represented for process creation",
        ));
    }
    units.push(0);
    Ok(units)
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_node_appcontainer_options_preserve_caller_options_and_are_idempotent() {
        let key = OsString::from("node_options");
        let mut environment =
            std::collections::BTreeMap::from([(key.clone(), OsString::from("--trace-warnings"))]);
        add_node_appcontainer_options(&mut environment);
        add_node_appcontainer_options(&mut environment);
        assert_eq!(
            environment.get(&key).unwrap(),
            "--trace-warnings --preserve-symlinks --preserve-symlinks-main"
        );
    }
}
