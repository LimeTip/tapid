//! Experimental macOS Restricted backend using deprecated/private native Seatbelt APIs.

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
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const INTERNAL_OUTPUT_CEILING: usize = 16 * 1024 * 1024;
const PIPE_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);
const LIVE_SINK_BACKPRESSURE_GRACE: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const LIMITATIONS: &[&str] = &[
    "sampled positive/negative syscall controls are not an exhaustive proof of Seatbelt semantics",
    "experimental backend uses deprecated/private native Seatbelt APIs and path-based CanonicalPath grants",
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
        if let Some(helper) = request
            .launcher
            .as_ref()
            .and_then(|t| t.executable.as_ref())
        {
            paths.push(helper.clone());
        }
        if let Some(binding) = &request.reserved_node {
            paths.push(binding.directory.clone());
            // This is the selected runtime inode, even if its original pathname is gone.
            paths.push(binding.directory.join("node"));
        } else if let Some(runtime) = &request.trusted_node_runtime {
            paths.push(runtime.path.clone());
        }
        paths.sort();
        paths.dedup();
        let mut additions = RuntimeFilesystemAdditions::checked(paths, Vec::new())?;
        let root = additions
            .read
            .iter_mut()
            .find(|grant| grant.path == Path::new("/"))
            .expect("runtime root exists");
        root.access = FilesystemAccess::ReadData;
        root.kind = FilesystemGrantKind::ExactDirectory;
        let mut metadata = root.clone();
        metadata.access = FilesystemAccess::ReadMetadata;
        metadata.kind = FilesystemGrantKind::DirectorySubtree;
        additions.read.push(metadata);
        Ok(additions)
    }

    fn bind_filesystem(
        &self,
        request: &ExecutionRequest,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<FilesystemBindings, ExecutionError> {
        if let Some(binding) = &request.reserved_node {
            binding.validate_write_authority(policy)?;
        }
        materialize_missing_write_directories(policy)?;
        FilesystemBindings::canonical_path(policy)
    }

    fn prepare<'a>(
        &'a self,
        request: &ExecutionRequest,
        preflight: &'a ValidatedPreflight,
    ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
        let profile = compile_profile(request, preflight).map_err(PreparationError::from)?;
        let launch =
            prepare_launch(request, preflight, &profile).map_err(PreparationError::from)?;
        Ok(OwnedExecutionAttempt::new(
            preflight,
            Box::new(MacosLifecycle {
                request: request.clone(),
                preflight,
                launch: Some(launch),
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
    if request
        .launcher
        .as_ref()
        .and_then(|token| token.executable.as_ref())
        .is_none()
    {
        return unsupported(
            "current-executable private launcher was not initialized as the first operation in main",
        );
    }
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
        "native generated deny-default Seatbelt sampled syscall controls plus explicit exec setup",
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
        Some("deprecated/private native Seatbelt backend; experimental and not a ManagedTree boundary".into()),
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
    let root = std::env::temp_dir().join(format!(
        "tapid-native-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "probe clock unavailable")?
            .as_nanos()
    ));
    fs::create_dir(&root).map_err(|_| "cannot create native probe directory")?;
    let result = native_controls(&root);
    let _ = fs::remove_dir_all(root);
    result
}

fn probe_profile(root: &Path, permissive: bool) -> Result<CompiledProfile, &'static str> {
    use crate::config::{ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy};
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(
            vec![if permissive { "." } else { "allowed" }.into()],
            vec![if permissive { "." } else { "allowed" }.into()],
        )
        .map_err(|_| "probe filesystem policy invalid")?,
        permissive,
        Vec::new(),
        permissive,
        ExecutionLimits::default(),
    )
    .map_err(|_| "probe policy invalid")?;
    let request = ExecutionRequest::builder("/bin/sh")
        .project_root(root)
        .policy(policy)
        .build()
        .map_err(|_| "probe request invalid")?;
    let additions = PlatformBackend
        .runtime_filesystem_additions(&request)
        .map_err(|_| "probe runtime unavailable")?;
    let policy =
        super::resolve_policy(&request, additions).map_err(|_| "probe resolution failed")?;
    let bindings =
        FilesystemBindings::canonical_path(&policy).map_err(|_| "probe binding failed")?;
    // Compilation needs bindings, not a claim of support. Avoid recursively invoking support.
    let support = ContainmentSupport::unsupported(
        backend_identity(),
        "macos",
        "probe in progress",
        EnforcementDimensions::requested_by(request.policy()),
        EnforcementDimensions::none(),
        EnforcementDimensions::none(),
    );
    compile_profile(
        &request,
        &ValidatedPreflight {
            support,
            policy,
            bindings,
            child_environment: request.child_environment(),
        },
    )
    .map_err(|_| "probe compilation failed")
}

#[derive(Clone, Copy, Debug)]
enum NativeOperation {
    Read,
    Write,
    ForkWrite,
    Fork,
    TcpBind,
    TcpConnect(u16),
    UnixBind,
    UdpConnect,
    ReservedConnect,
}

// This routine runs after fork. All memory is prepared in the parent, and only libc
// syscalls, stack values, and _exit are used. Return errno, not an arbitrary failure code.
unsafe fn perform_probe(operation: NativeOperation, path: &std::ffi::CStr) -> i32 {
    unsafe {
        let result = match operation {
            NativeOperation::Read | NativeOperation::Write => {
                let flags = if matches!(operation, NativeOperation::Read) {
                    libc::O_RDONLY
                } else {
                    libc::O_WRONLY | libc::O_CREAT
                };
                let fd = libc::open(path.as_ptr(), flags, 0o600);
                if fd >= 0 {
                    libc::close(fd);
                    0
                } else {
                    -1
                }
            }
            NativeOperation::Fork | NativeOperation::ForkWrite => {
                let pid = libc::fork();
                if pid == 0 {
                    let code = if matches!(operation, NativeOperation::ForkWrite) {
                        perform_probe(NativeOperation::Write, path)
                    } else {
                        0
                    };
                    libc::_exit(code);
                }
                if pid < 0 {
                    -1
                } else {
                    let mut status = 0;
                    if libc::waitpid(pid, &mut status, 0) != pid {
                        return libc::EIO;
                    }
                    return if libc::WIFEXITED(status) {
                        libc::WEXITSTATUS(status)
                    } else {
                        libc::EIO
                    };
                }
            }
            NativeOperation::UnixBind => {
                let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
                if fd < 0 {
                    return *libc::__error();
                }
                let mut address: libc::sockaddr_un = std::mem::zeroed();
                address.sun_family = libc::AF_UNIX as u8;
                let bytes = path.to_bytes_with_nul();
                if bytes.len() > address.sun_path.len() {
                    libc::close(fd);
                    return libc::ENAMETOOLONG;
                }
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    address.sun_path.as_mut_ptr().cast(),
                    bytes.len(),
                );
                address.sun_len = std::mem::size_of_val(&address) as u8;
                let rc = libc::bind(
                    fd,
                    (&address as *const libc::sockaddr_un).cast(),
                    std::mem::size_of_val(&address) as u32,
                );
                let error = if rc == 0 { 0 } else { *libc::__error() };
                libc::close(fd);
                return error;
            }
            _ => {
                let udp = matches!(operation, NativeOperation::UdpConnect);
                let fd = libc::socket(
                    libc::AF_INET,
                    if udp {
                        libc::SOCK_DGRAM
                    } else {
                        libc::SOCK_STREAM
                    },
                    0,
                );
                if fd < 0 {
                    return *libc::__error();
                }
                let mut address: libc::sockaddr_in = std::mem::zeroed();
                address.sin_len = std::mem::size_of_val(&address) as u8;
                address.sin_family = libc::AF_INET as u8;
                address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
                address.sin_port = match operation {
                    NativeOperation::TcpConnect(port) => port,
                    NativeOperation::UdpConnect => 53,
                    NativeOperation::ReservedConnect => 9,
                    _ => 0,
                }
                .to_be();
                if matches!(operation, NativeOperation::ReservedConnect) {
                    address.sin_addr.s_addr = u32::from_ne_bytes([192, 0, 2, 1]);
                    libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
                }
                let rc = if matches!(operation, NativeOperation::TcpBind) {
                    libc::bind(
                        fd,
                        (&address as *const libc::sockaddr_in).cast(),
                        std::mem::size_of_val(&address) as u32,
                    )
                } else {
                    libc::connect(
                        fd,
                        (&address as *const libc::sockaddr_in).cast(),
                        std::mem::size_of_val(&address) as u32,
                    )
                };
                let error = if rc == 0 { 0 } else { *libc::__error() };
                libc::close(fd);
                return if matches!(operation, NativeOperation::ReservedConnect)
                    && error == libc::EINPROGRESS
                {
                    0
                } else {
                    error
                };
            }
        };
        if result == 0 { 0 } else { *libc::__error() }
    }
}

fn native_probe(
    profile: &CompiledProfile,
    operation: NativeOperation,
    path: &Path,
) -> Result<i32, &'static str> {
    let bytes = profile
        .bytecode()
        .map_err(|_| "native probe profile rejected")?;
    let path =
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| "invalid probe path")?;
    // SAFETY: fork child runs only the syscall routine above and exits without unwinding.
    // The parent waits for its exact child. No target-controlled code participates.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err("native probe fork failed");
        }
        if pid == 0 {
            let mut profile = NativeProfile {
                builtin: std::ptr::null_mut(),
                data: bytes.as_ptr(),
                size: bytes.len(),
            };
            if sandbox_apply(&mut profile) != 0 {
                libc::_exit(254);
            }
            libc::_exit(perform_probe(operation, &path));
        }
        let mut status = 0;
        if libc::waitpid(pid, &mut status, 0) != pid || !libc::WIFEXITED(status) {
            return Err("native probe did not complete");
        }
        let code = libc::WEXITSTATUS(status);
        if code == 254 {
            return Err("native probe profile installation failed");
        }
        Ok(code)
    }
}

fn native_controls(root: &Path) -> Result<(), &'static str> {
    fs::create_dir(root.join("allowed")).map_err(|_| "cannot create probe grant")?;
    let marker = root.join("marker");
    fs::write(&marker, b"probe").map_err(|_| "cannot create probe marker")?;
    let allow = probe_profile(root, true)?;
    let deny = probe_profile(root, false)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|_| "TCP positive listener unavailable")?;
    let port = listener
        .local_addr()
        .map_err(|_| "TCP probe address unavailable")?
        .port();
    // A short socket path is essential on Darwin. Both profiles explicitly allow its parent
    // filesystem directory so the negative result tests network authority, not file writes.
    let socket_dir = PathBuf::from(format!(
        "/private/tmp/tapid-socket-probe-{}",
        std::process::id()
    ));
    fs::create_dir(&socket_dir).map_err(|_| "cannot create socket probe directory")?;
    let socket = socket_dir.join("s");
    let result = (|| {
        for operation in [
            NativeOperation::Read,
            NativeOperation::Write,
            NativeOperation::Fork,
            NativeOperation::TcpBind,
            NativeOperation::TcpConnect(port),
            NativeOperation::UdpConnect,
            NativeOperation::ReservedConnect,
        ] {
            if native_probe(&allow, operation, &marker)? != 0 {
                return Err("native per-operation positive control failed");
            }
            if !matches!(
                native_probe(&deny, operation, &marker)?,
                libc::EPERM | libc::EACCES
            ) {
                return Err("native per-operation negative control failed");
            }
        }
        let mut inherited = probe_profile(root, false)?;
        inherited.text = inherited
            .text
            .replace("(deny process-fork)", "(allow process-fork)");
        if native_probe(&allow, NativeOperation::ForkWrite, &marker)? != 0
            || !matches!(
                native_probe(&inherited, NativeOperation::ForkWrite, &marker)?,
                libc::EPERM | libc::EACCES
            )
        {
            return Err("native descendant write control failed");
        }
        let socket_allow = probe_profile(&socket_dir, true)?;
        let mut socket_deny = probe_profile(&socket_dir, true)?;
        socket_deny.text = socket_deny.text.replace("(allow network*)", "");
        // Keep both profiles otherwise identical, including generated filesystem rules.
        if native_probe(&socket_allow, NativeOperation::UnixBind, &socket)? != 0 {
            return Err("Unix socket positive control failed");
        }
        fs::remove_file(&socket).map_err(|_| "cannot reset socket control")?;
        if !matches!(
            native_probe(&socket_deny, NativeOperation::UnixBind, &socket)?,
            libc::EPERM | libc::EACCES
        ) {
            return Err("Unix socket negative control failed");
        }
        let mut exec = Command::new("/bin/sh");
        exec.args(["-c", "test \"$TAPID_PROBE\" = expected && test -z \"${HOME+x}\" && test -z \"${TMPDIR+x}\" && ! read value"])
            .env_clear().env("TAPID_PROBE", "expected").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        socket_allow
            .configure(&mut exec)
            .map_err(|_| "environment control setup failed")?;
        if !exec.status().is_ok_and(|status| status.success()) {
            return Err("environment and stdin control failed");
        }
        let mut replace = Command::new("/bin/sh");
        replace
            .args(["-c", "exec /bin/sh -c 'exit 0'"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        deny.configure(&mut replace)
            .map_err(|_| "exec replacement setup failed")?;
        if !replace.status().is_ok_and(|status| status.success()) {
            return Err("exec replacement positive control failed");
        }
        descriptor_controls(&allow, &marker)?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(socket_dir);
    result
}

fn descriptor_controls(profile: &CompiledProfile, marker: &Path) -> Result<(), &'static str> {
    let file = fs::File::open(marker).map_err(|_| "descriptor marker unavailable")?;
    // SAFETY: create a uniquely owned duplicate; avoid fixed descriptor numbers shared by tests.
    let raw = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
    if raw < 0 {
        return Err("descriptor control duplication failed");
    }
    // SAFETY: raw is a new owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let script = format!("test -e /dev/fd/{}", fd.as_raw_fd());
    for preserve in [true, false] {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        profile
            .configure(&mut command)
            .map_err(|_| "descriptor control setup failed")?;
        if preserve {
            let raw = fd.as_raw_fd();
            // SAFETY: the owned descriptor stays live through spawn. Only this positive control
            // deliberately makes it inheritable after the production descriptor setup.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(raw, libc::F_SETFD, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let status = command
            .status()
            .map_err(|_| "descriptor control exec failed")?;
        if status.success() != preserve {
            return Err("descriptor positive/negative control failed");
        }
    }
    Ok(())
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
        "(version 1)\n(deny default)\n(allow process-exec)\n(allow signal (target self))\n(allow sysctl-read\n (sysctl-name \"hw.activecpu\")\n (sysctl-name \"hw.availcpu\")\n (sysctl-name \"hw.logicalcpu\")\n (sysctl-name \"hw.logicalcpu_max\")\n (sysctl-name \"hw.ncpu\")\n (sysctl-name \"hw.pagesize\")\n (sysctl-name \"hw.pagesize_compat\")\n (sysctl-name \"kern.argmax\")\n (sysctl-name \"kern.hostname\")\n (sysctl-name \"kern.osrelease\")\n (sysctl-name \"kern.osversion\"))\n",
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
        let filter = match grant.kind() {
            FilesystemGrantKind::ExactFile
            | FilesystemGrantKind::CharacterDevice
            | FilesystemGrantKind::ExactDirectory => "literal",
            FilesystemGrantKind::DirectorySubtree => "subpath",
        };
        let operation = match grant.access() {
            FilesystemAccess::Read => "file-read*",
            FilesystemAccess::ReadData => "file-read-data",
            FilesystemAccess::ReadMetadata => "file-read-metadata",
            FilesystemAccess::Write => "file-write*",
        };
        text.push_str(&format!(
            "(allow {operation} ({filter} (param \"{key}\")))\n"
        ));
        parameters.push((key, grant.path().as_os_str().to_owned()));
    }
    if let Some(binding) = &request.reserved_node {
        text.push_str("(deny file-link)\n(deny file-write* (subpath (param \"N0\")))\n");
        parameters.push(("N0".into(), binding.directory.as_os_str().to_owned()));
    }
    let helper = fs::canonicalize(std::env::current_exe().map_err(|_| ExecConfirmation::error())?)
        .map_err(|_| ExecConfirmation::error())?;
    text.push_str("(allow file-read* (literal (param \"H0\")))\n");
    parameters.push(("H0".into(), helper.into_os_string()));
    Ok(CompiledProfile {
        text,
        parameters,
        #[cfg(test)]
        abort_before_exec: false,
    })
}

struct CompiledProfile {
    #[cfg(test)]
    abort_before_exec: bool,
    text: String,
    parameters: Vec<(String, OsString)>,
}

// The private Seatbelt ABI is also used by Chromium/WebKit. Compile in the parent;
// the post-fork child only applies immutable bytecode, without parsing or allocation.
#[repr(C)]
struct NativeProfile {
    builtin: *mut libc::c_char,
    data: *const u8,
    size: usize,
}

#[link(name = "sandbox")]
unsafe extern "C" {
    fn sandbox_create_params() -> *mut libc::c_void;
    fn sandbox_set_param(
        params: *mut libc::c_void,
        key: *const libc::c_char,
        value: *const libc::c_char,
    ) -> libc::c_int;
    fn sandbox_free_params(params: *mut libc::c_void);
    fn sandbox_compile_string(
        text: *const libc::c_char,
        params: *mut libc::c_void,
        error: *mut *mut libc::c_char,
    ) -> *mut NativeProfile;
    fn sandbox_free_profile(profile: *mut NativeProfile);
    fn sandbox_free_error(error: *mut libc::c_char);
    fn sandbox_apply(profile: *mut NativeProfile) -> libc::c_int;
}

impl CompiledProfile {
    fn bytecode(&self) -> Result<Vec<u8>, ExecutionError> {
        let invalid = || {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                "cannot compile native Seatbelt profile",
            )
        };
        let text = std::ffi::CString::new(self.text.as_bytes()).map_err(|_| invalid())?;
        let parameters = self
            .parameters
            .iter()
            .map(|(key, value)| {
                Ok((
                    std::ffi::CString::new(key.as_bytes()).map_err(|_| invalid())?,
                    std::ffi::CString::new(value.as_bytes()).map_err(|_| invalid())?,
                ))
            })
            .collect::<Result<Vec<_>, ExecutionError>>()?;
        // SAFETY: C strings stay live through compilation. All native allocations are freed
        // in this parent process, including every error path.
        unsafe {
            let params = sandbox_create_params();
            if params.is_null() {
                return Err(invalid());
            }
            for (key, value) in &parameters {
                if sandbox_set_param(params, key.as_ptr(), value.as_ptr()) != 0 {
                    sandbox_free_params(params);
                    return Err(invalid());
                }
            }
            let mut error = std::ptr::null_mut();
            let profile = sandbox_compile_string(text.as_ptr(), params, &mut error);
            sandbox_free_params(params);
            if !error.is_null() {
                sandbox_free_error(error);
            }
            if profile.is_null() {
                return Err(invalid());
            }
            let bytes = std::slice::from_raw_parts((*profile).data, (*profile).size).to_vec();
            sandbox_free_profile(profile);
            Ok(bytes)
        }
    }

    fn configure(&self, command: &mut Command) -> Result<(), ExecutionError> {
        let bytecode = self.bytecode()?;
        configure_child(command);
        // SAFETY: immutable owned bytecode is prepared before fork. sandbox_apply installs
        // compiled kernel policy; no Rust allocation, locks, or destructors run in the child.
        // std's private CLOEXEC error pipe reports apply/exec errors and closes on target exec.
        unsafe {
            command.pre_exec(move || {
                let mut profile = NativeProfile {
                    builtin: std::ptr::null_mut(),
                    data: bytecode.as_ptr(),
                    size: bytecode.len(),
                };
                if sandbox_apply(&mut profile) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

const PRIVATE_MARKER: &str = "--tapid-private-launcher-v1";
const FRAME_SIZE: usize = 32;

fn protocol_frame(kind: u32, nonce: [u8; 16], errno: i32) -> [u8; FRAME_SIZE] {
    let mut frame = [0; FRAME_SIZE];
    frame[..4].copy_from_slice(&1_u32.to_be_bytes());
    frame[4..8].copy_from_slice(&kind.to_be_bytes());
    frame[8..24].copy_from_slice(&nonce);
    frame[24..28].copy_from_slice(&errno.to_be_bytes());
    frame
}

fn validate_frame(bytes: &[u8], kind: u32, nonce: [u8; 16]) -> Result<i32, ExecutionError> {
    if bytes.len() != FRAME_SIZE {
        return Err(ExecConfirmation::error());
    }
    let errno = i32::from_be_bytes(bytes[24..28].try_into().unwrap());
    if bytes != protocol_frame(kind, nonce, errno) || (kind != 3 && errno != 0) {
        return Err(ExecConfirmation::error());
    }
    Ok(errno)
}

fn read_protocol(file: &mut fs::File, bytes: &mut [u8]) -> Result<(), ExecutionError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        let mut poll = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live descriptor and initialized poll entry.
        if remaining == 0 || unsafe { libc::poll(&mut poll, 1, remaining as i32) } != 1 {
            return Err(ExecConfirmation::error());
        }
        match file.read(&mut bytes[offset..]) {
            Ok(0) | Err(_) => return Err(ExecConfirmation::error()),
            Ok(n) => offset += n,
        }
    }
    Ok(())
}

pub(crate) fn dispatch_private_launcher() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(PRIVATE_MARKER)) {
        return;
    }
    let result = (|| {
        let nonce_text = args.next().ok_or_else(ExecConfirmation::error)?;
        let text = nonce_text.to_str().ok_or_else(ExecConfirmation::error)?;
        if text.len() != 32 {
            return Err(ExecConfirmation::error());
        }
        // Every nonce byte comes from the parent-supplied argument, not
        // from a fixed initializer that could be mistaken for a generated nonce.
        let nonce: [u8; 16] = (0..16)
            .map(|i| {
                u8::from_str_radix(
                    text.get(i * 2..i * 2 + 2)
                        .ok_or_else(ExecConfirmation::error)?,
                    16,
                )
                .map_err(|_| ExecConfirmation::error())
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| ExecConfirmation::error())?;
        let program = args.next().ok_or_else(ExecConfirmation::error)?;
        // SAFETY: private descriptors must be pipes supplied by the parent. Reject anything else.
        unsafe {
            for fd in [3, 4] {
                let mut stat: libc::stat = std::mem::zeroed();
                if libc::fstat(fd, &mut stat) != 0 || stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
                    return Err(ExecConfirmation::error());
                }
            }
            sanitize_descriptors(true).map_err(|_| ExecConfirmation::error())?;
            if libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                return Err(ExecConfirmation::error());
            }
        }
        // SAFETY: the validated descriptors are exclusively owned by this private process.
        let mut status = unsafe { fs::File::from_raw_fd(3) };
        let mut gate = unsafe { fs::File::from_raw_fd(4) };
        status
            .write_all(&protocol_frame(1, nonce, 0))
            .map_err(|_| ExecConfirmation::error())?;
        let mut go = [0; FRAME_SIZE];
        read_protocol(&mut gate, &mut go)?;
        validate_frame(&go, 2, nonce)?;
        drop(gate);
        #[cfg(test)]
        if program == "--test-death-after-ready" {
            unsafe { libc::_exit(71) }
        }
        // No protocol environment is used. OsString values go directly to execvp/execve.
        let error = Command::new(program).args(args).exec();
        status
            .write_all(&protocol_frame(
                3,
                nonce,
                error.raw_os_error().unwrap_or(libc::EIO),
            ))
            .map_err(|_| ExecConfirmation::error())?;
        Err::<(), _>(ExecConfirmation::error())
    })();
    let _ = result;
    // SAFETY: private dispatch never returns into the consumer or unwinds application state.
    unsafe { libc::_exit(125) }
}

struct ExecConfirmation {
    status: fs::File,
    gate: fs::File,
    queue: OwnedFd,
    nonce: [u8; 16],
}

fn random_nonce() -> [u8; 16] {
    let mut nonce = std::mem::MaybeUninit::<[u8; 16]>::uninit();
    // SAFETY: arc4random_buf fills all 16 bytes of this valid, exclusively
    // borrowed allocation before returning and has no partial-success result.
    // Every bit pattern is valid for [u8; 16], so the array is then initialized.
    unsafe {
        libc::arc4random_buf(nonce.as_mut_ptr().cast(), std::mem::size_of_val(&nonce));
        nonce.assume_init()
    }
}

impl ExecConfirmation {
    fn prepare(command: &mut Command) -> Result<Self, ExecutionError> {
        let (ready_read, ready_write) = launch_pipe()?;
        let (resume_read, resume_write) = launch_pipe()?;
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(Self::error());
        }
        let queue = unsafe { OwnedFd::from_raw_fd(raw) };
        let nonce = random_nonce();
        command
            .arg(PRIVATE_MARKER)
            .arg(nonce.iter().map(|b| format!("{b:02x}")).collect::<String>());
        configure_child(command);
        // Duplicate above the fixed protocol slots before mapping, preserving std's error pipe.
        unsafe {
            command.pre_exec(move || {
                let status = libc::fcntl(ready_write.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64);
                let gate = libc::fcntl(resume_read.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64);
                if status < 0 || gate < 0 || libc::dup2(status, 3) < 0 || libc::dup2(gate, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(status);
                libc::close(gate);
                Ok(())
            });
        }
        Ok(Self {
            status: fs::File::from(ready_read),
            gate: fs::File::from(resume_write),
            queue,
            nonce,
        })
    }

    fn confirm(mut self, pid: u32, launch: Option<&ReservedLaunch>) -> Result<(), ExecutionError> {
        let mut ready = [0; FRAME_SIZE];
        read_protocol(&mut self.status, &mut ready)?;
        validate_frame(&ready, 1, self.nonce)?;
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: libc::NOTE_EXEC | libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(Self::error());
        }
        self.gate
            .write_all(&protocol_frame(2, self.nonce, 0))
            .map_err(|_| Self::error())?;
        if let Some(launch) = launch {
            let _ = launch.released.set(());
        }
        let timeout = libc::timespec {
            tv_sec: 3,
            tv_nsec: 0,
        };
        let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
        let count = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                &timeout,
            )
        };
        if count != 1 {
            return Err(Self::error());
        }
        let event = unsafe { event.assume_init() };
        // An observed exit without NOTE_EXEC proves the gated helper never ran
        // the target. An ambiguous confirmation failure must retain the binding.
        if event.ident == pid as usize
            && event.filter == libc::EVFILT_PROC
            && event.flags & libc::EV_ERROR == 0
            && event.fflags & libc::NOTE_EXIT != 0
            && event.fflags & libc::NOTE_EXEC == 0
            && let Some(launch) = launch
        {
            let _ = launch.never_executed.set(());
        }
        let mut poll = libc::pollfd {
            fd: self.status.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 3000) } != 1 {
            return Err(Self::error());
        }
        let mut first = [0; 1];
        match self.status.read(&mut first) {
            Ok(0)
                if event.ident == pid as usize
                    && event.filter == libc::EVFILT_PROC
                    && event.flags & libc::EV_ERROR == 0
                    && event.fflags & libc::NOTE_EXEC != 0 =>
            {
                Ok(())
            }
            Ok(1) => {
                let mut error = [0; FRAME_SIZE];
                error[0] = first[0];
                read_protocol(&mut self.status, &mut error[1..])?;
                let errno = validate_frame(&error, 3, self.nonce)?;
                if let Some(launch) = launch {
                    let _ = launch.never_executed.set(());
                }
                Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    format!(
                        "private helper target exec failed: errno {errno}: {}",
                        std::io::Error::from_raw_os_error(errno)
                    ),
                ))
            }
            _ => Err(Self::error()),
        }
    }

    fn error() -> ExecutionError {
        ExecutionError::new(
            ExecutionErrorCategory::Spawn,
            "private launcher requires valid READY, kernel NOTE_EXEC, and CLOEXEC status EOF",
        )
    }
}

fn launch_pipe() -> Result<(OwnedFd, OwnedFd), ExecutionError> {
    let mut fds = [-1; 2];
    // SAFETY: pipe initializes exactly two descriptors; both are immediately owned.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(ExecConfirmation::error());
    }
    // SAFETY: these are the new descriptors returned by pipe.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [&read, &write] {
        // SAFETY: owned live descriptor, no pointer arguments.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(ExecConfirmation::error());
        }
    }
    Ok((read, write))
}

struct MacosLifecycle<'a> {
    request: ExecutionRequest,
    preflight: &'a ValidatedPreflight,
    launch: Option<(Command, ExecConfirmation)>,
    child: Option<Child>,
    process_group: Option<i32>,
    cleanup_attempted: bool,
    cleanup_observed: bool,
}

impl ExecutionLifecycle for MacosLifecycle<'_> {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        if let Some(binding) = &self.request.reserved_node {
            binding.validate()?;
        }
        validate_helper(&self.request)?;
        let (mut command, confirmation) = self.launch.take().ok_or_else(ExecConfirmation::error)?;
        let child = command.spawn().map_err(|error| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot install Seatbelt profile or exec target: {error}"),
            )
        })?;
        drop(command);
        let pid = child.id();
        self.process_group = i32::try_from(pid).ok();
        self.child = Some(child);
        confirmation.confirm(
            pid,
            self.request
                .reserved_node
                .as_ref()
                .map(|b| b.launch.as_ref()),
        )?;
        let stdout = self
            .child
            .as_mut()
            .expect("spawned child retained")
            .stdout
            .take()
            .ok_or_else(|| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "target stdout pipe is unavailable",
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
                    "target stderr pipe is unavailable",
                )
            })?;
        set_nonblocking(&stdout)?;
        set_nonblocking(&stderr)?;

        let stop = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let sink_backpressure = Arc::new(AtomicBool::new(false));
        let output_bytes = Arc::new(AtomicUsize::new(0));
        let (mut sinks, stdout_sink, stderr_sink) = LiveSinks::spawn()?;
        let stdout_reader = spawn_reader(
            stdout,
            Arc::clone(&stop),
            Arc::clone(&overflow),
            Arc::clone(&output_bytes),
            Some(stdout_sink),
            Arc::clone(&sink_backpressure),
        );
        let stderr_reader = spawn_reader(
            stderr,
            Arc::clone(&stop),
            Arc::clone(&overflow),
            output_bytes,
            Some(stderr_sink),
            Arc::clone(&sink_backpressure),
        );
        let status = loop {
            if sink_backpressure.load(Ordering::Acquire) {
                self.kill_group();
                stop.store(true, Ordering::Release);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "live output sink remained backpressured past the bounded grace period",
                ));
            }
            if overflow.load(Ordering::Acquire) {
                self.kill_group();
                stop.store(true, Ordering::Release);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::OutputLimit,
                    format!(
                        "shared internal stdout+stderr safety ceiling of {INTERNAL_OUTPUT_CEILING} combined bytes was exceeded"
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
        // `try_wait` reaped the leader. Its numeric process-group ID can now be reused,
        // so it must never be signalled during delayed pipe shutdown.
        self.process_group = None;
        self.child = None;
        if let Some(binding) = &self.request.reserved_node {
            let _ = binding.launch.root_reaped.set(());
        }
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
        sinks.finish()?;
        let requested = self.preflight.support.requested().clone();
        let established = evidence_for_dimensions(
            &requested,
            "sandbox-exec deny-default Seatbelt, nonce-bound private helper READY/GO, kernel NOTE_EXEC and CLOEXEC status EOF",
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
        if self.child.is_none() {
            self.process_group = None;
            return;
        }
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

// This fixed buffer is deliberately independent of both current and hard rlimits.
// A full buffer may be truncated, so reject it rather than miss inherited authority.
// The child is single-threaded here. No allocation or descriptor creation occurs
// between enumeration and sanitation, including in Rust's post-fork pre_exec.
fn sanitize_descriptors(close: bool) -> std::io::Result<()> {
    sanitize_descriptors_with_capacity::<16_384>(close)
}

fn sanitize_descriptors_with_capacity<const CAPACITY: usize>(close: bool) -> std::io::Result<()> {
    let mut entries = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; CAPACITY];
    let size = std::mem::size_of_val(&entries);
    // SAFETY: writable fixed-size buffer and the current process's PID.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            entries.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    if bytes <= 0
        || bytes as usize >= size
        || !(bytes as usize).is_multiple_of(std::mem::size_of::<libc::proc_fdinfo>())
    {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    for entry in &entries[..bytes as usize / std::mem::size_of::<libc::proc_fdinfo>()] {
        let fd = entry.proc_fd;
        if fd < 0 {
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        if fd < if close { 5 } else { 3 } {
            continue;
        }
        // Before helper exec preserve Rust's spawn-error pipe until exec by setting
        // CLOEXEC. In the helper only stdio and validated protocol pipes survive.
        let result = unsafe {
            if close {
                libc::close(fd)
            } else {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC)
            }
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn configure_child(command: &mut Command) {
    // SAFETY: this closure runs after std has installed descriptors 0/1/2. setpgid uses the child
    // PID (0), and CLOEXEC removes unintended descriptors >=3 at target exec; neither
    // operation captures Rust borrows.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            sanitize_descriptors(false)?;
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
    stop: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    output_bytes: Arc<AtomicUsize>,
    mut live_sink: Option<ChildStdin>,
    sink_backpressure: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<Vec<u8>, ExecutionError>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    let mut used = output_bytes.load(Ordering::Acquire);
                    let reserved = loop {
                        let Some(total) = used
                            .checked_add(count)
                            .filter(|total| *total <= INTERNAL_OUTPUT_CEILING)
                        else {
                            break false;
                        };
                        match output_bytes.compare_exchange_weak(
                            used,
                            total,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => break true,
                            Err(actual) => used = actual,
                        }
                    };
                    if !reserved {
                        overflow.store(true, Ordering::Release);
                        break;
                    }
                    captured.extend_from_slice(&chunk[..count]);
                    if let Some(sink) = live_sink.as_mut() {
                        write_live_output(sink, &chunk[..count], &stop, &sink_backpressure)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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

fn write_live_output(
    sink: &mut ChildStdin,
    bytes: &[u8],
    stop: &AtomicBool,
    backpressure: &AtomicBool,
) -> Result<(), ExecutionError> {
    let mut written = 0;
    let mut blocked_since = None;
    while written < bytes.len() {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match sink.write(&bytes[written..]) {
            Ok(0) => {
                backpressure.store(true, Ordering::Release);
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "live output sink closed before accepting child output",
                ));
            }
            Ok(count) => {
                written += count;
                blocked_since = None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let since = blocked_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= LIVE_SINK_BACKPRESSURE_GRACE {
                    backpressure.store(true, Ordering::Release);
                    return Err(ExecutionError::new(
                        ExecutionErrorCategory::Internal,
                        "live output sink remained backpressured past the bounded grace period",
                    ));
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                backpressure.store(true, Ordering::Release);
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    format!("cannot stream child output: {error}"),
                ));
            }
        }
    }
    Ok(())
}

struct LiveSinks {
    children: Vec<Child>,
}

impl LiveSinks {
    fn spawn() -> Result<(Self, ChildStdin, ChildStdin), ExecutionError> {
        let (stdout_child, stdout_input) = spawn_live_sink(libc::STDOUT_FILENO)?;
        let (stderr_child, stderr_input) = match spawn_live_sink(libc::STDERR_FILENO) {
            Ok(sink) => sink,
            Err(error) => {
                let mut child = stdout_child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok((
            Self {
                children: vec![stdout_child, stderr_child],
            },
            stdout_input,
            stderr_input,
        ))
    }

    fn finish(&mut self) -> Result<(), ExecutionError> {
        let deadline = Instant::now() + LIVE_SINK_BACKPRESSURE_GRACE;
        loop {
            let mut complete = true;
            for child in &mut self.children {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => {}
                    Ok(Some(_)) => {
                        return Err(ExecutionError::new(
                            ExecutionErrorCategory::Internal,
                            "live output sink failed",
                        ));
                    }
                    Ok(None) => complete = false,
                    Err(error) => {
                        return Err(ExecutionError::new(
                            ExecutionErrorCategory::Internal,
                            format!("cannot supervise live output sink: {error}"),
                        ));
                    }
                }
            }
            if complete {
                self.children.clear();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "live output sink remained backpressured past the bounded grace period",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for LiveSinks {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_live_sink(destination: i32) -> Result<(Child, ChildStdin), ExecutionError> {
    // SAFETY: duplicate the caller's live stream descriptor into independent ownership.
    let duplicate = unsafe { libc::fcntl(destination, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::Internal,
            format!(
                "cannot duplicate live output sink: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    // SAFETY: `duplicate` is a newly owned descriptor.
    let output = unsafe { OwnedFd::from_raw_fd(duplicate) };
    let mut command = Command::new("/bin/cat");
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output))
        .stderr(Stdio::null());
    // SAFETY: std has installed the relay's assigned stdio before this closure runs.
    // Mark every remaining descriptor close-on-exec, including Rust's spawn-error channel;
    // that channel must survive until exec and is closed atomically by a successful exec.
    unsafe {
        command.pre_exec(|| sanitize_descriptors(false));
    }
    let mut child = command.spawn().map_err(|error| {
        ExecutionError::new(
            ExecutionErrorCategory::Internal,
            format!("cannot start bounded live output sink: {error}"),
        )
    })?;
    let input = child.stdin.take().ok_or_else(|| {
        ExecutionError::new(
            ExecutionErrorCategory::Internal,
            "live output sink input is unavailable",
        )
    })?;
    set_nonblocking(&input)?;
    Ok((child, input))
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ReservedLaunch {
    released: OnceLock<()>,
    never_executed: OnceLock<()>,
    root_reaped: OnceLock<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReservedNodeBinding {
    directory: PathBuf,
    runtime: PathBuf,
    identity: (u64, u64),
    mode: u32,
    size: u64,
    launch: Arc<ReservedLaunch>,
}
impl ReservedNodeBinding {
    fn validate_write_authority(
        &self,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<(), ExecutionError> {
        let directory = fs::canonicalize(&self.directory).map_err(|_| ExecConfirmation::error())?;
        let private_overlaps =
            |path: &Path| directory.starts_with(path) || path.starts_with(&directory);
        if policy.write.iter().any(|grant| {
            private_overlaps(&grant.path)
                || match &grant.resolution {
                    super::GrantResolution::MissingWriteDirectory {
                        canonical_ancestor, ..
                    } => private_overlaps(canonical_ancestor),
                    _ => false,
                }
        }) {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "reserved node directory overlaps project write authority",
            ));
        }
        if policy.write.iter().any(|grant| {
            self.runtime.starts_with(&grant.path)
                || match &grant.resolution {
                    super::GrantResolution::MissingWriteDirectory {
                        canonical_ancestor,
                        relative_target,
                    } => self
                        .runtime
                        .starts_with(canonical_ancestor.join(relative_target)),
                    _ => false,
                }
        }) {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "trusted Node runtime overlaps project write authority",
            ));
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), ExecutionError> {
        use std::os::unix::fs::MetadataExt;
        let dir = fs::symlink_metadata(&self.directory).map_err(|_| ExecConfirmation::error())?;
        let meta = fs::symlink_metadata(self.directory.join("node"))
            .map_err(|_| ExecConfirmation::error())?;
        if !dir.is_dir()
            || dir.mode() & 0o777 != 0o700
            || dir.uid() != unsafe { libc::geteuid() }
            || !meta.is_file()
            || (meta.dev(), meta.ino()) != self.identity
            || meta.mode() != self.mode
            || meta.len() != self.size
            || meta.nlink() != 1
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "reserved node identity or private directory changed",
            ));
        }
        Ok(())
    }
}

struct ReservedNode {
    evidence: ReservedNodeBinding,
    descendants_allowed: bool,
}
impl ReservedNode {
    fn create(runtime: &super::TrustedNodeRuntime, project: &Path) -> Result<Self, ExecutionError> {
        Self::create_with_copy(runtime, project, copy_runtime_snapshot)
    }

    fn create_with_copy(
        runtime: &super::TrustedNodeRuntime,
        project: &Path,
        copy: impl FnOnce(&Path, &Path) -> std::io::Result<(u64, u64, u64, u64, u32, u64)>,
    ) -> Result<Self, ExecutionError> {
        use std::os::unix::fs::DirBuilderExt;
        let project = fs::canonicalize(project).map_err(|_| ExecConfirmation::error())?;
        let current = super::TrustedNodeRuntime::checked(&runtime.path)?;
        if &current != runtime {
            return Err(ExecConfirmation::error());
        }
        // Canonical parent plus an atomic exclusive mkdir avoids traversal through generated names.
        let parent =
            fs::canonicalize(std::env::temp_dir()).map_err(|_| ExecConfirmation::error())?;
        let nonce = random_nonce();
        let directory = parent.join(format!(
            "tapid-node-{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        if directory.starts_with(&project) || project.starts_with(&directory) {
            return Err(ExecConfirmation::error());
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| ExecConfirmation::error())?;
        let mut guard = Self {
            evidence: ReservedNodeBinding {
                directory,
                runtime: runtime.path.clone(),
                identity: (0, 0),
                mode: 0,
                size: 0,
                launch: Arc::new(ReservedLaunch::default()),
            },
            descendants_allowed: true,
        };
        let (source_device, source_inode, device, inode, mode, size) =
            copy(&runtime.path, &guard.evidence.directory.join("node")).map_err(|error| {
                ExecutionError::new(
                    ExecutionErrorCategory::PolicyViolation,
                    format!("cannot create verified reserved Node snapshot: {error}"),
                )
            })?;
        if (source_device, source_inode) != (runtime.identity.device, runtime.identity.inode) {
            return Err(ExecConfirmation::error());
        }
        guard.evidence.identity = (device, inode);
        guard.evidence.mode = mode;
        guard.evidence.size = size;
        guard.evidence.validate()?;
        Ok(guard)
    }
    fn cleanup(&mut self) -> bool {
        if (self.descendants_allowed || self.evidence.launch.root_reaped.get().is_none())
            && self.evidence.launch.released.get().is_some()
            && self.evidence.launch.never_executed.get().is_none()
        {
            // Restricted cannot prove every descendant is gone, even on errors.
            // Never delete a PATH authority that a surviving process may use.
            return false;
        }
        fs::remove_dir_all(&self.evidence.directory).is_ok() || !self.evidence.directory.exists()
    }
}
impl Drop for ReservedNode {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// Relocation must not silently change the executable's library search authority.
// External dylibs require a separately verified transitive closure, not a symlink
// back to the installation or DYLD_* injection. Inspect the held source descriptor.
fn validate_snapshot_dependencies(source: &mut fs::File) -> std::io::Result<()> {
    let invalid = |message: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let mut magic = [0; 4];
    source.read_exact(&mut magic)?;
    if &magic[..2] == b"#!" {
        source.seek(SeekFrom::Start(0))?;
        return Ok(());
    }
    let header_size = match u32::from_le_bytes(magic) {
        0xfeed_facf => 32,
        0xfeed_face => 28,
        _ => {
            return Err(invalid(
                "unsupported Mach-O runtime format; use a standalone thin Node distribution",
            ));
        }
    };
    let mut header = vec![0; header_size - 4];
    source.read_exact(&mut header)?;
    let word = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().unwrap());
    let count = word(&header[12..16]) as usize;
    let size = word(&header[16..20]) as usize;
    if size > 1024 * 1024 || count > size / 8 {
        return Err(invalid("invalid Mach-O load-command bounds"));
    }
    let mut commands = vec![0; size];
    source.read_exact(&mut commands)?;
    let mut offset = 0;
    for _ in 0..count {
        let prefix = commands
            .get(offset..offset + 8)
            .ok_or_else(|| invalid("truncated Mach-O load command"))?;
        let kind = word(&prefix[..4]) & 0x7fff_ffff;
        let length = word(&prefix[4..]) as usize;
        if length < 8 || !length.is_multiple_of(4) {
            return Err(invalid("invalid Mach-O load-command length"));
        }
        let command = commands
            .get(offset..offset + length)
            .ok_or_else(|| invalid("truncated Mach-O load command"))?;
        // LC_LOAD_DYLIB, WEAK, REEXPORT, LAZY, UPWARD and LOAD_DYLINKER.
        if matches!(kind, 0xc | 0x18 | 0x1f | 0x20 | 0x23 | 0xe) {
            let minimum = if kind == 0xe { 12 } else { 24 };
            if length < minimum {
                return Err(invalid("invalid Mach-O dependency command"));
            }
            let start = word(&command[8..12]) as usize;
            if start < minimum || start >= length {
                return Err(invalid("invalid Mach-O dependency string offset"));
            }
            let name = &command[start..];
            let end = name
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| invalid("unterminated Mach-O dependency name"))?;
            let name = &name[..end];
            let system = name.starts_with(b"/usr/lib/") || name.starts_with(b"/System/Library/");
            if !system || name.split(|b| *b == b'/').any(|p| p == b".." || p == b".") {
                return Err(invalid(
                    "nonrelocatable Mach-O dependency; use a standalone Node distribution with only Apple system libraries",
                ));
            }
        } else if kind == 0x27 {
            return Err(invalid(
                "Mach-O dyld environment is unsupported for runtime snapshots",
            ));
        }
        offset += length;
    }
    if offset != size {
        return Err(invalid("invalid Mach-O load-command total"));
    }
    source.seek(SeekFrom::Start(0))?;
    Ok(())
}

fn copy_runtime_snapshot(
    source: &Path,
    target: &Path,
) -> std::io::Result<(u64, u64, u64, u64, u32, u64)> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let mut source_file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source)?;
    let before = source_file.metadata()?;
    validate_snapshot_dependencies(&mut source_file)?;
    let mut target_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(target)?;
    std::io::copy(&mut source_file, &mut target_file)?;
    target_file.sync_all()?;
    target_file.set_permissions(fs::Permissions::from_mode(before.mode()))?;

    source_file.seek(SeekFrom::Start(0))?;
    target_file.seek(SeekFrom::Start(0))?;
    let mut source_bytes = [0_u8; 64 * 1024];
    let mut target_bytes = [0_u8; 64 * 1024];
    loop {
        let source_count = source_file.read(&mut source_bytes)?;
        let target_count = target_file.read(&mut target_bytes)?;
        if source_count != target_count
            || source_bytes[..source_count] != target_bytes[..target_count]
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reserved Node snapshot differs from held runtime",
            ));
        }
        if source_count == 0 {
            break;
        }
    }
    let after = source_file.metadata()?;
    let snapshot = target_file.metadata()?;
    if (before.dev(), before.ino(), before.mode(), before.len())
        != (after.dev(), after.ino(), after.mode(), after.len())
        || !snapshot.is_file()
        || snapshot.mode() != before.mode()
        || snapshot.len() != before.len()
        || (snapshot.dev(), snapshot.ino()) == (before.dev(), before.ino())
        || snapshot.nlink() != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "reserved Node snapshot metadata validation failed",
        ));
    }
    Ok((
        before.dev(),
        before.ino(),
        snapshot.dev(),
        snapshot.ino(),
        snapshot.mode(),
        snapshot.len(),
    ))
}

pub(super) fn execute_reserved(
    request: &ExecutionRequest,
) -> Result<ExecutionOutcome, ExecutionError> {
    request.validate()?;
    let mut request = request.clone();
    request.project_root =
        fs::canonicalize(&request.project_root).map_err(|_| ExecConfirmation::error())?;
    let mut reserved = request
        .trusted_node_runtime
        .as_ref()
        .map(|runtime| ReservedNode::create(runtime, &request.project_root))
        .transpose()?;
    if let Some(binding) = &mut reserved {
        binding.descendants_allowed = request.policy().subprocess();
        request
            .executable_search_paths
            .insert(0, binding.evidence.directory.clone());
        request.reserved_node = Some(binding.evidence.clone());
    }
    let mut result = super::execute_with_backend(&request, &PlatformBackend);
    let cleanup = reserved.as_mut().map(ReservedNode::cleanup);
    if let Ok(outcome) = &mut result {
        outcome.enforcement.executable_resolution = Some(super::ExecutableResolutionEvidence {
            path: request.child_environment()[std::ffi::OsStr::new("PATH")].as_bytes().to_vec(),
            path_order: request.executable_search_paths.iter().map(|p| p.as_os_str().as_bytes().to_vec()).collect(),
            caller_path_inherited: false,
            reserved_node: request.reserved_node.as_ref().map(|b| super::ReservedExecutableEvidence {
                command: "node".into(), private_path: b.directory.join("node").as_os_str().as_bytes().to_vec(),
                trusted_runtime: b.runtime.as_os_str().as_bytes().to_vec(), device: b.identity.0, inode: b.identity.1,
                mechanism: "byte-verified private snapshot".into(),
                identity_checks: "held runtime identity and executable metadata; distinct snapshot inode; exact byte comparison; immediately before spawn".into(),
                cleanup_observed: cleanup.unwrap_or(false),
                limitations: if cleanup == Some(true) {
                    "private snapshot removed after target exit under subprocess=false, whose Seatbelt process-fork denial prevents descendants; host writes or races after final validation remain possible; explicit project node paths are outside reserved bare-node/env-shebang resolution"
                } else if request.policy().subprocess() {
                    "private snapshot retained because subprocess-enabled descendants may survive Restricted cleanup; fresh owner-only random directory and distinct inode per attempt, never reused; Seatbelt denies target and descendant writes to the snapshot while project writes remain allowed; consumes temporary storage until OS cleanup or host removal after all descendants exit; removal while descendants survive ends reserved-node protection; host writes or races after final validation remain possible; explicit project node paths are outside reserved bare-node/env-shebang resolution"
                } else {
                    "private snapshot removal was not observed after subprocess=false root exit; temporary storage may remain until OS cleanup or host removal; directories are never reused; host writes or races after final validation remain possible; explicit project node paths are outside reserved bare-node/env-shebang resolution"
                }.into(),
            }),
        });
    }
    result
}

fn validate_helper(request: &ExecutionRequest) -> Result<(), ExecutionError> {
    let helper = request
        .launcher
        .as_ref()
        .ok_or_else(ExecConfirmation::error)?;
    let helper_path = helper
        .executable
        .as_ref()
        .ok_or_else(ExecConfirmation::error)?;
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(helper_path).map_err(|_| ExecConfirmation::error())?;
    if fs::canonicalize(helper_path).ok().as_ref() != Some(helper_path)
        || helper.identity != Some((metadata.dev(), metadata.ino()))
    {
        return Err(ExecConfirmation::error());
    }
    Ok(())
}

fn prepare_launch(
    request: &ExecutionRequest,
    preflight: &ValidatedPreflight,
    profile: &CompiledProfile,
) -> Result<(Command, ExecConfirmation), ExecutionError> {
    let helper = request
        .launcher
        .as_ref()
        .ok_or_else(ExecConfirmation::error)?;
    let helper_path = helper
        .executable
        .as_ref()
        .ok_or_else(ExecConfirmation::error)?;
    validate_helper(request)?;
    let mut command = Command::new(SANDBOX_EXEC);
    for (key, value) in &profile.parameters {
        let mut binding = OsString::from(format!("{key}="));
        binding.push(value);
        command.arg("-D").arg(binding);
    }
    command.arg("-p").arg(&profile.text).arg(helper_path);
    let confirmation = ExecConfirmation::prepare(&mut command)?;
    #[cfg(test)]
    if profile.abort_before_exec {
        command.arg("--test-death-after-ready");
    }
    command.arg(&request.program);
    command
        .args(&request.arguments)
        .current_dir(request.working_directory())
        .env_clear()
        .envs(&preflight.child_environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok((command, confirmation))
}

#[cfg(test)]
#[path = "macos_restricted/tests.rs"]
mod tests;
