#![cfg(target_os = "windows")]
use super::windows_cancellation::WindowsCancellation;
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

#[cfg(test)]
std::thread_local! {
    static EMPTY_JOB_BEFORE_RESTORE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod write_lifecycle_tests {
    use super::*;
    use crate::config::{AssuranceLevel, FilesystemPolicy, SandboxMode, SandboxPolicy};
    use crate::execution::{
        execute_with_backend,
        windows_acl::{initialize_inheritance, read_acl},
    };

    // Test-only backend seam invokes the real lifecycle without opening the integrated gate.
    struct WriteBackend;
    impl ExecutionBackend for WriteBackend {
        fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
            assert!(!request.policy().network());
            let requested = EnforcementDimensions::requested_by(request.policy());
            let identity =
                BackendIdentity::new("tapid-runner/native-write-lifecycle-test", "1", None)
                    .unwrap();
            let evidence = evidence_for_dimensions(
                &requested,
                "direct native write lifecycle acceptance seam",
                &[],
            );
            ContainmentSupport::supported(
                identity,
                requested.clone(),
                requested.clone(),
                requested,
                evidence.clone(),
                evidence,
            )
        }
        fn bind_filesystem(
            &self,
            request: &ExecutionRequest,
            policy: &ResolvedSandboxPolicy,
        ) -> Result<FilesystemBindings, ExecutionError> {
            PlatformBackend.bind_filesystem(request, policy)
        }
        fn prepare<'a>(
            &'a self,
            request: &ExecutionRequest,
            preflight: &'a ValidatedPreflight,
        ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
            PlatformBackend.prepare(request, preflight)
        }
    }

    fn run_write_completion(tail: &str, expected: Termination, assurance: AssuranceLevel) {
        let owner = tapid_test_support::TempProject::new("write-completion").unwrap();
        let runtime_owner =
            tapid_test_support::TempProject::new("write-completion-runtime").unwrap();
        let root = fs::canonicalize(owner.path()).unwrap();
        let runtime = fs::canonicalize(runtime_owner.path()).unwrap();
        let node = runtime.join("node.exe");
        fs::copy(
            std::env::var_os("TAPID_TEST_NODE").expect("real standalone Node is mandatory"),
            &node,
        )
        .unwrap();
        let writable = root.join("writable");
        let overlap = writable.join("overlap");
        let control = root.join("readonly control");
        fs::create_dir(&writable).unwrap();
        fs::create_dir(&overlap).unwrap();
        fs::create_dir(&control).unwrap();
        let existing = writable.join("existing.txt");
        fs::write(&existing, b"host-existing").unwrap();
        let control_dir = control.join("nested");
        fs::create_dir(&control_dir).unwrap();
        let control_file = control_dir.join("created.txt");
        fs::write(&control_file, b"control").unwrap();
        initialize_inheritance(&root);
        initialize_inheritance(&runtime);
        let paths = [
            &root,
            &writable,
            &overlap,
            &existing,
            &control,
            &control_dir,
            &control_file,
            &runtime,
            &node,
        ];
        let before: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            assurance,
            FilesystemPolicy::new(
                vec![".".into()],
                vec![
                    "writable".into(),
                    "writable/overlap".into(),
                    "writable".into(),
                ],
            )
            .unwrap(),
            false,
            vec![],
            false,
            ExecutionLimits::new(
                Some(if expected == Termination::Cancelled {
                    15
                } else {
                    2
                }),
                Some(4096),
                Some(8),
                Some(512 * 1024 * 1024),
            )
            .unwrap(),
        )
        .unwrap();
        let script = format!(
            r#"const fs=require('node:fs');fs.writeFileSync('writable/existing.txt','updated');fs.mkdirSync('writable/overlap/nested');fs.writeFileSync('writable/overlap/nested/created.txt','created');fs.closeSync(fs.openSync('writable/overlap/nested/created.txt',fs.constants.O_WRONLY));process.stdout.write('CREATED_AND_REOPENED\\n',()=>fs.writeFileSync('writable/overlap/nested/ready.txt','ready'));{tail}"#
        );
        let request = ExecutionRequest::builder(node.as_os_str())
            .args([
                OsString::from("--preserve-symlinks"),
                "--preserve-symlinks-main".into(),
                "-e".into(),
                script.into(),
            ])
            .project_root(&root)
            .executable_search_path(&runtime)
            .policy(policy)
            .build()
            .unwrap();
        assert!(containment_support(&request).is_supported());
        let cancellation_sender = if expected == Termination::Cancelled {
            let marker = overlap.join("nested/ready.txt");
            Some(std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(8);
                while !marker.exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "child never reached write marker before cancellation"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                // This runs only inside the isolated CREATE_NEW_CONSOLE helper.
                assert_ne!(
                    unsafe {
                        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
                            windows_sys::Win32::System::Console::CTRL_C_EVENT,
                            0,
                        )
                    },
                    0
                );
            }))
        } else {
            None
        };
        EMPTY_JOB_BEFORE_RESTORE.with(|count| count.set(0));
        let result = execute_with_backend(&request, &WriteBackend);
        if let Some(sender) = cancellation_sender {
            sender.join().unwrap();
        }
        let outcome = result.unwrap();
        assert_eq!(
            EMPTY_JOB_BEFORE_RESTORE.with(|count| count.get()),
            1,
            "native kernel empty-Job check must be observed before grant revocation"
        );
        let after: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        for ((path, before), after) in paths.iter().zip(&before).zip(&after) {
            eprintln!(
                "WRITE_LIFECYCLE_DACL_RECEIPT path={} before={before:02x?} after={after:02x?}",
                path.display()
            );
        }
        eprintln!(
            "CREATED_OBJECT_DACL_RECEIPT directory={:02x?} file={:02x?}",
            read_acl(&overlap.join("nested")),
            read_acl(&overlap.join("nested/created.txt"))
        );
        assert_eq!(
            after, before,
            "existing/control/runtime full raw DACL/control restoration"
        );
        assert_eq!(
            read_acl(&overlap.join("nested")),
            read_acl(&control_dir),
            "child-created directory revocation"
        );
        assert_eq!(
            read_acl(&overlap.join("nested/created.txt")),
            read_acl(&control_file),
            "child-created file revocation"
        );
        if overlap.join("nested/ready.txt").exists() {
            let ready_acl = read_acl(&overlap.join("nested/ready.txt"));
            eprintln!("READY_OBJECT_DACL_RECEIPT {ready_acl:02x?}");
            assert_eq!(ready_acl, read_acl(&control_file));
        }
        assert_eq!(fs::read(&existing).unwrap(), b"updated");
        assert_eq!(outcome.termination(), &expected);
        assert!(String::from_utf8_lossy(outcome.stdout()).contains("CREATED_AND_REOPENED"));
        let completion = outcome.completion();
        if assurance == AssuranceLevel::ManagedTree {
            assert_eq!(
                completion.cleanup_confidence(),
                CleanupConfidence::KernelOwnedComplete
            );
            assert!(completion.confirmed().process_tree_membership());
            assert!(completion.confirmed().descendant_lifecycle());
        } else {
            assert_eq!(
                completion.cleanup_confidence(),
                CleanupConfidence::BestEffortObserved
            );
            assert!(!completion.confirmed().process_tree_membership());
            assert!(!completion.confirmed().descendant_lifecycle());
        }
    }

    #[test]
    fn write_normal_zero_revokes_existing_and_created_objects() {
        run_write_completion(
            "process.exit(0)",
            Termination::Exited(0),
            AssuranceLevel::ManagedTree,
        );
    }

    #[test]
    fn write_normal_nonzero_revokes_existing_and_created_objects() {
        run_write_completion(
            "process.exit(7)",
            Termination::Exited(7),
            AssuranceLevel::ManagedTree,
        );
    }

    #[test]
    fn write_timeout_revokes_existing_and_created_objects() {
        run_write_completion(
            "setInterval(()=>{},1000)",
            Termination::TimedOut,
            AssuranceLevel::ManagedTree,
        );
    }

    #[test]
    fn write_output_limit_revokes_existing_and_created_objects() {
        run_write_completion(
            "while(true)process.stdout.write('x'.repeat(4096))",
            Termination::OutputLimitExceeded,
            AssuranceLevel::ManagedTree,
        );
    }

    #[test]
    fn restricted_write_completion_does_not_claim_managed_tree_dimensions() {
        run_write_completion(
            "process.exit(0)",
            Termination::Exited(0),
            AssuranceLevel::Restricted,
        );
    }

    #[test]
    fn write_ctrl_c_revokes_existing_and_created_objects() {
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const ENV: &str = "TAPID_NATIVE_WRITE_CTRL_C_HELPER";
        if std::env::var_os(ENV).is_some() {
            run_write_completion(
                "setInterval(()=>{},1000)",
                Termination::Cancelled,
                AssuranceLevel::ManagedTree,
            );
            println!("WRITE_CTRL_C_SUCCESS");
            return;
        }
        let mut helper = Command::new(std::env::current_exe().unwrap()).args([
            "--exact", "execution::platform_backend::write_lifecycle_tests::write_ctrl_c_revokes_existing_and_created_objects", "--nocapture", "--test-threads=1"
        ]).env(ENV, "1").creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE)
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let drain = |mut pipe: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                bytes
            })
        };
        let stdout = drain(Box::new(helper.stdout.take().unwrap()));
        let stderr = drain(Box::new(helper.stderr.take().unwrap()));
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        let status = loop {
            if let Some(status) = helper.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = Command::new("taskkill.exe")
                    .args(["/PID", &helper.id().to_string(), "/T", "/F"])
                    .output();
                let _ = helper.wait();
                panic!("isolated write cancellation helper timed out");
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let out = stdout.join().unwrap();
        let err = stderr.join().unwrap();
        println!("{}", String::from_utf8_lossy(&out));
        eprintln!("{}", String::from_utf8_lossy(&err));
        assert!(
            status.success(),
            "isolated write cancellation failed: {status}"
        );
        assert!(String::from_utf8_lossy(&out).contains("WRITE_CTRL_C_SUCCESS"));
    }

    #[test]
    fn actual_spawn_failure_surfaces_write_cleanup_failure() {
        preparation_failure(false);
    }

    #[test]
    fn missing_executable_revokes_write_grants() {
        preparation_failure(true);
    }

    fn preparation_failure(missing: bool) {
        let owner = tapid_test_support::TempProject::new("spawn-write-cleanup").unwrap();
        let root = fs::canonicalize(owner.path()).unwrap();
        let writable = root.join("writable");
        fs::create_dir(&writable).unwrap();
        let invalid = root.join("invalid.exe");
        fs::write(&invalid, b"not a Windows executable").unwrap();
        initialize_inheritance(&root);
        let paths = [&root, &writable, &invalid];
        let before: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec!["writable".into()]).unwrap(),
            false,
            vec![],
            false,
            ExecutionLimits::new(Some(5), Some(4096), Some(8), Some(512 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let program = if missing {
            root.join("missing.exe")
        } else {
            invalid.clone()
        };
        let request = ExecutionRequest::builder(program.as_os_str())
            .project_root(&root)
            .executable_search_path(&root)
            .policy(policy)
            .build()
            .unwrap();
        assert!(containment_support(&request).is_supported());
        WindowsFilesystemGrants::fail_next_restore_for_test();
        let error = execute_with_backend(&request, &WriteBackend).unwrap_err();
        let after: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        assert_eq!(
            after, before,
            "exact raw DACL/control cleanup before return"
        );
        if missing {
            assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
            assert!(
                error.to_string().contains("not found"),
                "missing executable was not rejected: {error}"
            );
            assert!(
                !program.exists(),
                "missing target must never be materialized"
            );
        } else {
            assert_eq!(
                error.category(),
                ExecutionErrorCategory::UnsupportedContainment
            );
            assert!(
                error.to_string().contains("create suspended child")
                    && (error.to_string().contains("193") || error.to_string().contains("216")),
                "must reach real CreateProcess invalid-image failure: {error}"
            );
        }
        assert!(
            error.to_string().contains("injected rollback denial"),
            "cleanup failure swallowed: {error}"
        );
    }
}

// Opt-in host-side diagnostics only: never log arguments, environment values, or child output.
pub(super) fn trace_windows_stage(stage: &str) {
    if std::env::var_os("TAPID_WINDOWS_STAGE_TRACE").as_deref() == Some(OsStr::new("1")) {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let elapsed = START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis();
        eprintln!("[tapid-windows-stage] {elapsed}ms {stage}");
    }
}

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
        if let Err(mut error) = lifecycle.prepare() {
            // Preparation owns grants even when no child is created. Explicit cleanup is
            // observable; Drop is only a final retry for resources whose restoration failed.
            lifecycle.cleanup_resources();
            if let Some(cleanup) = lifecycle.cleanup_error.as_ref() {
                error
                    .0
                    .message
                    .push_str(&format!("; Windows preparation cleanup failed: {cleanup}"));
            }
            return Err(error);
        }
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
    cancellation: Option<WindowsCancellation>,
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
            cancellation: None,
            appcontainer: None,
            grants: None,
            job: None,
            child: None,
            capture: None,
            cleanup_error: None,
        }
    }

    fn prepare(&mut self) -> Result<(), PreparationError> {
        trace_windows_stage("prepare: cancellation");
        self.cancellation = Some(WindowsCancellation::install_and_activate()?);
        trace_windows_stage("prepare: AppContainer");
        self.appcontainer = Some(WindowsAppContainer::create()?);
        let appcontainer = self.appcontainer.as_ref().expect("AppContainer prepared");
        trace_windows_stage("prepare: filesystem grants");
        self.grants = Some(WindowsFilesystemGrants::apply(
            appcontainer,
            &self.preflight.bindings.receipt().grants,
        )?);
        trace_windows_stage("prepare: Job Object");
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
        trace_windows_stage("prepare: suspended child");
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
        #[cfg(test)]
        if self.grants.is_some()
            && let Some(job) = self.job.as_ref()
        {
            assert_eq!(
                job.active_process_count().unwrap(),
                0,
                "ACL restoration must never precede kernel-confirmed empty Job"
            );
            EMPTY_JOB_BEFORE_RESTORE.with(|count| count.set(count.get() + 1));
        }
        self.child.take();
        self.job.take(); // The tree is confirmed empty before the final kernel-owned handle closes.
        if let Some(capture) = self.capture.take()
            && let Err(error) = capture.finish()
        {
            cleanup_error.get_or_insert(error);
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
        let cancellation = self
            .cancellation
            .as_ref()
            .expect("prepared cancellation scope");
        trace_windows_stage("execute: resume and wait");
        let termination = child.resume_and_wait_for_status(
            job,
            timeout_ms,
            output_limit_exceeded,
            Some(cancellation),
        )?;
        trace_windows_stage("execute: tree exited, drain output");
        let (stdout, stderr) = self.capture.take().expect("prepared capture").finish()?;
        trace_windows_stage("execute: cleanup");
        let completion = self.cleanup_resources();
        // Cleanup is part of the cancellation scope. Atomically stop owning console events and
        // snapshot the final generation, so an event cannot be swallowed after a late check.
        let cancelled = self
            .cancellation
            .take()
            .expect("prepared cancellation scope")
            .finish();
        let termination = if cancelled {
            WindowsChildTermination::Cancelled
        } else {
            termination
        };
        trace_windows_stage("execute: complete");
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
            WindowsChildTermination::Cancelled => Termination::Cancelled,
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
