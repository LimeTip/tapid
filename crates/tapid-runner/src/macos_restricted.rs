//! Experimental macOS Restricted backend using deprecated Seatbelt `sandbox-exec`.

use super::{
    AssuranceLevel, BackendIdentity, CleanupConfidence, CompletionEvidence, ContainmentSupport,
    EnforcementDimensions, EnforcementReceipt, ExecutionBackend, ExecutionError,
    ExecutionErrorCategory, ExecutionLifecycle, ExecutionOutcome, ExecutionRequest,
    FilesystemAccess, FilesystemBindings, FilesystemGrantKind, OwnedExecutionAttempt,
    PreparationError, ResolvedSandboxPolicy, RuntimeFilesystemAdditions, Termination,
    ValidatedPreflight, evidence_for_dimensions,
};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const INTERNAL_OUTPUT_CEILING: usize = 16 * 1024 * 1024;
const PIPE_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const LIMITATIONS: &[&str] = &[
    "experimental backend uses deprecated sandbox-exec and path-based CanonicalPath grants",
    "root directory names and filesystem metadata are readable for runtime compatibility",
    "named sysctls and unrestricted network are non-filesystem Seatbelt grants",
    "process-group supervision is best effort; detached descendants may escape cleanup",
    "no cancellation API or process-global signal handlers are installed",
];

pub(super) struct PlatformBackend;

impl ExecutionBackend for PlatformBackend {
    fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
        containment_support(request)
    }

    fn runtime_filesystem_additions(
        &self,
        request: &ExecutionRequest,
    ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
        let mut paths = vec![
            "/",
            "/System/Library",
            "/usr/lib",
            "/System/Volumes/Preboot/Cryptexes/OS",
            "/bin/sh",
            "/bin/bash",
            "/private/var/select/sh",
            "/etc/localtime",
            "/private/var/db/timezone",
            "/dev/null",
            "/dev/random",
            "/dev/urandom",
        ]
        .into_iter()
        .map(canonical_existing)
        .collect::<Result<Vec<_>, _>>()?;
        if let Some(runtime) = &request.trusted_node_runtime {
            paths.push(runtime.path.clone());
        }
        paths.sort();
        paths.dedup();
        RuntimeFilesystemAdditions::checked(paths, Vec::new())
    }

    fn bind_filesystem(
        &self,
        _request: &ExecutionRequest,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<FilesystemBindings, ExecutionError> {
        materialize_missing_write_directories(policy)?;
        FilesystemBindings::canonical_path(policy)
    }

    fn prepare<'a>(
        &'a self,
        request: &ExecutionRequest,
        preflight: &'a ValidatedPreflight,
    ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
        let profile = compile_profile(request, preflight).map_err(PreparationError::from)?;
        Ok(OwnedExecutionAttempt::new(
            preflight,
            Box::new(MacosLifecycle {
                request: request.clone(),
                preflight,
                profile,
                child: None,
                process_group: None,
                cleanup_attempted: false,
                cleanup_observed: false,
            }),
        ))
    }
}

pub(super) fn containment_support(request: &ExecutionRequest) -> ContainmentSupport {
    let requested = EnforcementDimensions::requested_by(request.policy());
    let identity = backend_identity();
    let unsupported = |reason: &str| {
        ContainmentSupport::unsupported(
            identity.clone(),
            "macos",
            reason,
            requested.clone(),
            EnforcementDimensions::none(),
            EnforcementDimensions::none(),
        )
    };
    if request.policy().assurance() != AssuranceLevel::Restricted {
        return unsupported("native macOS ManagedTree containment is unsupported");
    }
    let limits = request.policy().limits();
    if limits.timeout_seconds().is_some()
        || limits.max_output_bytes().is_some()
        || limits.max_processes().is_some()
        || limits.max_memory_bytes().is_some()
    {
        return unsupported(
            "native macOS Restricted cannot enforce configured timeout, output, process-count, or memory limits",
        );
    }
    if let Err(reason) = support_probes() {
        return unsupported(reason);
    }
    let evidence = evidence_for_dimensions(
        &requested,
        "macOS Seatbelt behavioral positive/negative probes plus explicit launch setup",
        LIMITATIONS,
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

fn backend_identity() -> BackendIdentity {
    static VERSION: OnceLock<String> = OnceLock::new();
    let version = VERSION.get_or_init(|| {
        let product = command_text("/usr/bin/sw_vers", &["-productVersion"])
            .unwrap_or_else(|| "unknown".to_owned());
        let build = command_text("/usr/bin/sw_vers", &["-buildVersion"])
            .unwrap_or_else(|| "unknown".to_owned());
        format!(
            "{}; macOS {product} build {build}",
            env!("CARGO_PKG_VERSION")
        )
    });
    BackendIdentity::new(
        "tapid-runner/macos-seatbelt-restricted-experimental",
        version.clone(),
        Some("deprecated sandbox-exec backend; experimental and not a ManagedTree boundary".into()),
    )
    .expect("static backend identity and bounded sw_vers output must be valid")
}

fn command_text(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).env_clear().output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty() && value.len() <= 80 && !value.chars().any(char::is_control))
        .then_some(value)
}

fn support_probes() -> Result<(), &'static str> {
    static RESULT: OnceLock<Result<(), &'static str>> = OnceLock::new();
    *RESULT.get_or_init(run_support_probes)
}

fn run_support_probes() -> Result<(), &'static str> {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err("deprecated /usr/bin/sandbox-exec is unavailable");
    }
    let root = std::env::temp_dir().join(format!(
        "tapid-seatbelt-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable for Seatbelt probe")?
            .as_nanos()
    ));
    fs::create_dir(&root).map_err(|_| "cannot create trusted Seatbelt probe directory")?;
    let root = fs::canonicalize(&root)
        .map_err(|_| "cannot canonicalize trusted Seatbelt probe directory")?;
    let marker = root.join("denied-marker");
    let marker_text = marker.to_string_lossy();
    let deny_write = format!(
        "(version 1)(allow default)(deny file-write* (literal \"{}\"))",
        escape_sbpl_literal(&marker_text)
    );
    let write_denied = run_probe(
        &deny_write,
        "/bin/sh",
        &["-c", "printf x > \"$1\"", "probe", &marker_text],
    );
    if write_denied || marker.exists() {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt negative filesystem probe did not deny write");
    }
    let descendant_denied = run_probe(
        &deny_write,
        "/bin/sh",
        &[
            "-c",
            "/bin/sh -c 'printf x > \"$1\"' child \"$1\"",
            "probe",
            &marker_text,
        ],
    );
    if descendant_denied || marker.exists() {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt descendant propagation probe did not deny write");
    }
    if !run_probe("(version 1)(allow default)", "/bin/sh", &["-c", "exit 0"]) {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt positive execution probe failed");
    }
    if run_probe(
        "(version 1)(allow default)(deny process-fork)",
        "/bin/sh",
        &["-c", "(/bin/true &) >/dev/null 2>&1"],
    ) {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt subprocess denial probe did not deny process creation");
    }
    if !run_probe(
        "(version 1)(allow default)(deny process-fork)",
        "/bin/sh",
        &["-c", "exec /usr/bin/true"],
    ) {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt positive exec-replacement probe failed");
    }
    let network_script = "require 'socket'; s=TCPServer.new('127.0.0.1',0); c=TCPSocket.new('127.0.0.1',s.addr[1]); a=s.accept; c.close; a.close; s.close";
    if !run_probe(
        "(version 1)(allow default)",
        "/usr/bin/ruby",
        &["--disable-gems", "-e", network_script],
    ) {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt positive network probe failed");
    }
    fs::write(&marker, b"probe").map_err(|_| "cannot create trusted Seatbelt read probe")?;
    let deny_read = format!(
        "(version 1)(allow default)(deny file-read-data (literal \"{}\"))",
        escape_sbpl_literal(&marker_text)
    );
    if run_probe(&deny_read, "/bin/cat", &[&marker_text]) {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt negative filesystem probe did not deny read");
    }
    let _ = fs::remove_file(&marker);
    if run_probe(
        "(version 1)(allow default)(deny network*)",
        "/usr/bin/ruby",
        &["--disable-gems", "-e", network_script],
    ) {
        let _ = fs::remove_dir_all(&root);
        return Err("Seatbelt negative network probe did not deny loopback bind");
    }
    if !run_environment_probe() {
        let _ = fs::remove_dir_all(&root);
        return Err("explicit environment and null-stdin probe failed");
    }
    if !run_descriptor_probe() {
        let _ = fs::remove_dir_all(&root);
        return Err("inherited descriptor closure probe failed");
    }
    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn run_probe(profile: &str, program: &str, args: &[&str]) -> bool {
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(profile)
        .arg(program)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_child(&mut command);
    command.status().is_ok_and(|status| status.success())
}

fn run_environment_probe() -> bool {
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .args([
            "-p",
            "(version 1)(allow default)",
            "/bin/sh",
            "-c",
            "test \"$TAPID_PROBE\" = expected && test -z \"${HOME+x}\" && test -z \"${TMPDIR+x}\" && ! read value",
        ])
        .env_clear()
        .env("TAPID_PROBE", "expected")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_child(&mut command);
    command.status().is_ok_and(|status| status.success())
}

fn run_descriptor_probe() -> bool {
    let Ok(file) = fs::File::open("/etc/hosts") else {
        return false;
    };
    const PROBE_FD: libc::c_int = 100;
    // SAFETY: the source fd is live; the duplicate is test-owned and closed below. Clearing
    // CLOEXEC deliberately proves that configure_child, rather than ambient fd flags, removes it.
    if unsafe { libc::dup2(file.as_raw_fd(), PROBE_FD) } != PROBE_FD
        || unsafe { libc::fcntl(PROBE_FD, libc::F_SETFD, 0) } != 0
    {
        // SAFETY: closing an absent or partially-created probe descriptor is harmless.
        unsafe { libc::close(PROBE_FD) };
        return false;
    }
    let passed = run_probe(
        "(version 1)(allow default)",
        "/usr/bin/ruby",
        &[
            "--disable-gems",
            "-e",
            "begin; IO.for_fd(100, autoclose: false); exit 9; rescue Errno::EBADF; exit 0; end",
        ],
    );
    // SAFETY: PROBE_FD is the owned duplicate created above.
    unsafe { libc::close(PROBE_FD) };
    passed
}

fn canonical_existing(path: &str) -> Result<PathBuf, ExecutionError> {
    fs::canonicalize(path).map_err(|error| {
        ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            format!("required macOS runtime path is unavailable ({path}): {error}"),
        )
    })
}

fn materialize_missing_write_directories(
    policy: &ResolvedSandboxPolicy,
) -> Result<(), ExecutionError> {
    for grant in &policy.write {
        if let super::GrantResolution::MissingWriteDirectory {
            canonical_ancestor,
            relative_target,
        } = &grant.resolution
        {
            let target = canonical_ancestor.join(relative_target);
            securely_create_relative_directories(canonical_ancestor, relative_target)?;
            let canonical = fs::canonicalize(&target).map_err(|error| {
                ExecutionError::new(
                    ExecutionErrorCategory::PolicyViolation,
                    format!(
                        "cannot re-resolve write grant {}: {error}",
                        target.display()
                    ),
                )
            })?;
            if canonical != target || !canonical.starts_with(&policy.project_root) {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::PolicyViolation,
                    "materialized write grant escaped the canonical project root",
                ));
            }
        }
    }
    Ok(())
}

fn securely_create_relative_directories(
    canonical_ancestor: &Path,
    relative_target: &Path,
) -> Result<(), ExecutionError> {
    let ancestor =
        std::ffi::CString::new(canonical_ancestor.as_os_str().as_bytes()).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "write grant ancestor contains NUL",
            )
        })?;
    // SAFETY: `ancestor` is live and NUL-terminated. The returned fd is immediately owned.
    let initial = unsafe {
        libc::open(
            ancestor.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if initial < 0 {
        return Err(materialize_error(
            canonical_ancestor,
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `initial` is a newly returned owned descriptor.
    let mut directory = unsafe { OwnedFd::from_raw_fd(initial) };
    let mut current = canonical_ancestor.to_path_buf();
    for component in relative_target.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "write grant materialization requires normal relative components",
            ));
        };
        current.push(name);
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "write grant component contains NUL",
            )
        })?;
        // SAFETY: the directory fd and component string are live. Existing components are
        // accepted, then independently opened with O_NOFOLLOW below.
        if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(materialize_error(&current, error));
            }
        }
        // SAFETY: live arguments; O_NOFOLLOW and O_DIRECTORY reject symlinks and non-directories.
        let next = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if next < 0 {
            return Err(materialize_error(&current, std::io::Error::last_os_error()));
        }
        // SAFETY: `next` is a newly returned owned descriptor.
        directory = unsafe { OwnedFd::from_raw_fd(next) };
    }
    Ok(())
}

fn materialize_error(path: &Path, error: std::io::Error) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::PolicyViolation,
        format!(
            "cannot securely materialize write grant {}: {error}",
            path.display()
        ),
    )
}

fn compile_profile(
    request: &ExecutionRequest,
    preflight: &ValidatedPreflight,
) -> Result<CompiledProfile, ExecutionError> {
    let receipts = preflight.bindings.receipt();
    let mut text = String::from(
        "(version 1)\n(deny default)\n(allow file-read-metadata (subpath \"/\"))\n(allow file-read-data (literal \"/\"))\n(allow process-exec)\n(allow signal (target self))\n(allow sysctl-read\n (sysctl-name \"hw.activecpu\")\n (sysctl-name \"hw.availcpu\")\n (sysctl-name \"hw.logicalcpu\")\n (sysctl-name \"hw.logicalcpu_max\")\n (sysctl-name \"hw.ncpu\")\n (sysctl-name \"hw.pagesize\")\n (sysctl-name \"hw.pagesize_compat\")\n (sysctl-name \"kern.argmax\")\n (sysctl-name \"kern.hostname\")\n (sysctl-name \"kern.osrelease\")\n (sysctl-name \"kern.osversion\"))\n",
    );
    if request.policy().subprocess() {
        text.push_str("(allow process-fork)\n");
    } else {
        text.push_str("(deny process-fork)\n");
    }
    if request.policy().network() {
        text.push_str("(allow network*)\n");
    }
    let mut parameters = Vec::with_capacity(receipts.grants().len());
    for (index, grant) in receipts.grants().iter().enumerate() {
        let key = format!("G{index}");
        let filter = if grant.path() == Path::new("/") {
            // The root runtime grant is exact: only root directory data (names), plus the separate
            // metadata rule above, is exposed. It must never become a root subtree data grant.
            "literal"
        } else {
            match grant.kind() {
                FilesystemGrantKind::ExactFile => "literal",
                FilesystemGrantKind::DirectorySubtree => "subpath",
            }
        };
        let operation = match grant.access() {
            FilesystemAccess::Read => "file-read*",
            FilesystemAccess::Write => "file-write*",
        };
        text.push_str(&format!(
            "(allow {operation} ({filter} (param \"{key}\")))\n"
        ));
        parameters.push((key, grant.path().as_os_str().to_owned()));
    }
    Ok(CompiledProfile { text, parameters })
}

struct CompiledProfile {
    text: String,
    parameters: Vec<(String, OsString)>,
}

struct MacosLifecycle<'a> {
    request: ExecutionRequest,
    preflight: &'a ValidatedPreflight,
    profile: CompiledProfile,
    child: Option<Child>,
    process_group: Option<i32>,
    cleanup_attempted: bool,
    cleanup_observed: bool,
}

impl ExecutionLifecycle for MacosLifecycle<'_> {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        let mut command = Command::new(SANDBOX_EXEC);
        for (key, value) in &self.profile.parameters {
            let mut definition = OsString::from("-D");
            definition.push(key);
            definition.push("=");
            definition.push(value);
            command.arg(definition);
        }
        command
            .arg("-p")
            .arg(&self.profile.text)
            .arg(&self.request.program)
            .args(&self.request.arguments)
            .current_dir(&self.preflight.policy.project_root)
            .env_clear()
            .envs(&self.preflight.child_environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_child(&mut command);
        let child = command.spawn().map_err(|error| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot start deprecated macOS Seatbelt launcher: {error}"),
            )
        })?;
        self.process_group = i32::try_from(child.id()).ok();
        self.child = Some(child);
        let stdout = self
            .child
            .as_mut()
            .expect("spawned child retained")
            .stdout
            .take()
            .ok_or_else(|| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "launcher stdout pipe is unavailable",
                )
            })?;
        let stderr = self
            .child
            .as_mut()
            .expect("spawned child retained")
            .stderr
            .take()
            .ok_or_else(|| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "launcher stderr pipe is unavailable",
                )
            })?;
        set_nonblocking(&stdout)?;
        set_nonblocking(&stderr)?;

        let stop = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let stdout_reader = spawn_reader(
            stdout,
            false,
            Arc::clone(&stop),
            Arc::clone(&overflow),
            true,
        );
        let stderr_reader =
            spawn_reader(stderr, true, Arc::clone(&stop), Arc::clone(&overflow), true);
        let status = loop {
            if overflow.load(Ordering::Acquire) {
                self.kill_group();
                stop.store(true, Ordering::Release);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::OutputLimit,
                    format!(
                        "internal output safety ceiling of {INTERNAL_OUTPUT_CEILING} bytes was exceeded"
                    ),
                ));
            }
            match self
                .child
                .as_mut()
                .expect("spawned child retained")
                .try_wait()
            {
                Ok(Some(status)) => break status,
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(error) => {
                    self.kill_group();
                    stop.store(true, Ordering::Release);
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(ExecutionError::new(
                        ExecutionErrorCategory::Internal,
                        format!("cannot supervise macOS Seatbelt process: {error}"),
                    ));
                }
            }
        };
        self.child = None;
        let deadline = Instant::now() + PIPE_SHUTDOWN_GRACE;
        while Instant::now() < deadline
            && !(stdout_reader.is_finished() && stderr_reader.is_finished())
        {
            thread::sleep(POLL_INTERVAL);
        }
        if !(stdout_reader.is_finished() && stderr_reader.is_finished()) {
            self.kill_group();
            stop.store(true, Ordering::Release);
        }
        let stdout = stdout_reader.join().map_err(|_| {
            ExecutionError::new(ExecutionErrorCategory::Internal, "stdout reader panicked")
        })??;
        let stderr = stderr_reader.join().map_err(|_| {
            ExecutionError::new(ExecutionErrorCategory::Internal, "stderr reader panicked")
        })??;
        let requested = self.preflight.support.requested().clone();
        let established = evidence_for_dimensions(
            &requested,
            "deny-default Seatbelt profile installed by sandbox-exec before target exec",
            LIMITATIONS,
        );
        let receipt = EnforcementReceipt::checked(self.preflight, requested, established)?;
        let completion = CompletionEvidence::checked(
            self.preflight,
            EnforcementDimensions::none(),
            Vec::new(),
            if self.cleanup_attempted && self.cleanup_observed {
                CleanupConfidence::BestEffortObserved
            } else {
                CleanupConfidence::NotGuaranteed
            },
        )?;
        let termination = if let Some(code) = status.code() {
            Termination::Exited(code)
        } else {
            Termination::Signaled(status.signal().unwrap_or(0))
        };
        Ok(Box::new(ExecutionOutcome::checked(
            termination,
            stdout,
            stderr,
            receipt,
            completion,
        )?))
    }

    fn cleanup(&mut self) -> CompletionEvidence {
        self.kill_group();
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            self.cleanup_observed |= child.wait().is_ok();
        }
        self.child = None;
        CompletionEvidence::checked(
            self.preflight,
            EnforcementDimensions::none(),
            Vec::new(),
            if self.cleanup_attempted && self.cleanup_observed {
                CleanupConfidence::BestEffortObserved
            } else {
                CleanupConfidence::NotGuaranteed
            },
        )
        .expect("Restricted completion has no required tree dimensions")
    }
}

impl MacosLifecycle<'_> {
    fn kill_group(&mut self) {
        let Some(group) = self.process_group else {
            return;
        };
        self.cleanup_attempted = true;
        // SAFETY: a negative PID addresses the fresh child process group. SIGKILL has no borrowed
        // pointers, and ESRCH simply means the best-effort group no longer exists.
        let result = unsafe { libc::kill(-group, libc::SIGKILL) };
        self.cleanup_observed |= result == 0;
    }
}

fn configure_child(command: &mut Command) {
    // SAFETY: this closure runs after std has installed descriptors 0/1/2. setpgid uses the child
    // PID (0), and the explicit close loop removes only unintended descriptors >=3; neither
    // operation captures Rust borrows.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            if libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = limit.assume_init().rlim_cur.min(libc::c_int::MAX as u64) as libc::c_int;
            for fd in 3..limit {
                libc::close(fd);
            }
            Ok(())
        });
    }
}

fn set_nonblocking<T: std::os::fd::AsRawFd>(value: &T) -> Result<(), ExecutionError> {
    let fd = value.as_raw_fd();
    // SAFETY: fd is owned and live for both fcntl calls; no pointer arguments are involved.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::Internal,
            format!(
                "cannot make output pipe nonblocking: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    stderr: bool,
    stop: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    live_output: bool,
) -> thread::JoinHandle<Result<Vec<u8>, ExecutionError>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if captured.len().saturating_add(count) > INTERNAL_OUTPUT_CEILING {
                        overflow.store(true, Ordering::Release);
                        break;
                    }
                    captured.extend_from_slice(&chunk[..count]);
                    if live_output {
                        let result = if stderr {
                            std::io::stderr().write_all(&chunk[..count])
                        } else {
                            std::io::stdout().write_all(&chunk[..count])
                        };
                        result.map_err(|error| {
                            ExecutionError::new(
                                ExecutionErrorCategory::Internal,
                                format!("cannot stream child output: {error}"),
                            )
                        })?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => {
                    return Err(ExecutionError::new(
                        ExecutionErrorCategory::Internal,
                        format!("cannot read child output: {error}"),
                    ));
                }
            }
        }
        Ok(captured)
    })
}

fn escape_sbpl_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy};
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;

    fn temp_project(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tapid-seatbelt-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    fn policy(network: bool, subprocess: bool, environment: Vec<String>) -> SandboxPolicy {
        SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::Restricted,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            network,
            environment,
            subprocess,
            ExecutionLimits::default(),
        )
        .unwrap()
    }

    fn run_ruby(root: &Path, script: &str, network: bool, subprocess: bool) -> ExecutionOutcome {
        let request = ExecutionRequest::builder("/usr/bin/ruby")
            .args(["--disable-gems", "-e", script])
            .project_root(root)
            .policy(policy(network, subprocess, Vec::new()))
            .executable_search_path("/usr/bin")
            .build()
            .unwrap();
        super::super::execute(&request).unwrap()
    }

    #[test]
    fn sbpl_literal_escape_is_only_used_for_trusted_probe_paths() {
        assert_eq!(escape_sbpl_literal("a\\b\"c"), "a\\\\b\\\"c");
    }

    #[test]
    fn output_reader_sets_overflow_without_growing_past_internal_ceiling() {
        let stop = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let input = std::io::Cursor::new(vec![b'x'; INTERNAL_OUTPUT_CEILING + 1]);
        let reader = spawn_reader(input, false, stop, Arc::clone(&overflow), false);
        let captured = reader.join().unwrap().unwrap();
        assert!(overflow.load(Ordering::Acquire));
        assert!(captured.len() <= INTERNAL_OUTPUT_CEILING);
    }

    #[test]
    fn malformed_and_unavailable_sandbox_launchers_fail_before_target_marker() {
        let root = temp_project("launcher-failure");
        let marker = root.join("untrusted-marker");
        let marker_text = marker.to_string_lossy();
        let malformed = Command::new(SANDBOX_EXEC)
            .args([
                "-p",
                "(version 1)(this-operation-does-not-exist)",
                "/bin/sh",
                "-c",
                "printf spawned > \"$1\"",
                "target",
                &marker_text,
            ])
            .env_clear()
            .status()
            .unwrap();
        assert!(!malformed.success());
        assert!(!marker.exists());
        let unavailable = Command::new("/definitely/unavailable/sandbox-exec")
            .args([
                "/bin/sh",
                "-c",
                "printf spawned > \"$1\"",
                "target",
                &marker_text,
            ])
            .env_clear()
            .status();
        assert!(unavailable.is_err());
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn securely_materializing_a_write_grant_rejects_a_symlink_component() {
        use std::os::unix::fs::symlink;
        let root = temp_project("materialize");
        let outside = temp_project("materialize-outside");
        symlink(&outside, root.join("link")).unwrap();
        let error = securely_create_relative_directories(&root, Path::new("link/child"))
            .expect_err("O_NOFOLLOW traversal must reject symlinks");
        assert_eq!(error.category(), ExecutionErrorCategory::PolicyViolation);
        assert!(!outside.join("child").exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn network_false_denies_tcp_bind_and_connect_unix_sockets_dns_and_reserved_connect() {
        let root = temp_project("network-deny");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let unix_path = PathBuf::from(format!(
            "/private/tmp/tapid-net-{}.sock",
            std::process::id()
        ));
        let _ = fs::remove_file(&unix_path);
        let scripts = [
            "require 'socket'; TCPServer.new('127.0.0.1', 0)".to_owned(),
            format!("require 'socket'; TCPSocket.new('127.0.0.1', {port})"),
            format!("require 'socket'; UNIXServer.new({unix_path:?})"),
            "require 'socket'; s=UDPSocket.new; s.connect('127.0.0.1',53)".to_owned(),
            "require 'socket'; s=Socket.new(:INET,:STREAM); begin; s.connect_nonblock(Socket.sockaddr_in(9,'192.0.2.1')); rescue IO::WaitWritable; exit 0; end".to_owned(),
        ];
        for script in scripts {
            let outcome = run_ruby(&root, &script, false, true);
            assert_ne!(outcome.termination(), &Termination::Exited(0), "{script}");
        }
        assert!(!unix_path.exists());
        drop(listener);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subprocess_false_denies_fork_but_allows_exec_replacement() {
        let root = temp_project("subprocess");
        let denied = run_ruby(
            &root,
            "begin; Process.fork { exit 0 }; Process.wait; exit 9; rescue SystemCallError; exit 0; end",
            false,
            false,
        );
        assert_eq!(denied.termination(), &Termination::Exited(0));
        let request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", "exec /bin/sh -c 'exit 23'"])
            .project_root(&root)
            .policy(policy(false, false, Vec::new()))
            .executable_search_path("/usr/bin")
            .build()
            .unwrap();
        let replaced = super::super::execute(&request).unwrap();
        assert_eq!(replaced.termination(), &Termination::Exited(23));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_gets_only_validated_environment_null_stdin_and_fidelity_preserved_arguments() {
        let root = temp_project("environment");
        let hostile = OsString::from("space ' quote \" dollar $ semicolon ;");
        let request = ExecutionRequest::builder("/usr/bin/ruby")
            .args([
                OsString::from("--disable-gems"),
                OsString::from("-e"),
                OsString::from("abort unless STDIN.read.empty?; print [ENV.to_h, ARGV].inspect"),
                hostile.clone(),
            ])
            .project_root(&root)
            .policy(policy(false, true, vec!["TAPID_SENTINEL".into()]))
            .executable_search_path("/usr/bin")
            .env("TAPID_SENTINEL", "present")
            .build()
            .unwrap();
        let outcome = super::super::execute(&request).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        let output = String::from_utf8(outcome.stdout().to_vec()).unwrap();
        assert!(output.contains("TAPID_SENTINEL"));
        assert!(output.contains("PATH"));
        for forbidden in [
            "HOME",
            "TMPDIR",
            "SSH_AUTH_SOCK",
            "HTTP_PROXY",
            "DYLD_INSERT_LIBRARIES",
        ] {
            assert!(!output.contains(forbidden), "ambient {forbidden} leaked");
        }
        assert!(output.contains(&format!("{:?}", hostile.to_str().unwrap())));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_regular_connected_and_listening_descriptors_are_closed() {
        let root = temp_project("fds");
        let file = fs::File::open("/etc/hosts").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(peer_listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = peer_listener.accept().unwrap();
        let sources = [file.as_raw_fd(), stream.as_raw_fd(), listener.as_raw_fd()];
        let fds = [100, 101, 102];
        for (source, fd) in sources.into_iter().zip(fds) {
            // SAFETY: each source fd is live; high-number duplicates create adversarial inherited
            // authorities without colliding with std's child setup descriptors.
            assert_eq!(unsafe { libc::dup2(source, fd) }, fd);
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        }
        let script = format!(
            "fds={fds:?}; exit(fds.all? {{ |fd| begin; IO.for_fd(fd, autoclose: false); false; rescue Errno::EBADF; true; end }} ? 0 : 9)"
        );
        let outcome = run_ruby(&root, &script, true, true);
        for fd in fds {
            // SAFETY: these are the test-owned duplicates created above.
            unsafe { libc::close(fd) };
        }
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_authority_does_not_cross_fork_exec_setsid_or_double_fork() {
        let root = temp_project("propagation");
        let outside = root.with_extension("outside-marker");
        let outside_text = outside.to_string_lossy();
        let scripts = [
            format!("Process.fork {{ File.write({outside_text:?}, 'x') }}; Process.wait"),
            format!(
                r#"Process.fork {{ exec('/bin/sh','-c', 'printf x > "$1"', 'child', {outside_text:?}) }}; Process.wait"#
            ),
            format!(
                "Process.fork {{ Process.setsid; File.write({outside_text:?}, 'x') }}; Process.wait"
            ),
            format!(
                "Process.fork {{ Process.fork {{ File.write({outside_text:?}, 'x') }}; Process.wait }}; Process.wait"
            ),
        ];
        for script in scripts {
            let _ = run_ruby(&root, &script, false, true);
            assert!(!outside.exists(), "descendant escaped Seatbelt: {script}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_tree_and_every_resource_limit_fail_closed() {
        let root = temp_project("unsupported");
        let build = |assurance, limits| {
            let configured = SandboxPolicy::new_with_assurance(
                SandboxMode::Required,
                assurance,
                FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
                false,
                Vec::new(),
                true,
                limits,
            )
            .unwrap();
            ExecutionRequest::builder("/bin/sh")
                .args(["-c", "exit 0"])
                .project_root(&root)
                .policy(configured)
                .executable_search_path("/usr/bin")
                .build()
                .unwrap()
        };
        assert!(
            !containment_support(&build(
                AssuranceLevel::ManagedTree,
                ExecutionLimits::default()
            ))
            .is_supported()
        );
        for limits in [
            ExecutionLimits::new(Some(1), None, None, None).unwrap(),
            ExecutionLimits::new(None, Some(1), None, None).unwrap(),
            ExecutionLimits::new(None, None, Some(1), None).unwrap(),
            ExecutionLimits::new(None, None, None, Some(1)).unwrap(),
        ] {
            assert!(
                !containment_support(&build(AssuranceLevel::Restricted, limits)).is_supported()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn path_alias_symlink_rename_hardlink_and_hostile_parameter_inputs_do_not_escape() {
        let root = temp_project("hostile-\")-(allow-default)-");
        let outside = root.with_extension("outside");
        fs::write(&outside, b"outside").unwrap();
        let alias = root.to_string_lossy().replacen("/private/var/", "/var/", 1);
        let request = ExecutionRequest::builder("/usr/bin/ruby")
            .args([
                "--disable-gems",
                "-e",
                "outside=ARGV[0]; begin; File.symlink(outside,'sym'); File.write('sym','bad'); rescue SystemCallError; end; begin; File.rename(outside,'renamed'); rescue SystemCallError; end; begin; File.link(outside,'hard'); rescue SystemCallError; end; File.write('allowed','ok')",
                outside.to_str().unwrap(),
            ])
            .project_root(&alias)
            .policy(policy(false, true, Vec::new()))
            .executable_search_path("/usr/bin")
            .build()
            .unwrap();
        let outcome = super::super::execute(&request).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert!(!root.join("renamed").exists());
        assert!(!root.join("hard").exists());
        assert_eq!(fs::read(root.join("allowed")).unwrap(), b"ok");
        assert!(
            outcome
                .enforcement()
                .resolved_filesystem()
                .grants()
                .iter()
                .all(|grant| !grant.path().to_string_lossy().starts_with("/var/"))
        );
        let _ = fs::remove_file(root.join("sym"));
        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn held_output_pipe_is_bounded_and_does_not_hang_completion() {
        let root = temp_project("held-pipe");
        let started = Instant::now();
        let outcome = run_ruby(
            &root,
            "Process.fork { sleep 30 }; puts 'parent-exit'",
            false,
            true,
        );
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            outcome.completion().cleanup_confidence(),
            CleanupConfidence::BestEffortObserved
        );
        fs::remove_dir_all(root).unwrap();
    }
}
