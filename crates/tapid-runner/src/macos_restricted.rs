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
        let mut nonce = [0; 16];
        for (i, byte) in nonce.iter_mut().enumerate() {
            *byte = u8::from_str_radix(
                text.get(i * 2..i * 2 + 2)
                    .ok_or_else(ExecConfirmation::error)?,
                16,
            )
            .map_err(|_| ExecConfirmation::error())?;
        }
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

impl ExecConfirmation {
    fn prepare(command: &mut Command) -> Result<Self, ExecutionError> {
        let (ready_read, ready_write) = launch_pipe()?;
        let (resume_read, resume_write) = launch_pipe()?;
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(Self::error());
        }
        let queue = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut nonce = [0; 16];
        unsafe {
            libc::arc4random_buf(nonce.as_mut_ptr().cast(), nonce.len());
        }
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
    const CAPACITY: usize = 16_384;
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
    launch: Arc<ReservedLaunch>,
}
impl ReservedNodeBinding {
    fn validate_write_authority(
        &self,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<(), ExecutionError> {
        let directory = fs::canonicalize(&self.directory).map_err(|_| ExecConfirmation::error())?;
        let overlaps = |path: &Path| directory.starts_with(path) || path.starts_with(&directory);
        if overlaps(&policy.project_root)
            || policy.write.iter().any(|grant| {
                overlaps(&grant.path)
                    || match &grant.resolution {
                        super::GrantResolution::MissingWriteDirectory {
                            canonical_ancestor,
                            ..
                        } => overlaps(canonical_ancestor),
                        _ => false,
                    }
            })
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                "reserved node directory overlaps project write authority",
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
        Self::create_with_link(runtime, project, |source, target| {
            fs::hard_link(source, target)
        })
    }

    fn create_with_link(
        runtime: &super::TrustedNodeRuntime,
        project: &Path,
        link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
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
        let mut nonce = [0_u8; 16];
        unsafe {
            libc::arc4random_buf(nonce.as_mut_ptr().cast(), nonce.len());
        }
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
        let guard = Self {
            evidence: ReservedNodeBinding {
                directory,
                runtime: runtime.path.clone(),
                identity: (runtime.identity.device, runtime.identity.inode),
                launch: Arc::new(ReservedLaunch::default()),
            },
            descendants_allowed: true,
        };
        link(&runtime.path, &guard.evidence.directory.join("node")).map_err(|error| {
            ExecutionError::new(
                ExecutionErrorCategory::PolicyViolation,
                format!(
                    "cannot create reserved node hard link (cross-device is unsupported): {error}"
                ),
            )
        })?;
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
                mechanism: "verified-hard-link".into(),
                identity_checks: "runtime capture; hard-link creation; immediately before spawn".into(),
                cleanup_observed: cleanup.unwrap_or(false),
                limitations: if cleanup == Some(true) {
                    "private binding removed after target exit under subprocess=false, whose Seatbelt process-fork denial prevents descendants; host writes to the inode or races after final validation remain possible; explicit project node paths are outside reserved bare-node/env-shebang resolution"
                } else if request.policy().subprocess() {
                    "private binding retained because subprocess-enabled descendants may survive Restricted cleanup; fresh owner-only random directory per attempt, never reused; consumes temporary storage until OS cleanup or host removal after all descendants exit; removal while descendants survive ends reserved-node protection; host writes to the inode or races after final validation remain possible; explicit project node paths are outside reserved bare-node/env-shebang resolution"
                } else {
                    "private binding removal was not observed after subprocess=false root exit; temporary storage may remain until OS cleanup or host removal; directories are never reused; host writes to the inode or races after final validation remain possible; explicit project node paths are outside reserved bare-node/env-shebang resolution"
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
        .current_dir(&preflight.policy.project_root)
        .env_clear()
        .envs(&preflight.child_environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok((command, confirmation))
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
    fn reserved_node_rejects_relative_and_aliased_project_temp_write_authority() {
        const CHILD: &str = "TAPID_TEST_PRIVATE_OVERLAP";
        if let Ok(mode) = std::env::var(CHILD) {
            let root = fs::canonicalize(".").unwrap();
            let project = if mode == "relative" {
                PathBuf::from(".")
            } else {
                PathBuf::from(root.to_string_lossy().replacen("/private/var/", "/var/", 1))
            };
            let request = ExecutionRequest::builder("/bin/sh")
                .args(["-c", ": > marker"])
                .project_root(project)
                .trusted_node_runtime(root.join("node"))
                .executable_search_path(&root)
                .policy(policy(false, true, Vec::new()))
                .build()
                .unwrap();
            let result = super::super::execute(&request);
            assert!(
                result.is_err(),
                "overlapping private directory issued a receipt"
            );
            assert!(!root.join("marker").exists());
            assert_eq!(
                fs::read_dir(root.join("tmp")).unwrap().count(),
                0,
                "failed creation left a reserved directory"
            );
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let root = temp_project("private-overlap");
        fs::create_dir(root.join("tmp")).unwrap();
        fs::write(root.join("node"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(root.join("node"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut failures = Vec::new();
        for mode in ["relative", "alias"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution::platform_backend::tests::reserved_node_rejects_relative_and_aliased_project_temp_write_authority", "--nocapture"])
                .current_dir(&root).env(CHILD, mode).env("TMPDIR", root.join("tmp"))
                .output().unwrap();
            if !output.status.success() {
                failures.push(format!(
                    "{mode}: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            let _ = fs::remove_file(root.join("marker"));
        }
        fs::remove_dir_all(root).unwrap();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn surviving_descendant_keeps_reserved_node_after_root_receipt() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = temp_project("surviving-node");
        let local = root.join("node_modules/.bin");
        fs::create_dir_all(&local).unwrap();
        let runtime = root.join("node");
        for (path, script) in [
            (&runtime, "#!/bin/sh\nprintf verified > verified\n"),
            (
                &local.join("node"),
                "#!/bin/sh\nprintf hijacked > hijacked\n",
            ),
        ] {
            fs::write(path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        // setsid escapes best-effort group cleanup. Redirect every stream so root
        // completion does not depend on the descendant closing an inherited pipe.
        let script = r#"
            directory = ENV.fetch('PATH').split(':').first
            Process.fork do
                Process.setsid
                STDIN.reopen('/dev/null')
                STDOUT.reopen('descendant.stdout', 'w')
                STDERR.reopen('descendant.stderr', 'w')
                File.write('ready', 'ready')
                deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 5
                sleep 0.01 until File.exist?('receipt-returned') || Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
                deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 1
                sleep 0.01 while File.directory?(directory) && Process.clock_gettime(Process::CLOCK_MONOTONIC) < deadline
                File.write('binding-state', File.directory?(directory) ? 'retained' : 'removed')
                system('/bin/sh', '-c', 'node')
                File.write('done', 'done')
                exit! 0
            end
            deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 3
            sleep 0.01 until File.exist?('ready') || Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
            abort 'descendant never ready' unless File.exist?('ready')
        "#;
        let request = ExecutionRequest::builder("/usr/bin/ruby")
            .args(["--disable-gems", "-e", script])
            .project_root(&root)
            .trusted_node_runtime(&runtime)
            .executable_search_paths([local, root.clone(), PathBuf::from("/usr/bin")])
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let outcome = super::super::execute(&request).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        fs::write(root.join("receipt-returned"), "receipt").unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !root.join("done").exists() && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        assert!(root.join("done").exists(), "descendant did not finish");
        let node = outcome
            .enforcement()
            .executable_resolution()
            .unwrap()
            .reserved_node
            .as_ref()
            .unwrap();
        let directory = Path::new(std::ffi::OsStr::from_bytes(&node.private_path))
            .parent()
            .unwrap();
        let hijacked = root.join("hijacked").exists();
        let verified = root.join("verified").exists();
        let state = fs::read_to_string(root.join("binding-state")).unwrap();
        let metadata = fs::metadata(directory).ok();
        // This test knows the descendant has finished using PATH. Production does not.
        let _ = fs::remove_dir_all(directory);
        fs::remove_dir_all(root).unwrap();
        assert!(
            !hijacked,
            "hostile node created hijacked marker despite receipt; binding={state}; cleanup_observed={}",
            node.cleanup_observed
        );
        assert!(verified);
        assert_eq!(state, "retained");
        assert!(!node.cleanup_observed);
        assert!(node.limitations.contains("retained"));
        let metadata = metadata.unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    }

    #[test]
    fn reserved_node_cleans_after_subprocess_denied_root_exit() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_project("node-no-descendants");
        let runtime = root.join("node");
        fs::write(&runtime, "#!/bin/sh\nprintf verified\n").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", "exec node"])
            .project_root(&root)
            .trusted_node_runtime(&runtime)
            .executable_search_path(&root)
            .policy(policy(false, false, Vec::new()))
            .build()
            .unwrap();
        let outcome = super::super::execute(&request).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert_eq!(outcome.stdout(), b"verified");
        let node = outcome
            .enforcement()
            .executable_resolution()
            .unwrap()
            .reserved_node
            .as_ref()
            .unwrap();
        assert!(node.cleanup_observed);
        assert!(node.limitations.contains("subprocess=false"));
        let directory = Path::new(std::ffi::OsStr::from_bytes(&node.private_path))
            .parent()
            .unwrap();
        assert!(!directory.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reserved_node_preserves_native_bytes_and_exact_empty_caller_path() {
        use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
        let root = temp_project("node-bytes");
        let runtime_dir = root.join("runtime");
        fs::create_dir(&runtime_dir).unwrap();
        let runtime = runtime_dir.join("node");
        fs::write(&runtime, b"#!/bin/sh\nprintf '%s' \"$1\"").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let local = root.join("local");
        fs::create_dir(&local).unwrap();
        let value = OsString::from_vec(b"f\x80o".to_vec());
        let request = ExecutionRequest::builder("/bin/sh")
            .args([
                OsString::from("-c"),
                OsString::from("node \"$1\""),
                OsString::from("fixture"),
                value.clone(),
            ])
            .trusted_node_runtime(&runtime)
            .executable_search_paths([local.clone(), runtime_dir.clone()])
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let outcome = super::super::execute(&request).unwrap();
        assert_eq!(outcome.stdout(), value.as_bytes());
        let resolution = outcome.enforcement().executable_resolution().unwrap();
        assert_eq!(resolution.path_order.len(), 3);
        assert_eq!(resolution.path_order[1], local.as_os_str().as_bytes());
        assert_eq!(resolution.path_order[2], runtime_dir.as_os_str().as_bytes());
        assert_eq!(resolution.path, resolution.path_order.join(&b':'));
        assert!(!resolution.caller_path_inherited);
        assert!(!resolution.reserved_node.as_ref().unwrap().cleanup_observed);
        fs::remove_dir_all(Path::new(std::ffi::OsStr::from_bytes(
            &resolution.path_order[0],
        )))
        .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reserved_node_executes_captured_inode_after_source_rename() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_project("node-source-rename");
        let runtime = root.join("node");
        fs::write(&runtime, b"#!/bin/sh\nprintf captured-inode").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let mut request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", "node"])
            .trusted_node_runtime(&runtime)
            .executable_search_path(&root)
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let binding =
            ReservedNode::create(request.trusted_node_runtime.as_ref().unwrap(), &root).unwrap();
        request
            .executable_search_paths
            .insert(0, binding.evidence.directory.clone());
        request.reserved_node = Some(binding.evidence.clone());
        fs::rename(&runtime, root.join("renamed-node")).unwrap();
        let outcome = super::super::execute_with_backend(&request, &PlatformBackend).unwrap();
        assert_eq!(outcome.stdout(), b"captured-inode");
        assert!(
            outcome
                .enforcement()
                .resolved_filesystem()
                .grants()
                .iter()
                .any(|g| g.path() == binding.evidence.directory.join("node")
                    && g.source() == super::super::FilesystemGrantSource::BackendRuntime)
        );
        drop(binding);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reserved_node_is_a_verified_hardlink_and_detects_tampering() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = temp_project("node-identity");
        let runtime = root.join("node");
        fs::write(&runtime, b"#!/bin/sh\nprintf trusted").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let trusted = super::super::TrustedNodeRuntime::checked(&runtime).unwrap();
        let mut binding = ReservedNode::create(&trusted, &root).unwrap();
        let node = binding.evidence.directory.join("node");
        assert!(
            !fs::symlink_metadata(&node)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(&node).unwrap().ino(),
            fs::metadata(&runtime).unwrap().ino()
        );
        assert_eq!(
            fs::metadata(&binding.evidence.directory).unwrap().mode() & 0o777,
            0o700
        );
        fs::rename(&runtime, root.join("old-node")).unwrap();
        fs::write(&runtime, "replacement").unwrap();
        binding.evidence.validate().unwrap();
        fs::remove_file(&node).unwrap();
        fs::write(&node, "tamper").unwrap();
        assert!(binding.evidence.validate().is_err());
        let directory = binding.evidence.directory.clone();
        assert!(binding.cleanup());
        assert!(!directory.exists());
        assert!(ReservedNode::create(&trusted, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_helper_closes_unexpected_descriptors_before_target_exec() {
        let file = fs::File::open("/dev/null").unwrap();
        let raw = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
        assert!(raw >= 64);
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut command = Command::new(std::env::current_exe().unwrap());
        let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
        command
            .arg("/bin/sh")
            .args(["-c", &format!("test ! -e /dev/fd/{raw}")])
            .env_clear()
            .env("PATH", "")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Simulate a launcher leaking a descriptor after the parent's initial sanitation.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(raw, libc::F_SETFD, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop(command);
        confirmation.confirm(child.id(), None).unwrap();
        let status = child.wait().unwrap();
        drop(fd);
        assert!(
            status.success(),
            "private helper leaked an unexpected descriptor"
        );
    }

    #[test]
    fn lowered_nofile_never_leaks_authority_through_helper_exec() {
        let root = temp_project("lowered-nofile");
        let file = fs::File::create(root.join("authority")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let connected = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        for source in [
            file.as_raw_fd(),
            listener.as_raw_fd(),
            connected.as_raw_fd(),
        ] {
            for helper_leak in [false, true] {
                let raw = 512;
                let mut command = Command::new(std::env::current_exe().unwrap());
                // Lower only the child limit, before production pre_exec sanitation.
                unsafe {
                    command.pre_exec(move || {
                        let mut limit = std::mem::zeroed::<libc::rlimit>();
                        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        limit.rlim_cur = limit.rlim_max.min(1024);
                        if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0
                            || libc::dup2(source, raw) != raw
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                        limit.rlim_cur = 256;
                        if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0
                            || libc::fcntl(raw, libc::F_SETFD, 0) != 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
                command
                    .arg("/usr/bin/ruby")
                    .args([
                        "--disable-gems",
                        "-e",
                        &format!(
                            "begin; IO.for_fd({raw}); exit 91; rescue Errno::EBADF; exit 0; end"
                        ),
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if helper_leak {
                    unsafe {
                        command.pre_exec(move || {
                            if libc::fcntl(raw, libc::F_SETFD, 0) < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                }
                let mut child = command.spawn().unwrap();
                drop(command);
                confirmation.confirm(child.id(), None).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                let status = loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    if Instant::now() >= deadline {
                        child.kill().unwrap();
                        child.wait().unwrap();
                        panic!("descriptor target stalled");
                    }
                    thread::sleep(POLL_INTERVAL);
                };
                assert!(
                    status.success(),
                    "FD {raw} survived lowered limit; helper leak={helper_leak}, status={status}"
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn helper_dead_after_ready_before_go_cannot_confirm_exec() {
        let mut command = Command::new("/usr/bin/ruby");
        command.args([
            "--disable-gems",
            "-e",
            "IO.for_fd(3).syswrite([1,1].pack('NN') + [ARGV[1]].pack('H*') + [0,0].pack('NN'))",
            "--",
        ]);
        let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        drop(command);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "READY writer failed");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("READY writer stalled");
            }
            thread::sleep(POLL_INTERVAL);
        }
        // The full READY is queued, but the child is already reaped before the
        // parent can register NOTE_EXEC or send GO. EOF cannot count as exec.
        assert!(confirmation.confirm(child.id(), None).is_err());
        assert!(Instant::now() < deadline);
    }

    #[test]
    fn real_pipe_protocol_failures_have_no_receipt_and_reap_the_helper() {
        let root = temp_project("protocol-failures");
        let request = ExecutionRequest::builder("/bin/sh")
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let additions = PlatformBackend
            .runtime_filesystem_additions(&request)
            .unwrap();
        let policy = super::super::resolve_policy(&request, additions).unwrap();
        let preflight = ValidatedPreflight {
            support: containment_support(&request),
            bindings: FilesystemBindings::canonical_path(&policy).unwrap(),
            policy,
            child_environment: request.child_environment(),
        };
        for fault in [
            "valid",
            "nonce",
            "version",
            "short",
            "duplicate",
            "eof",
            "before-ready",
            "after-go",
            "death",
            "no-status-eof",
        ] {
            // A separate Ruby process speaks the actual pipe protocol. The retained
            // status FD case performs a real exec, so NOTE_EXEC alone cannot pass.
            let script = format!(
                r#"
                status = IO.for_fd(3); gate = IO.for_fd(4)
                status.close_on_exec = false
                nonce = [ARGV[1]].pack('H*')
                ready = [1, 1].pack('NN') + nonce + [0, 0].pack('NN')
                fault = {fault:?}
                sleep 30 if fault == 'before-ready'
                exit! 0 if fault == 'eof'
                ready.setbyte(8, ready.getbyte(8) ^ 1) if fault == 'nonce'
                ready.setbyte(3, 2) if fault == 'version'
                ready = ready[0, 31] if fault == 'short'
                status.syswrite(ready)
                exit! 0 if fault == 'short' || fault == 'death'
                status.syswrite(ready) if fault == 'duplicate'
                gate.read(32)
                sleep 30 if fault == 'after-go'
                status.close_on_exec = fault != 'no-status-eof'
                exec('/bin/sleep', fault == 'no-status-eof' ? '30' : '0.01')
            "#
            );
            let mut command = Command::new("/usr/bin/ruby");
            command.args(["--disable-gems", "-e", &script, "--"]);
            let confirmation = ExecConfirmation::prepare(&mut command).unwrap();
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut lifecycle = MacosLifecycle {
                request: request.clone(),
                preflight: &preflight,
                launch: Some((command, confirmation)),
                child: None,
                process_group: None,
                cleanup_attempted: false,
                cleanup_observed: false,
            };
            let start = Instant::now();
            let result = lifecycle.execute();
            let pid = lifecycle.process_group.unwrap() as u32;
            let cleanup = lifecycle.cleanup();
            if fault == "valid" {
                assert!(result.is_ok(), "valid real-pipe control failed: {result:?}");
            } else {
                assert!(result.is_err(), "{fault} issued a receipt");
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "{fault} was unbounded"
            );
            if fault != "valid" {
                assert_eq!(
                    cleanup.cleanup_confidence(),
                    CleanupConfidence::BestEffortObserved
                );
            }
            assert!(lifecycle.child.is_none());
            assert_eq!(
                unsafe { libc::kill(pid as i32, 0) },
                -1,
                "{fault} child survived"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reserved_execution_failures_leave_no_receipt_or_private_directory() {
        const CHILD: &str = "TAPID_TEST_RESERVED_FAILURE";
        if std::env::var_os(CHILD).is_some() {
            let root = fs::canonicalize(".").unwrap();
            for fault in ["preparation", "exec", "protocol"] {
                let program = if fault == "protocol" {
                    OsString::from("--test-death-after-ready")
                } else {
                    root.join("missing-target").into_os_string()
                };
                let mut request = ExecutionRequest::builder(program)
                    .project_root(&root)
                    .trusted_node_runtime(root.join("node"))
                    .executable_search_path(&root)
                    .policy(policy(false, true, Vec::new()))
                    .build()
                    .unwrap();
                if fault == "preparation" {
                    request.launcher = None;
                }
                let start = Instant::now();
                let result = super::super::execute(&request);
                assert!(result.is_err(), "{fault} issued a receipt");
                assert!(start.elapsed() < Duration::from_secs(8));
                if fault != "preparation" {
                    assert_eq!(
                        result
                            .unwrap_err()
                            .completion()
                            .unwrap()
                            .cleanup_confidence(),
                        CleanupConfidence::BestEffortObserved
                    );
                }
                assert_eq!(
                    fs::read_dir(std::env::temp_dir()).unwrap().count(),
                    0,
                    "{fault} leaked reserved directory"
                );
            }
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let root = temp_project("reserved-failure");
        let temporary = temp_project("reserved-failure-tmp");
        fs::write(root.join("node"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(root.join("node"), fs::Permissions::from_mode(0o700)).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "execution::platform_backend::tests::reserved_execution_failures_leave_no_receipt_or_private_directory", "--nocapture"])
            .current_dir(&root).env(CHILD, "1").env("TMPDIR", &temporary).output().unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(temporary).unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn reserved_hardlink_creation_failure_removes_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_project("link-failure");
        let path = root.join("node");
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = super::super::TrustedNodeRuntime::checked(&path).unwrap();
        let mut created = None;
        let result = ReservedNode::create_with_link(&runtime, &root, |_, target| {
            let directory = target.parent().unwrap().to_path_buf();
            assert!(directory.is_dir());
            created = Some(directory);
            Err(std::io::Error::from_raw_os_error(libc::EXDEV))
        });
        let error = result.err().expect("injected hard-link failure succeeded");
        assert!(
            error
                .to_string()
                .contains("cannot create reserved node hard link")
        );
        assert!(
            !created.unwrap().exists(),
            "creation failure leaked directory"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_launch_revalidates_identities_and_cleans_failed_attempts() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = temp_project("prepared-tamper");
        let runtime = root.join("node");
        fs::write(&runtime, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        for fault in ["helper", "binding", "spawn", "preparation"] {
            let mut request = ExecutionRequest::builder("/bin/sh")
                .args(["-c", ": > marker"])
                .project_root(&root)
                .trusted_node_runtime(&runtime)
                .executable_search_path(&root)
                .policy(policy(false, true, Vec::new()))
                .build()
                .unwrap();
            let binding =
                ReservedNode::create(request.trusted_node_runtime.as_ref().unwrap(), &root)
                    .unwrap();
            let directory = binding.evidence.directory.clone();
            request.reserved_node = Some(binding.evidence.clone());
            request.executable_search_paths.insert(0, directory.clone());
            let helper_path = root.join("helper");
            fs::copy(std::env::current_exe().unwrap(), &helper_path).unwrap();
            let metadata = fs::metadata(&helper_path).unwrap();
            let helper = request.launcher.as_mut().unwrap();
            helper.executable = Some(helper_path.clone());
            helper.identity = Some((metadata.dev(), metadata.ino()));
            let additions = PlatformBackend
                .runtime_filesystem_additions(&request)
                .unwrap();
            let policy = super::super::resolve_policy(&request, additions).unwrap();
            let preflight = ValidatedPreflight {
                support: containment_support(&request),
                bindings: PlatformBackend.bind_filesystem(&request, &policy).unwrap(),
                policy,
                child_environment: request.child_environment(),
            };
            let profile = compile_profile(&request, &preflight).unwrap();
            if fault == "preparation" {
                fs::remove_file(&helper_path).unwrap();
                assert!(prepare_launch(&request, &preflight, &profile).is_err());
            } else {
                let mut launch = prepare_launch(&request, &preflight, &profile).unwrap();
                match fault {
                    "helper" => {
                        fs::rename(&helper_path, root.join("old-helper")).unwrap();
                        fs::write(&helper_path, "#!/bin/sh\n: > marker\n").unwrap();
                        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700))
                            .unwrap();
                    }
                    "binding" => {
                        fs::remove_file(directory.join("node")).unwrap();
                        fs::write(directory.join("node"), "replacement").unwrap();
                    }
                    "spawn" => {
                        launch.0.current_dir(root.join("absent"));
                    }
                    _ => unreachable!(),
                }
                let result = OwnedExecutionAttempt::new(
                    &preflight,
                    Box::new(MacosLifecycle {
                        request,
                        preflight: &preflight,
                        launch: Some(launch),
                        child: None,
                        process_group: None,
                        cleanup_attempted: false,
                        cleanup_observed: false,
                    }),
                )
                .finish();
                let error = result.expect_err("failed launch issued receipt");
                assert!(
                    error.completion().is_some(),
                    "owned attempt did not clean up"
                );
            }
            assert!(!root.join("marker").exists());
            drop(binding);
            assert!(!directory.exists(), "{fault} left reserved directory");
            let _ = fs::remove_file(&helper_path);
            let _ = fs::remove_file(root.join("old-helper"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_protocol_rejects_version_nonce_kind_and_short_frames() {
        let nonce = [7; 16];
        let ready = protocol_frame(1, nonce, 0);
        assert!(validate_frame(&ready, 1, nonce).is_ok());
        for index in [0, 4, 8, 24] {
            let mut malformed = ready;
            malformed[index] ^= 1;
            assert!(validate_frame(&malformed, 1, nonce).is_err());
        }
        assert!(validate_frame(&ready[..31], 1, nonce).is_err());
        assert!(validate_frame(&[ready.as_slice(), &[0]].concat(), 1, nonce).is_err());
    }

    #[test]
    fn missing_early_initializer_fails_before_spawn() {
        let root = temp_project("missing-init");
        let mut request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", "touch marker"])
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        request.launcher = None;
        let error = super::super::execute(&request).unwrap_err();
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert!(!root.join("marker").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn support_evidence_identifies_native_generated_policy_controls() {
        let root = temp_project("native-probes");
        let request = ExecutionRequest::builder("/bin/sh")
            .project_root(&root)
            .policy(policy(false, false, Vec::new()))
            .build()
            .unwrap();
        let support = containment_support(&request);
        assert!(support.is_supported(), "{support:?}");
        for evidence in support.observed_evidence() {
            assert!(
                evidence
                    .mechanism()
                    .contains("native generated deny-default"),
                "{evidence:?}"
            );
            assert!(
                evidence
                    .limitations()
                    .iter()
                    .any(|text| text.contains("sampled"))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_native_profile_fails_before_target_marker() {
        let root = temp_project("native-malformed");
        let marker = root.join("marker");
        let mut profile = probe_profile(&root, true).unwrap();
        profile.text = "(version 1)(not-a-seatbelt-operation)".into();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf untrusted > \"$1\"", "target"])
            .arg(&marker);
        assert_eq!(
            profile.configure(&mut command).unwrap_err().category(),
            ExecutionErrorCategory::Spawn
        );
        let request = ExecutionRequest::builder("/bin/sh")
            .args([
                OsString::from("-c"),
                OsString::from("printf untrusted > \"$1\""),
                OsString::from("target"),
                marker.clone().into_os_string(),
            ])
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let additions = PlatformBackend
            .runtime_filesystem_additions(&request)
            .unwrap();
        let policy = super::super::resolve_policy(&request, additions).unwrap();
        let preflight = ValidatedPreflight {
            support: containment_support(&request),
            bindings: FilesystemBindings::canonical_path(&policy).unwrap(),
            policy,
            child_environment: request.child_environment(),
        };
        let launch = prepare_launch(&request, &preflight, &profile).unwrap();
        let result = OwnedExecutionAttempt::new(
            &preflight,
            Box::new(MacosLifecycle {
                request,
                preflight: &preflight,
                launch: Some(launch),
                child: None,
                process_group: None,
                cleanup_attempted: false,
                cleanup_observed: false,
            }),
        )
        .finish();
        assert_eq!(
            result.unwrap_err().category(),
            ExecutionErrorCategory::Spawn
        );
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_execution_preserves_empty_path_and_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let root = temp_project("native-bytes");
        let value = OsString::from_vec(vec![b'a', 0xff, b' ', b'\'']);
        let request = ExecutionRequest::builder("/bin/sh")
            .args([
                OsString::from("-c"),
                OsString::from("test -z \"$PATH\" && printf '%s' \"$1\""),
                OsString::from("target"),
                value.clone(),
            ])
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let outcome = super::super::execute(&request).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert_eq!(outcome.stdout(), value.as_bytes());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn death_after_profile_application_without_exec_never_issues_a_receipt() {
        let root = temp_project("preexec-death");
        let request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", "exit 0"])
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let additions = PlatformBackend
            .runtime_filesystem_additions(&request)
            .unwrap();
        let policy = super::super::resolve_policy(&request, additions).unwrap();
        let bindings = FilesystemBindings::canonical_path(&policy).unwrap();
        let preflight = ValidatedPreflight {
            support: containment_support(&request),
            policy,
            bindings,
            child_environment: request.child_environment(),
        };
        let mut profile = compile_profile(&request, &preflight).unwrap();
        profile.abort_before_exec = true;
        let launch = prepare_launch(&request, &preflight, &profile).unwrap();
        let mut lifecycle = MacosLifecycle {
            request,
            preflight: &preflight,
            launch: Some(launch),
            child: None,
            process_group: None,
            cleanup_attempted: false,
            cleanup_observed: false,
        };
        let result = lifecycle.execute();
        lifecycle.cleanup();
        assert!(result.is_err(), "death before exec produced a receipt");
        assert_eq!(
            result.unwrap_err().category(),
            ExecutionErrorCategory::Spawn
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launch_confirmation_distinguishes_exec_failure_from_target_exit_71() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_project("exec-confirmation");
        let noexec = root.join("noexec");
        fs::write(&noexec, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&noexec, fs::Permissions::from_mode(0o600)).unwrap();
        for (target, errno) in [(root.join("missing"), libc::ENOENT), (noexec, libc::EACCES)] {
            let request = ExecutionRequest::builder(target)
                .project_root(&root)
                .policy(policy(false, true, Vec::new()))
                .build()
                .unwrap();
            let result = super::super::execute(&request);
            assert!(result.is_err(), "exec failure issued receipt: {result:?}");
            let error = result.unwrap_err();
            assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
            assert!(
                error.to_string().contains(&format!("errno {errno}:")),
                "{error}"
            );
        }
        for code in [0, 71] {
            let request = ExecutionRequest::builder("/bin/sh")
                .args([
                    "-c",
                    &format!("test ! -e /dev/fd/3 && test ! -e /dev/fd/4 || exit 99; exit {code}"),
                ])
                .project_root(&root)
                .policy(policy(false, true, Vec::new()))
                .build()
                .unwrap();
            assert_eq!(
                super::super::execute(&request).unwrap().termination(),
                &Termination::Exited(code)
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compiled_filesystem_rules_match_typed_effective_authority() {
        let root = temp_project("typed-authority");
        let request = ExecutionRequest::builder("/bin/sh")
            .project_root(&root)
            .policy(policy(false, true, Vec::new()))
            .build()
            .unwrap();
        let additions = PlatformBackend
            .runtime_filesystem_additions(&request)
            .unwrap();
        let resolved = super::super::resolve_policy(&request, additions).unwrap();
        let bindings = FilesystemBindings::canonical_path(&resolved).unwrap();
        let receipt = bindings.receipt();
        let roots: Vec<_> = receipt
            .grants()
            .iter()
            .filter(|g| g.path() == Path::new("/"))
            .collect();
        assert_eq!(
            roots.len(),
            2,
            "root data and global metadata need distinct evidence"
        );
        assert_eq!(
            receipt
                .read()
                .filter(|grant| grant.path() == Path::new("/"))
                .count(),
            2,
            "read authority iterator must include data and metadata grants"
        );
        assert_eq!(
            (roots[0].access(), roots[0].kind()),
            (
                FilesystemAccess::ReadData,
                FilesystemGrantKind::ExactDirectory
            )
        );
        assert_eq!(
            (roots[1].access(), roots[1].kind()),
            (
                FilesystemAccess::ReadMetadata,
                FilesystemGrantKind::DirectorySubtree
            )
        );
        let preflight = ValidatedPreflight {
            support: containment_support(&request),
            policy: resolved,
            bindings,
            child_environment: request.child_environment(),
        };
        let compiled = compile_profile(&request, &preflight).unwrap();
        assert_eq!(
            compiled
                .text
                .lines()
                .filter(|line| line.starts_with("(allow file-"))
                .count(),
            receipt.grants().len() + 1
        );
        for (index, grant) in receipt.grants().iter().enumerate() {
            let operation = match grant.access() {
                FilesystemAccess::ReadData => "file-read-data",
                FilesystemAccess::ReadMetadata => "file-read-metadata",
                FilesystemAccess::Read => "file-read*",
                FilesystemAccess::Write => "file-write*",
            };
            let filter = if grant.kind() == FilesystemGrantKind::DirectorySubtree {
                "subpath"
            } else {
                "literal"
            };
            assert!(compiled.text.contains(&format!(
                "(allow {operation} ({filter} (param \"G{index}\")))"
            )));
            assert_eq!(compiled.parameters[index].1, grant.path().as_os_str());
        }
        assert!(receipt.grants().iter().any(|g| g.path() == root
            && g.source() == super::super::FilesystemGrantSource::ProjectPolicy));
        fs::remove_dir_all(root).unwrap();
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
        let root = PathBuf::from(format!("/private/tmp/tapid-network-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let unix_path = root.join("s");
        let _ = fs::remove_file(&unix_path);
        let scripts = [
            "require 'socket'; TCPServer.new('127.0.0.1', 0)".to_owned(),
            format!("require 'socket'; TCPSocket.new('127.0.0.1', {port})"),
            format!("require 'socket'; UNIXServer.new({unix_path:?})"),
            "require 'socket'; s=UDPSocket.new; s.connect('127.0.0.1',53)".to_owned(),
            "require 'socket'; s=Socket.new(:INET,:STREAM); begin; s.connect_nonblock(Socket.sockaddr_in(9,'192.0.2.1')); rescue IO::WaitWritable; exit 0; end".to_owned(),
        ];
        for script in scripts {
            let allowed = run_ruby(&root, &script, true, true);
            assert_eq!(
                allowed.termination(),
                &Termination::Exited(0),
                "positive control: {script}"
            );
            if unix_path.exists() {
                fs::remove_file(&unix_path).unwrap();
            }
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
