use super::{
    AssuranceLevel, BackendIdentity, CleanupConfidence, CompletionEvidence, ContainmentSupport,
    EnforcementDimensions, EnforcementReceipt, ExecutionBackend, ExecutionError,
    ExecutionErrorCategory, ExecutionLifecycle, ExecutionOutcome, ExecutionRequest,
    FilesystemAccess, FilesystemBindings, FilesystemGrantKind, OwnedExecutionAttempt,
    PreparationError, ResolvedSandboxPolicy, RuntimeFilesystemAdditions, Termination,
    ValidatedPreflight, evidence_for_dimensions,
};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;
const LANDLOCK_EXECUTE: u64 = 1 << 0;
const LANDLOCK_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_READ_FILE: u64 = 1 << 2;
const LANDLOCK_READ_DIR: u64 = 1 << 3;
const LANDLOCK_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_REFER: u64 = 1 << 13;
const LANDLOCK_TRUNCATE: u64 = 1 << 14;
const LANDLOCK_READ: u64 = LANDLOCK_EXECUTE | LANDLOCK_READ_FILE | LANDLOCK_READ_DIR;
const LANDLOCK_WRITE: u64 = LANDLOCK_WRITE_FILE
    | LANDLOCK_REMOVE_DIR
    | LANDLOCK_REMOVE_FILE
    | LANDLOCK_MAKE_CHAR
    | LANDLOCK_MAKE_DIR
    | LANDLOCK_MAKE_REG
    | LANDLOCK_MAKE_SOCK
    | LANDLOCK_MAKE_FIFO
    | LANDLOCK_MAKE_BLOCK
    | LANDLOCK_MAKE_SYM
    | LANDLOCK_REFER
    | LANDLOCK_TRUNCATE;
const LANDLOCK_HANDLED: u64 = LANDLOCK_READ | LANDLOCK_WRITE;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
const PR_SET_SECCOMP: libc::c_int = 22;
const PRIVATE_LAUNCHER_MARKER: &str = "--tapid-private-linux-procfs-v1";
const LANDLOCK_RULESET_FD: libc::c_int = 198;
const PRIVATE_REPORT_FD: libc::c_int = 199;
const PRIVATE_REPORT_MAGIC: &[u8; 4] = b"TPMS";
const PRIVATE_REPORT_FRAME_BYTES: usize = 12;
const PRIVATE_REPORT_READY: u32 = 1;
const PRIVATE_REPORT_SETUP_ERROR: u32 = 2;
const PRIVATE_REPORT_EXEC_ERROR: u32 = 3;
const LIMITATIONS: &[&str] = &[
    "Landlock grants use path bindings checked against held filesystem identities before setup",
    "configured output, process-count, and memory limits are unsupported without writable per-tree cgroup v2 delegation; ManagedTree timeouts kill the namespace supervisor and rely on kernel --kill-child cleanup",
    "network-disabled policy denies Internet socket creation, connection, binding, listening, accepts, sendto, and recvfrom; AF_UNIX socketpairs with sendmsg/recvmsg and shutdown remain available for local runtime IPC; enabled networking is unrestricted",
    "Restricted does not own or guarantee cleanup of detached descendants; ManagedTree cleanup relies on the kernel PID namespace boundary",
    "standard streams remain connected; other inherited descriptors are closed",
];

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}
#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: libc::c_int,
    reserved: u32,
}

struct LandlockRuleset(OwnedFd);

impl LandlockRuleset {
    fn new() -> Result<Self, ExecutionError> {
        let abi = landlock_abi().ok_or_else(|| unsupported("Landlock is unavailable"))?;
        if abi < 3 {
            return Err(unsupported("Landlock ABI 3 or newer is required"));
        }
        let attr = RulesetAttr {
            handled_access_fs: LANDLOCK_HANDLED,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            ) as libc::c_int
        };
        if fd < 0 {
            return Err(unsupported("cannot create Landlock ruleset"));
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn add_grants(
        &self,
        policy: &ResolvedSandboxPolicy,
        bindings: &FilesystemBindings,
    ) -> Result<(), ExecutionError> {
        let grants = policy.read.iter().chain(&policy.write).collect::<Vec<_>>();
        if grants.len() != bindings.grants.len() {
            return Err(policy_error(
                "filesystem bindings differ from resolved policy",
            ));
        }
        for (grant, binding) in grants.into_iter().zip(&bindings.grants) {
            let held = binding.held.as_ref().ok_or_else(|| {
                unsupported("Landlock requires held native filesystem identities")
            })?;
            let original = held
                .metadata()
                .map_err(|e| policy_error(&format!("cannot inspect held filesystem grant: {e}")))?;
            let path = open_path(&grant.path)?;
            let current = path
                .metadata()
                .map_err(|e| policy_error(&format!("cannot inspect filesystem grant: {e}")))?;
            if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
                return Err(policy_error(
                    "filesystem grant identity changed before Landlock setup",
                ));
            }
            let access = match grant.access {
                FilesystemAccess::Read => {
                    if grant.kind == FilesystemGrantKind::ExactFile {
                        LANDLOCK_READ_FILE
                    } else {
                        LANDLOCK_READ
                    }
                }
                FilesystemAccess::ReadData => LANDLOCK_READ_FILE,
                FilesystemAccess::ReadMetadata => {
                    return Err(unsupported("Landlock cannot enforce metadata-only grants"));
                }
                FilesystemAccess::Write => {
                    if grant.kind == FilesystemGrantKind::ExactFile {
                        LANDLOCK_WRITE_FILE | LANDLOCK_TRUNCATE
                    } else {
                        LANDLOCK_WRITE
                    }
                }
            };
            let attr = PathBeneathAttr {
                allowed_access: access,
                parent_fd: path.as_raw_fd(),
                reserved: 0,
            };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    self.0.as_raw_fd(),
                    LANDLOCK_RULE_PATH_BENEATH,
                    &attr,
                    0u32,
                )
            };
            if result < 0 {
                return Err(unsupported("cannot add required Landlock path rule"));
            }
        }
        Ok(())
    }
}

fn open_path(path: &Path) -> Result<File, ExecutionError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    options.open(path).map_err(|e| {
        policy_error(&format!(
            "cannot open filesystem grant {}: {e}",
            path.display()
        ))
    })
}

fn landlock_abi() -> Option<i32> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        ) as i32
    };
    (result >= 0).then_some(result)
}

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

fn seccomp_filter(
    network: bool,
    subprocess: bool,
) -> Result<Vec<libc::sock_filter>, ExecutionError> {
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return Err(unsupported(
        "seccomp is supported only on x86_64 and aarch64",
    ));
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut filter = vec![
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 4,
            },
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 1,
                jf: 0,
                k: AUDIT_ARCH,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_KILL_PROCESS,
            },
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 0,
            },
        ];
        let mut denied = vec![
            libc::SYS_bpf,
            libc::SYS_ptrace,
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_open_tree,
            libc::SYS_move_mount,
            libc::SYS_fsopen,
            libc::SYS_fsconfig,
            libc::SYS_fsmount,
            libc::SYS_mount_setattr,
            libc::SYS_pivot_root,
            libc::SYS_setns,
            libc::SYS_unshare,
            libc::SYS_keyctl,
            libc::SYS_perf_event_open,
            libc::SYS_open_by_handle_at,
            libc::SYS_io_uring_setup,
        ];
        if !network {
            // AF_UNIX socketpairs are allowed; sendmsg/recvmsg/shutdown support local runtime IPC.
            // socket(), binding, listening, connecting, accepting, sendto, and recvfrom stay denied.
            filter.extend([
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 3,
                    k: libc::SYS_socketpair as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 16, // seccomp_data.args[0] (domain)
                },
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 1,
                    jf: 0,
                    k: libc::AF_UNIX as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
                },
            ]);
            denied.extend([
                libc::SYS_socket,
                libc::SYS_connect,
                libc::SYS_accept,
                libc::SYS_accept4,
                libc::SYS_bind,
                libc::SYS_listen,
                libc::SYS_sendto,
                libc::SYS_recvfrom,
            ]);
        }
        if !subprocess {
            denied.extend([libc::SYS_clone, libc::SYS_clone3]);
            #[cfg(target_arch = "x86_64")]
            denied.extend([libc::SYS_fork, libc::SYS_vfork]);
        }
        for syscall in denied {
            filter.push(libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: syscall as u32,
            });
            filter.push(libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
            });
        }
        filter.push(libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
        if filter.len() > u16::MAX as usize {
            return Err(unsupported("seccomp filter exceeds kernel limit"));
        }
        Ok(filter)
    }
}

fn install_restrictions(
    ruleset: libc::c_int,
    filter: &[libc::sock_filter],
    preserve_fd: Option<libc::c_int>,
) -> io::Result<()> {
    if unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) } < 0 {
        return Err(io::Error::last_os_error());
    }
    match preserve_fd {
        Some(fd) if fd > 3 => {
            if unsafe { libc::syscall(libc::SYS_close_range, 3u32, (fd - 1) as u32, 0u32) } < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::syscall(libc::SYS_close_range, (fd + 1) as u32, u32::MAX, 0u32) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid preserved descriptor",
            ));
        }
        None => {
            if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    let mut program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };
    if unsafe { libc::prctl(PR_SET_SECCOMP, 2, &mut program) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn add_private_procfs_read_rule(ruleset: libc::c_int) -> io::Result<()> {
    const PROC_ROOT: &[u8] = b"/proc\0";
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat,
            libc::AT_FDCWD,
            PROC_ROOT.as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) as libc::c_int
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let attr = PathBeneathAttr {
        allowed_access: LANDLOCK_READ,
        parent_fd: fd,
        reserved: 0,
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset,
            LANDLOCK_RULE_PATH_BENEATH,
            &attr,
            0u32,
        )
    };
    let error = (result < 0).then(io::Error::last_os_error);
    unsafe {
        libc::close(fd);
    }
    if let Some(error) = error {
        Err(error)
    } else {
        Ok(())
    }
}

fn namespace_identity(name: &str) -> io::Result<(u64, u64)> {
    let metadata = fs::metadata(format!("/proc/self/ns/{name}"))?;
    Ok((metadata.dev(), metadata.ino()))
}

fn establish_private_read_only_procfs(
    parent_pid_namespace: (u64, u64),
    parent_mount_namespace: (u64, u64),
) -> io::Result<()> {
    if namespace_identity("pid")? == parent_pid_namespace
        || namespace_identity("mnt")? == parent_mount_namespace
        || unsafe { libc::getpid() } != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private PID or mount namespace was not established",
        ));
    }
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/proc".as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut proc_stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c"/proc".as_ptr(), &mut proc_stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if proc_stats.f_flag & libc::ST_RDONLY as libc::c_ulong == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private procfs mount is not read-only",
        ));
    }
    Ok(())
}

fn write_private_report(fd: libc::c_int, kind: u32, value: i32) -> io::Result<()> {
    let mut frame = [0_u8; PRIVATE_REPORT_FRAME_BYTES];
    frame[..4].copy_from_slice(PRIVATE_REPORT_MAGIC);
    frame[4..8].copy_from_slice(&kind.to_le_bytes());
    frame[8..12].copy_from_slice(&value.to_le_bytes());
    let mut offset = 0;
    while offset < frame.len() {
        let result =
            unsafe { libc::write(fd, frame[offset..].as_ptr().cast(), frame.len() - offset) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "private launcher report pipe closed",
            ));
        }
        offset += result as usize;
    }
    Ok(())
}

fn parse_private_bool(value: OsString, name: &str) -> io::Result<bool> {
    match value.to_str() {
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid private launcher {name}"),
        )),
    }
}

fn parse_private_number<T: std::str::FromStr>(value: OsString, name: &str) -> io::Result<T> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid private launcher {name}"),
            )
        })
}

fn private_launcher_setup(
    arguments: Vec<OsString>,
) -> io::Result<(libc::c_int, OsString, Vec<OsString>)> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(OsStr::new(PRIVATE_LAUNCHER_MARKER)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private launcher marker is missing",
        ));
    }
    let network = parse_private_bool(
        arguments
            .next()
            .ok_or_else(|| io::Error::other("missing network flag"))?,
        "network flag",
    )?;
    let subprocess = parse_private_bool(
        arguments
            .next()
            .ok_or_else(|| io::Error::other("missing subprocess flag"))?,
        "subprocess flag",
    )?;
    let allow_proc_read = parse_private_bool(
        arguments
            .next()
            .ok_or_else(|| io::Error::other("missing private procfs read flag"))?,
        "private procfs read flag",
    )?;
    let ruleset = parse_private_number::<libc::c_int>(
        arguments
            .next()
            .ok_or_else(|| io::Error::other("missing ruleset descriptor"))?,
        "ruleset descriptor",
    )?;
    let report_fd = parse_private_number::<libc::c_int>(
        arguments
            .next()
            .ok_or_else(|| io::Error::other("missing report descriptor"))?,
        "report descriptor",
    )?;
    let parent_pid_namespace = (
        parse_private_number::<u64>(
            arguments
                .next()
                .ok_or_else(|| io::Error::other("missing PID namespace device"))?,
            "PID namespace device",
        )?,
        parse_private_number::<u64>(
            arguments
                .next()
                .ok_or_else(|| io::Error::other("missing PID namespace inode"))?,
            "PID namespace inode",
        )?,
    );
    let parent_mount_namespace = (
        parse_private_number::<u64>(
            arguments
                .next()
                .ok_or_else(|| io::Error::other("missing mount namespace device"))?,
            "mount namespace device",
        )?,
        parse_private_number::<u64>(
            arguments
                .next()
                .ok_or_else(|| io::Error::other("missing mount namespace inode"))?,
            "mount namespace inode",
        )?,
    );
    let program = arguments
        .next()
        .ok_or_else(|| io::Error::other("missing target program"))?;
    let program_arguments = arguments.collect::<Vec<_>>();
    if ruleset < 3 || report_fd < 3 || ruleset == report_fd {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid private launcher descriptors",
        ));
    }
    let mut report_stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(report_fd, &mut report_stat) } != 0
        || report_stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private launcher report descriptor is not a socket",
        ));
    }
    establish_private_read_only_procfs(parent_pid_namespace, parent_mount_namespace)?;
    if allow_proc_read {
        add_private_procfs_read_rule(ruleset)?;
    }
    let filter = seccomp_filter(network, subprocess)
        .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error.to_string()))?;
    install_restrictions(ruleset, &filter, Some(report_fd))?;
    Ok((report_fd, program, program_arguments))
}

fn private_launcher_entry(arguments: Vec<OsString>, report_fd: Option<libc::c_int>) -> ! {
    match private_launcher_setup(arguments) {
        Ok((report_fd, program, program_arguments)) => {
            if unsafe { libc::fcntl(report_fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                let error = io::Error::last_os_error();
                let _ = write_private_report(
                    report_fd,
                    PRIVATE_REPORT_SETUP_ERROR,
                    error.raw_os_error().unwrap_or(libc::EIO),
                );
                eprintln!("tapid: cannot protect private launcher report descriptor: {error}");
                unsafe { libc::_exit(125) }
            }
            if let Err(error) = write_private_report(report_fd, PRIVATE_REPORT_READY, 0) {
                eprintln!("tapid: cannot confirm private procfs setup: {error}");
                unsafe { libc::_exit(125) }
            }
            let error = Command::new(program).args(program_arguments).exec();
            let _ = write_private_report(
                report_fd,
                PRIVATE_REPORT_EXEC_ERROR,
                error.raw_os_error().unwrap_or(libc::EIO),
            );
            eprintln!("tapid: cannot exec restricted project script: {error}");
            unsafe { libc::_exit(127) }
        }
        Err(error) => {
            if let Some(report_fd) = report_fd {
                let _ = write_private_report(
                    report_fd,
                    PRIVATE_REPORT_SETUP_ERROR,
                    error.raw_os_error().unwrap_or(libc::EIO),
                );
            }
            eprintln!("tapid: cannot establish private read-only procfs: {error}");
            unsafe { libc::_exit(125) }
        }
    }
}

pub(crate) fn dispatch_private_launcher() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.first().map(OsString::as_os_str) != Some(OsStr::new(PRIVATE_LAUNCHER_MARKER)) {
        return;
    }
    let report_fd = arguments
        .get(5)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<libc::c_int>().ok());
    private_launcher_entry(arguments, report_fd);
}

fn parse_private_report(mut report: UnixStream) -> Result<(), ExecutionError> {
    let mut bytes = Vec::new();
    Read::by_ref(&mut report)
        .take((2 * PRIVATE_REPORT_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            unsupported(&format!("cannot read private procfs setup report: {error}"))
        })?;
    if bytes.is_empty()
        || bytes.len() > 2 * PRIVATE_REPORT_FRAME_BYTES
        || bytes.len() % PRIVATE_REPORT_FRAME_BYTES != 0
    {
        return Err(unsupported(
            "private PID/mount namespace setup did not provide confirmation",
        ));
    }
    let mut ready = false;
    let (frames, remainder) = bytes.as_chunks::<PRIVATE_REPORT_FRAME_BYTES>();
    if !remainder.is_empty() {
        return Err(unsupported("private procfs setup report was invalid"));
    }
    for frame in frames {
        if &frame[..4] != PRIVATE_REPORT_MAGIC {
            return Err(unsupported("private procfs setup report was invalid"));
        }
        let kind = u32::from_le_bytes(frame[4..8].try_into().expect("fixed report field"));
        let value = i32::from_le_bytes(frame[8..12].try_into().expect("fixed report field"));
        match kind {
            PRIVATE_REPORT_READY if !ready => ready = true,
            PRIVATE_REPORT_SETUP_ERROR => {
                return Err(unsupported(&format!(
                    "private read-only procfs setup failed (errno {value})"
                )));
            }
            PRIVATE_REPORT_EXEC_ERROR => {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    format!("cannot exec restricted project script (errno {value})"),
                ));
            }
            _ => return Err(unsupported("private procfs setup report was invalid")),
        }
    }
    if ready {
        Ok(())
    } else {
        Err(unsupported(
            "private PID/mount namespace setup did not provide confirmation",
        ))
    }
}

fn locate_unshare() -> Result<std::path::PathBuf, ExecutionError> {
    for candidate in ["/usr/bin/unshare", "/bin/unshare"] {
        let path = Path::new(candidate);
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return fs::canonicalize(path).map_err(|error| {
                unsupported(&format!("cannot resolve util-linux unshare: {error}"))
            });
        }
    }
    Err(unsupported(
        "process-memory-stat opt-in requires the util-linux unshare command",
    ))
}

fn unshare_namespace_arguments(is_root: bool) -> Vec<&'static str> {
    let mut arguments = Vec::with_capacity(8);
    if !is_root {
        arguments.extend(["--user", "--map-root-user"]);
    }
    arguments.extend([
        "--mount",
        "--pid",
        "--fork",
        "--kill-child",
        "--mount-proc",
        "--propagation",
        "private",
        "--",
    ]);
    arguments
}

fn duplicate_fd_above_private_range(fd: libc::c_int) -> Result<OwnedFd, ExecutionError> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, PRIVATE_REPORT_FD + 1) };
    if duplicate < 0 {
        return Err(unsupported(&format!(
            "cannot duplicate private launcher descriptor: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn private_memory_stats_command(
    request: &ExecutionRequest,
    ruleset: &OwnedFd,
) -> Result<(Command, UnixStream), ExecutionError> {
    let launcher = request
        .private_launcher_executable()
        .ok_or_else(|| unsupported("private launcher was not initialized"))?;
    let unshare = locate_unshare()?;
    let parent_pid_namespace = namespace_identity("pid")
        .map_err(|error| unsupported(&format!("cannot identify parent PID namespace: {error}")))?;
    let parent_mount_namespace = namespace_identity("mnt").map_err(|error| {
        unsupported(&format!("cannot identify parent mount namespace: {error}"))
    })?;
    let (report_reader, report_writer) = UnixStream::pair().map_err(|error| {
        unsupported(&format!(
            "cannot create private launcher report channel: {error}"
        ))
    })?;
    let ruleset_copy = duplicate_fd_above_private_range(ruleset.as_raw_fd())?;
    let report_copy = duplicate_fd_above_private_range(report_writer.as_raw_fd())?;
    let mut command = Command::new(unshare);
    command
        .args(unshare_namespace_arguments(unsafe { libc::geteuid() == 0 }))
        .arg(launcher)
        .arg(PRIVATE_LAUNCHER_MARKER)
        .arg(if request.policy().network() { "1" } else { "0" })
        .arg(if request.policy().subprocess() {
            "1"
        } else {
            "0"
        })
        .arg(if request.allow_process_memory_stats() {
            "1"
        } else {
            "0"
        })
        .arg(LANDLOCK_RULESET_FD.to_string())
        .arg(PRIVATE_REPORT_FD.to_string())
        .arg(parent_pid_namespace.0.to_string())
        .arg(parent_pid_namespace.1.to_string())
        .arg(parent_mount_namespace.0.to_string())
        .arg(parent_mount_namespace.1.to_string())
        .arg(request.program())
        .args(request.arguments());
    let ruleset_source = ruleset_copy.as_raw_fd();
    let report_source = report_copy.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            let _keep_open = (&ruleset_copy, &report_copy);
            if libc::dup2(ruleset_source, LANDLOCK_RULESET_FD) < 0
                || libc::dup2(report_source, PRIVATE_REPORT_FD) < 0
            {
                return Err(io::Error::last_os_error());
            }
            for fd in [LANDLOCK_RULESET_FD, PRIVATE_REPORT_FD] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    drop(report_writer);
    Ok((command, report_reader))
}

fn is_process_memory_stats_permission_error(stderr: &[u8]) -> bool {
    let contains = |needle: &[u8]| stderr.windows(needle.len()).any(|window| window == needle);
    contains(b"uv_resident_set_memory") && (contains(b"EACCES") || contains(b"permission denied"))
}

fn unsupported(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCategory::UnsupportedContainment, message)
}
fn policy_error(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCategory::PolicyViolation, message)
}

pub(super) struct PlatformBackend;
impl ExecutionBackend for PlatformBackend {
    fn containment_support(&self, request: &ExecutionRequest) -> ContainmentSupport {
        containment_support(request)
    }

    fn runtime_filesystem_additions(
        &self,
        _request: &ExecutionRequest,
    ) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
        let mut paths = [
            "/usr/bin",
            "/usr/lib",
            "/usr/lib64",
            "/lib",
            "/lib64",
            "/etc/ld.so.cache",
            "/etc/ssl/certs",
            "/etc/ssl/openssl.cnf",
            "/etc/localtime",
        ]
        .into_iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        RuntimeFilesystemAdditions::checked(paths, Vec::new())
    }

    fn bind_filesystem(
        &self,
        _request: &ExecutionRequest,
        policy: &ResolvedSandboxPolicy,
    ) -> Result<FilesystemBindings, ExecutionError> {
        FilesystemBindings::native_objects(policy)
    }

    fn prepare<'a>(
        &'a self,
        request: &ExecutionRequest,
        preflight: &'a ValidatedPreflight,
    ) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
        let ruleset = LandlockRuleset::new().map_err(PreparationError::from)?;
        ruleset
            .add_grants(&preflight.policy, &preflight.bindings)
            .map_err(PreparationError::from)?;
        let filter = seccomp_filter(request.policy().network(), request.policy().subprocess())
            .map_err(PreparationError::from)?;
        Ok(OwnedExecutionAttempt::new(
            preflight,
            Box::new(LinuxLifecycle {
                request: request.clone(),
                preflight,
                ruleset,
                filter,
                termination: None,
            }),
        ))
    }
}

fn backend_identity(assurance: AssuranceLevel) -> BackendIdentity {
    let name = match assurance {
        AssuranceLevel::Restricted => "tapid-runner/linux-landlock-seccomp-restricted",
        AssuranceLevel::ManagedTree => "tapid-runner/linux-landlock-seccomp-managed-tree",
    };
    BackendIdentity::new(
        name,
        format!(
            "{}; Landlock ABI {}",
            env!("CARGO_PKG_VERSION"),
            landlock_abi().unwrap_or(0)
        ),
        None,
    )
    .expect("static Linux identity is valid")
}

pub(super) fn containment_support(request: &ExecutionRequest) -> ContainmentSupport {
    let requested = EnforcementDimensions::requested_by(request.policy());
    let identity = backend_identity(request.policy().assurance());
    let unsupported = |reason: &str| {
        ContainmentSupport::unsupported(
            identity.clone(),
            "linux",
            reason,
            requested.clone(),
            EnforcementDimensions::none(),
            EnforcementDimensions::none(),
        )
    };
    if request.policy().assurance() == AssuranceLevel::ManagedTree {
        let limits = request.policy().limits();
        if limits.max_output_bytes().is_some()
            || limits.max_processes().is_some()
            || limits.max_memory_bytes().is_some()
        {
            return unsupported(
                "Linux ManagedTree does not support configured output, process-count, or memory limits without per-tree kernel controllers",
            );
        }
        if request.private_launcher_executable().is_none() || locate_unshare().is_err() {
            return unsupported(
                "Linux ManagedTree requires the initialized private launcher and util-linux unshare",
            );
        }
    }
    if landlock_abi().is_none_or(|abi| abi < 3) {
        return unsupported("Landlock ABI 3 or newer is unavailable");
    }
    let limits = request.policy().limits();
    if request.policy().assurance() == AssuranceLevel::Restricted
        && (limits.timeout_seconds().is_some()
            || limits.max_output_bytes().is_some()
            || limits.max_processes().is_some()
            || limits.max_memory_bytes().is_some())
    {
        return unsupported("Linux Restricted does not enforce configured resource limits");
    }
    if seccomp_filter(request.policy().network(), request.policy().subprocess()).is_err() {
        return unsupported("required seccomp architecture or filter is unavailable");
    }
    let evidence = evidence_for_dimensions(
        &requested,
        "Landlock ABI 3, no_new_privs, seccomp, explicit environment and close_range descriptor sanitation",
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

struct LinuxLifecycle<'a> {
    request: ExecutionRequest,
    preflight: &'a ValidatedPreflight,
    ruleset: LandlockRuleset,
    filter: Vec<libc::sock_filter>,
    termination: Option<Termination>,
}

fn wait_for_supervisor(
    child: &mut Child,
    timeout_seconds: Option<u64>,
) -> Result<(ExitStatus, bool), ExecutionError> {
    let Some(timeout_seconds) = timeout_seconds else {
        return child.wait().map(|status| (status, false)).map_err(|error| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot wait for restricted project script: {error}"),
            )
        });
    };
    let timeout = Duration::from_secs(timeout_seconds);
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot poll ManagedTree namespace supervisor: {error}"),
            )
        })? {
            return Ok((status, false));
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            if let Err(kill_error) = child.kill() {
                if let Some(status) = child.try_wait().map_err(|error| {
                    ExecutionError::new(
                        ExecutionErrorCategory::Spawn,
                        format!("cannot inspect ManagedTree supervisor after kill error: {error}"),
                    )
                })? {
                    return Ok((status, false));
                }
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Timeout,
                    format!(
                        "cannot kill ManagedTree namespace supervisor at timeout: {kill_error}"
                    ),
                ));
            }
            let status = child.wait().map_err(|error| {
                ExecutionError::new(
                    ExecutionErrorCategory::Timeout,
                    format!("cannot reap timed-out ManagedTree supervisor: {error}"),
                )
            })?;
            return Ok((status, true));
        }
        std::thread::sleep(Duration::from_millis(10).min(timeout.saturating_sub(elapsed)));
    }
}

impl ExecutionLifecycle for LinuxLifecycle<'_> {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        let allow_process_memory_stats = self.request.allow_process_memory_stats();
        let use_private_pid_namespace = self.request.policy().assurance()
            == AssuranceLevel::ManagedTree
            || allow_process_memory_stats;
        let (mut command, private_report) = if use_private_pid_namespace {
            let (command, report) = private_memory_stats_command(&self.request, &self.ruleset.0)?;
            (command, Some(report))
        } else {
            let mut command = Command::new(self.request.program());
            command.args(self.request.arguments());
            let ruleset = self.ruleset.0.try_clone().map_err(|e| {
                ExecutionError::new(
                    ExecutionErrorCategory::Spawn,
                    format!("cannot duplicate Landlock ruleset: {e}"),
                )
            })?;
            let filter = self.filter.clone();
            unsafe {
                command.pre_exec(move || install_restrictions(ruleset.as_raw_fd(), &filter, None));
            }
            (command, None)
        };
        command
            .current_dir(self.request.project_root())
            .env_clear()
            .envs(&self.preflight.child_environment)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot launch restricted project script: {e}"),
            )
        })?;
        drop(command);
        let mut child_stderr = child
            .stderr
            .take()
            .expect("stderr was configured as a pipe");
        let stderr_reader = std::thread::spawn(move || {
            const DETECTION_WINDOW: usize = 128;
            let mut output = io::stderr().lock();
            let mut carry = Vec::with_capacity(DETECTION_WINDOW);
            let mut buffer = [0_u8; 4096];
            let mut denied = false;
            loop {
                let count = child_stderr.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let _ = output.write_all(&buffer[..count]);
                let mut window = std::mem::take(&mut carry);
                window.extend_from_slice(&buffer[..count]);
                denied |= is_process_memory_stats_permission_error(&window);
                carry.extend_from_slice(&window[window.len().saturating_sub(DETECTION_WINDOW)..]);
            }
            let _ = output.flush();
            Ok::<bool, io::Error>(denied)
        });
        let (status, timed_out) =
            wait_for_supervisor(&mut child, self.request.policy().limits().timeout_seconds())?;
        let memory_stats_denied = stderr_reader
            .join()
            .map_err(|_| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "stderr diagnostic reader panicked",
                )
            })?
            .map_err(|e| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    format!("cannot read restricted project script stderr: {e}"),
                )
            })?;
        if let Some(report) = private_report {
            parse_private_report(report)?;
        }
        self.termination = Some(if timed_out {
            Termination::TimedOut
        } else if let Some(code) = status.code() {
            Termination::Exited(code)
        } else {
            Termination::Signaled(status.signal().unwrap_or(0))
        });
        let requested = self.preflight.support.requested();
        let process_memory_stats_evidence = if allow_process_memory_stats {
            "; explicit opt-in created private PID and mount namespaces, remounted procfs read-only, and granted Landlock read access only within that namespace-scoped procfs"
        } else {
            ""
        };
        let mechanism = if requested.process_tree_membership() {
            "Landlock and seccomp before target exec; target is PID 1 in a private namespace; timeout kills the util-linux supervisor and the kernel kills namespace descendants"
        } else {
            &format!(
                "Landlock and seccomp restrictions installed before target exec{process_memory_stats_evidence}"
            )
        };
        let receipt = EnforcementReceipt::checked(
            self.preflight,
            requested.clone(),
            evidence_for_dimensions(requested, mechanism, LIMITATIONS),
        )?;
        let completion = completion_for(self.preflight)?;
        Ok(Box::new(
            ExecutionOutcome::checked(
                self.termination.clone().unwrap(),
                Vec::new(),
                Vec::new(),
                receipt,
                completion,
            )?
            .with_process_memory_stats_hint(
                memory_stats_denied && !self.request.allow_process_memory_stats(),
            ),
        ))
    }

    fn cleanup(&mut self) -> CompletionEvidence {
        completion_for(self.preflight).expect("Restricted completion evidence is valid")
    }
}

fn completion_for(preflight: &ValidatedPreflight) -> Result<CompletionEvidence, ExecutionError> {
    let required = EnforcementDimensions::completion_required(preflight.support.requested());
    let managed = required.process_tree_membership();
    CompletionEvidence::checked(
        preflight,
        required.clone(),
        if managed {
            evidence_for_dimensions(
                &required,
                "kernel PID namespace teardown after ManagedTree init exits",
                &[],
            )
        } else {
            Vec::new()
        },
        if managed {
            CleanupConfidence::KernelOwnedComplete
        } else {
            CleanupConfidence::NotGuaranteed
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssuranceLevel, CleanupConfidence, ExecutionErrorCategory, ExecutionLimits,
        ExecutionRequest, FilesystemPolicy, SandboxMode, SandboxPolicy, Termination,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn execute(request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        super::super::execute_with_backend(request, &PlatformBackend)
    }

    fn root() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tapid-linux-restricted-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    fn request(root: &std::path::Path, command: &str, policy: SandboxPolicy) -> ExecutionRequest {
        ExecutionRequest::builder("/bin/sh")
            .args(["-c", command])
            .project_root(root)
            .executable_search_paths(["/usr/bin"])
            .policy(policy)
            .build()
            .unwrap()
    }

    fn restricted(_root: &std::path::Path, write: Vec<String>, network: bool) -> SandboxPolicy {
        SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::Restricted,
            FilesystemPolicy::new(vec![".".into()], write).unwrap(),
            network,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap()
    }

    fn managed_tree(_root: &std::path::Path, write: Vec<String>) -> SandboxPolicy {
        SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], write).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn network_denial_preserves_unix_ipc_but_blocks_inet_socket_creation() {
        let filter = seccomp_filter(false, true).unwrap();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
        if pid == 0 {
            unsafe {
                if libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::prctl(
                        PR_SET_SECCOMP,
                        libc::SECCOMP_MODE_FILTER,
                        &libc::sock_fprog {
                            len: filter.len() as u16,
                            filter: filter.as_ptr() as *mut libc::sock_filter,
                        },
                    ) != 0
                {
                    libc::_exit(1);
                }
                let mut fds = [-1; 2];
                if libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) != 0 {
                    libc::_exit(2);
                }
                let sent = b'x';
                let mut send_iov = libc::iovec {
                    iov_base: (&sent as *const u8).cast_mut().cast(),
                    iov_len: 1,
                };
                let mut send_message: libc::msghdr = std::mem::zeroed();
                send_message.msg_iov = &mut send_iov;
                send_message.msg_iovlen = 1;
                if libc::sendmsg(fds[0], &send_message, 0) != 1 {
                    libc::_exit(4);
                }
                let mut received = 0u8;
                let mut recv_iov = libc::iovec {
                    iov_base: (&mut received as *mut u8).cast(),
                    iov_len: 1,
                };
                let mut recv_message: libc::msghdr = std::mem::zeroed();
                recv_message.msg_iov = &mut recv_iov;
                recv_message.msg_iovlen = 1;
                if libc::recvmsg(fds[1], &mut recv_message, 0) != 1 || received != sent {
                    libc::_exit(5);
                }
                if libc::shutdown(fds[0], libc::SHUT_RDWR) != 0 {
                    libc::_exit(6);
                }
                libc::close(fds[0]);
                libc::close(fds[1]);
                if libc::socketpair(libc::AF_INET, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) != -1
                    || *libc::__errno_location() != libc::EPERM
                {
                    libc::_exit(3);
                }
                if libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) != -1
                    || *libc::__errno_location() != libc::EPERM
                {
                    libc::_exit(7);
                }
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "child status: {status}");
        assert_eq!(libc::WEXITSTATUS(status), 0, "child status: {status}");
    }

    #[test]
    fn root_namespace_launch_does_not_create_an_unprivileged_user_namespace() {
        assert_eq!(
            unshare_namespace_arguments(true),
            [
                "--mount",
                "--pid",
                "--fork",
                "--kill-child",
                "--mount-proc",
                "--propagation",
                "private",
                "--",
            ]
        );
        assert_eq!(
            &unshare_namespace_arguments(false)[..2],
            &["--user", "--map-root-user"]
        );
    }

    #[test]
    fn default_policy_keeps_current_process_memory_stats_denied() {
        let root = root();
        let req = ExecutionRequest::builder("/bin/cat")
            .arg("/proc/self/statm")
            .project_root(&root)
            .executable_search_paths(["/usr/bin"])
            .policy(restricted(&root, vec![], false))
            .build()
            .unwrap();
        let outcome = execute(&req).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(1));
        assert!(!outcome.process_memory_stats_hint());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opt_in_allows_only_current_process_memory_stats() {
        let root = root();
        let req = ExecutionRequest::builder("/bin/cat")
            .arg("/proc/self/statm")
            .project_root(&root)
            .executable_search_paths(["/usr/bin"])
            .policy(restricted(&root, vec![], false))
            .allow_process_memory_stats(true)
            .build()
            .unwrap();
        match execute(&req) {
            Ok(outcome) => assert_eq!(outcome.termination(), &Termination::Exited(0)),
            Err(error) => {
                assert_eq!(
                    error.category(),
                    ExecutionErrorCategory::UnsupportedContainment
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opt_in_does_not_allow_other_process_procfs_reads() {
        let root = root();
        let host_pid = std::process::id();
        let script = format!(
            "if [ -r /proc/{host_pid}/statm ]; then exit 41; fi; if printf x > /proc/self/comm 2>/dev/null; then exit 42; fi; exit 0"
        );
        let req = ExecutionRequest::builder("/bin/sh")
            .arg("-c")
            .arg(script)
            .project_root(&root)
            .executable_search_paths(["/usr/bin"])
            .policy(restricted(&root, vec![], false))
            .allow_process_memory_stats(true)
            .build()
            .unwrap();
        match execute(&req) {
            Ok(outcome) => assert_eq!(outcome.termination(), &Termination::Exited(0)),
            Err(error) => {
                assert_eq!(
                    error.category(),
                    ExecutionErrorCategory::UnsupportedContainment
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn process_memory_stats_permission_error_requires_libuv_signature_and_access_denial() {
        assert!(is_process_memory_stats_permission_error(
            b"[Error: EACCES: permission denied, uv_resident_set_memory]"
        ));
        assert!(!is_process_memory_stats_permission_error(
            b"EACCES: permission denied, open config.json"
        ));
        assert!(!is_process_memory_stats_permission_error(
            b"EIO: uv_resident_set_memory failed"
        ));
    }

    #[test]
    fn runner_streams_and_detects_libuv_process_memory_stats_denial() {
        let root = root();
        let req = request(
            &root,
            "printf '%s\\n' '[Error: EACCES: permission denied, uv_resident_set_memory]' >&2; exit 9",
            restricted(&root, vec![], false),
        );
        let outcome = execute(&req).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(9));
        assert!(outcome.process_memory_stats_hint());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restricted_backend_runs_real_child_and_issues_receipt() {
        let root = root();
        let req = request(
            &root,
            "printf allowed > marker",
            restricted(&root, vec![".".into()], false),
        );
        let outcome = execute(&req).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert_eq!(fs::read(root.join("marker")).unwrap(), b"allowed");
        assert_eq!(
            outcome.enforcement().backend().name(),
            "tapid-runner/linux-landlock-seccomp-restricted"
        );
        assert_eq!(
            outcome.enforcement().assurance(),
            AssuranceLevel::Restricted
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_allowlist_includes_openssl_configuration_when_present() {
        let root = root();
        let req = request(&root, "true", restricted(&root, vec![], false));
        let additions = PlatformBackend.runtime_filesystem_additions(&req).unwrap();
        if Path::new("/etc/ssl/openssl.cnf").exists() {
            assert!(
                additions
                    .read
                    .iter()
                    .any(|grant| grant.path == Path::new("/etc/ssl/openssl.cnf"))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restricted_backend_denies_write_outside_policy() {
        let root = root();
        let outside = root.with_extension("outside");
        let command = format!("printf denied > '{}'", outside.display());
        let req = request(&root, &command, restricted(&root, vec![], false));
        let outcome = execute(&req).unwrap();
        assert_ne!(outcome.termination(), &Termination::Exited(0));
        assert!(!outside.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_tree_support_reports_managed_tree_backend_identity() {
        let root = root();
        let req = request(&root, "true", managed_tree(&root, vec![]));
        let support = containment_support(&req);
        assert_eq!(
            support.backend().name(),
            "tapid-runner/linux-landlock-seccomp-managed-tree"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_tree_rejects_unavailable_memory_limits_before_target_execution() {
        let root = root();
        let marker = root.join("must-not-run");
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(None, None, None, Some(64 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let req = request(&root, &format!("touch '{}'", marker.display()), policy);
        let error = execute(&req).unwrap_err();
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supervisor_timeout_kills_pid_namespace_descendants() {
        let root = root();
        let marker = root.join("survived-supervisor-timeout");
        let unshare = locate_unshare().unwrap();
        let mut command = Command::new(unshare);
        command
            .args(unshare_namespace_arguments(unsafe { libc::geteuid() == 0 }))
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("sleep 2; touch '{}'", marker.display()))
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let (status, timed_out) = wait_for_supervisor(&mut child, Some(1)).unwrap();
        assert!(timed_out);
        assert!(!status.success());
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "a namespace descendant survived supervisor timeout"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_tree_timeout_kills_namespace_and_remaining_descendants() {
        let root = root();
        let marker = root.join("descendant-after-timeout");
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(1), None, None, None).unwrap(),
        )
        .unwrap();
        let req = request(
            &root,
            &format!("sleep 2; touch '{}'", marker.display()),
            policy,
        );
        let outcome = execute(&req).unwrap();
        assert_eq!(outcome.termination(), &Termination::TimedOut);
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "timed-out descendant survived the kernel cleanup boundary"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_tree_executes_and_records_kernel_owned_cleanup() {
        let root = root();
        let req = request(
            &root,
            &format!(
                "(sleep 1; echo survived > '{}') & exit 0",
                root.join("descendant-survived").display()
            ),
            managed_tree(&root, vec![".".into()]),
        );
        let outcome = execute(&req).expect("ManagedTree should be supported when namespaces work");
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert_eq!(
            outcome.enforcement().assurance(),
            AssuranceLevel::ManagedTree
        );
        assert_eq!(
            outcome.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(
            !root.join("descendant-survived").exists(),
            "a descendant ran after the ManagedTree init exited"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
