//! PID namespace ownership plus delegated cgroup v2 accounting.
use super::*;
use crate::ExecutionLimits;
use std::os::unix::fs::OpenOptionsExt;

#[path = "managed_filesystem.rs"]
mod filesystem;
use std::{
    path::PathBuf,
    process::Child,
    time::{Duration, Instant},
};

const LIMITATIONS: &[&str] = &[
    "requires a writable delegated cgroup v2 parent with memory and pids enabled, cgroup.kill, and private PID/mount namespaces",
    "pids.max counts kernel tasks including the trusted supervisor and runtime threads; memory.max accounts cgroup-charged memory and swap is disabled",
    "stdout and stderr share a ceiling of at most 16 MiB and are buffered until complete process cleanup",
    "cleanup waits for authoritative cgroup emptiness; uninterruptible kernel operations may delay cleanup",
    "enabled networking permits IPv4/IPv6 sockets and local stream socketpairs; host Unix sockets, clone3 cgroup reassignment, kernel-control filesystem writes, and credential changes are denied",
    "host administrators and concurrent host filesystem mutation remain outside the containment boundary",
];

struct Cgroup {
    path: PathBuf,
    directory: File,
}
impl Cgroup {
    fn prepare(limits: &ExecutionLimits) -> Result<Self, ExecutionError> {
        let membership = fs::read_to_string("/proc/self/cgroup")
            .map_err(|e| unsupported(&format!("cannot read cgroup membership: {e}")))?;
        let membership = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or_else(|| unsupported("cgroup v2 membership is unavailable"))?;
        let relative = Path::new(membership)
            .strip_prefix("/")
            .map_err(|_| unsupported("invalid cgroup membership"))?;
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(unsupported("noncanonical cgroup membership"));
        }
        let root = fs::canonicalize("/sys/fs/cgroup")
            .map_err(|e| unsupported(&format!("cannot resolve cgroup v2 mount: {e}")))?;
        let current = root.join(relative);
        let parent = if current == root {
            root.clone()
        } else {
            current.parent().unwrap().to_path_buf()
        };
        let parent = fs::canonicalize(parent)
            .map_err(|e| unsupported(&format!("cannot resolve delegated cgroup parent: {e}")))?;
        if !parent.starts_with(&root) {
            return Err(unsupported("delegated cgroup parent escapes cgroup mount"));
        }
        let mut random = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|e| unsupported(&format!("cannot reserve cgroup identity: {e}")))?;
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = parent.join(format!("tapid-managed-{name}"));
        fs::create_dir(&path).map_err(|e| {
            unsupported(&format!(
                "writable delegated cgroup v2 parent is required: {e}"
            ))
        })?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| unsupported(&format!("cannot hold cgroup identity: {e}")))?;
        let group = Self { path, directory };
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(group.directory.as_raw_fd(), &mut stat) } != 0
            || stat.f_type != libc::CGROUP2_SUPER_MAGIC as libc::c_long
        {
            return Err(unsupported("delegation is not cgroup v2"));
        }
        group.set(
            "pids.max",
            &limits
                .max_processes()
                .ok_or_else(|| unsupported("ManagedTree requires max-processes"))?
                .to_string(),
        )?;
        group.set(
            "memory.max",
            &limits
                .max_memory_bytes()
                .ok_or_else(|| unsupported("ManagedTree requires max-memory-bytes"))?
                .to_string(),
        )?;
        group.set("memory.swap.max", "0")?;
        group.set("memory.oom.group", "1")?;
        group.open("cgroup.kill", true)?;
        group.open("cgroup.procs", true)?;
        if group
            .read("cgroup.events")?
            .lines()
            .any(|line| line == "populated 1")
        {
            return Err(unsupported("fresh cgroup is unexpectedly populated"));
        }
        Ok(group)
    }
    fn open(&self, name: &str, write: bool) -> Result<File, ExecutionError> {
        let name = std::ffi::CString::new(name).expect("static cgroup filename");
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                if write {
                    libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW
                } else {
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW
                },
            )
        };
        if fd < 0 {
            return Err(unsupported(&format!(
                "required cgroup controller or interface unavailable: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn read(&self, name: &str) -> Result<String, ExecutionError> {
        let mut text = String::new();
        self.open(name, false)?
            .take(4096)
            .read_to_string(&mut text)
            .map_err(|e| unsupported(&format!("cannot read cgroup state: {e}")))?;
        Ok(text)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), ExecutionError> {
        self.open(name, true)?
            .write_all(value.as_bytes())
            .map_err(|e| unsupported(&format!("cannot establish cgroup limit: {e}")))?;
        if self.read(name)?.trim() != value {
            return Err(unsupported("cgroup limit readback does not match request"));
        }
        Ok(())
    }
    fn populated(&self) -> Result<bool, ExecutionError> {
        let state = self.read("cgroup.events")?;
        if state.lines().any(|line| line == "populated 0") {
            Ok(false)
        } else if state.lines().any(|line| line == "populated 1") {
            Ok(true)
        } else {
            Err(unsupported("cgroup population evidence is missing"))
        }
    }
    fn kill(&self) -> Result<(), ExecutionError> {
        self.open("cgroup.kill", true)?
            .write_all(b"1")
            .map_err(|e| unsupported(&format!("cannot kill managed cgroup: {e}")))
    }
    fn exceeded(&self, file: &str, event: &str) -> Result<bool, ExecutionError> {
        Ok(self.read(file)?.lines().any(|line| {
            line.split_once(' ').is_some_and(|(name, value)| {
                name == event && value.parse::<u64>().is_ok_and(|value| value > 0)
            })
        }))
    }
    fn wait_empty(&self) -> Result<(), ExecutionError> {
        while self.populated()? {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}
impl Drop for Cgroup {
    fn drop(&mut self) {
        // Kernel cgroup directories cannot be removed while populated. Never
        // kill an unverified path: all control accesses use the held directory.
        if self.populated().is_ok_and(|populated| !populated)
            && let (Ok(held), Ok(current)) =
                (self.directory.metadata(), fs::symlink_metadata(&self.path))
            && (held.dev(), held.ino()) == (current.dev(), current.ino())
        {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn identity() -> BackendIdentity {
    BackendIdentity::new(
        "tapid-runner/linux-pidns-cgroup-v2-managed",
        env!("CARGO_PKG_VERSION"),
        None,
    )
    .expect("static backend identity")
}
pub(super) fn containment_support(request: &ExecutionRequest) -> ContainmentSupport {
    let requested = EnforcementDimensions::requested_by(request.policy());
    let probe = || -> Result<(), ExecutionError> {
        if unsafe { libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() } {
            return Err(unsupported(
                "ManagedTree does not accept set-id caller credentials",
            ));
        }
        if landlock_abi().is_none_or(|abi| abi < 3) {
            return Err(unsupported("Landlock ABI 3 is required"));
        }
        if request.private_launcher_executable().is_none() {
            return Err(unsupported("private launcher was not initialized"));
        }
        locate_unshare()?;
        let limits = request.policy().limits();
        if limits.timeout_seconds().is_none()
            || limits
                .max_output_bytes()
                .is_none_or(|value| value > 16 * 1024 * 1024)
        {
            return Err(unsupported(
                "Linux ManagedTree requires a timeout and max-output-bytes no greater than 16 MiB",
            ));
        }
        filter(request.policy().network(), request.policy().subprocess())?;
        Cgroup::prepare(limits)?;
        Ok(())
    };
    match probe() {
        Err(error) => ContainmentSupport::unsupported(
            identity(),
            "linux",
            error.to_string(),
            requested,
            EnforcementDimensions::none(),
            EnforcementDimensions::none(),
        ),
        Ok(()) => {
            let evidence = evidence_for_dimensions(
                &requested,
                "Landlock/seccomp, private PID namespace, delegated cgroup v2 limits with readback, parent-death ownership, bounded pipe draining",
                LIMITATIONS,
            );
            ContainmentSupport::supported(
                identity(),
                requested.clone(),
                requested.clone(),
                requested,
                evidence.clone(),
                evidence,
            )
        }
    }
}

/// Install the parent-death boundary while the held supervisor pidfd proves the
/// original supervisor is still alive. Namespace init death kills descendants.
pub(super) fn protect_namespace_init(
    writes: &[(PathBuf, u64, u64)],
    view: &(PathBuf, u64, u64),
) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut poll = libc::pollfd {
        fd: MANAGED_SUPERVISOR_FD,
        events: libc::POLLIN,
        revents: 0,
    };
    if fs::read_link(format!("/proc/self/fd/{MANAGED_SUPERVISOR_FD}"))?
        != Path::new("anon_inode:[pidfd]")
        || unsafe { libc::poll(&mut poll, 1, 0) } != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed supervisor died before namespace init setup",
        ));
    }
    filesystem::readonly_view(writes, view)?;
    // Remove all setup privileges, including root's implicit capability regain.
    if unsafe { libc::prctl(libc::PR_SET_SECUREBITS, 0x3f, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let data = [
        Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn filter(
    network: bool,
    subprocess: bool,
) -> Result<Vec<libc::sock_filter>, ExecutionError> {
    let mut filter = seccomp_filter(network, subprocess)?;
    filter.pop(); // Replace the final allow with ManagedTree-specific restrictions.
    // clone3's CLONE_INTO_CGROUP accepts an O_PATH cgroup descriptor and
    // bypasses pathname write mediation. Return ENOSYS so libc can fall back
    // to legacy clone for threads, without exposing cgroup reassignment.
    filter.extend([
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_clone3 as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        },
    ]);
    let mut denied = vec![
        libc::SYS_chroot,
        libc::SYS_setuid,
        libc::SYS_setgid,
        libc::SYS_setreuid,
        libc::SYS_setregid,
        libc::SYS_setresuid,
        libc::SYS_setresgid,
        libc::SYS_setfsuid,
        libc::SYS_setfsgid,
        libc::SYS_setgroups,
        libc::SYS_capset,
        libc::SYS_memfd_create,
        libc::SYS_shmget,
        libc::SYS_shmat,
        libc::SYS_mknodat,
    ];
    #[cfg(target_arch = "x86_64")]
    denied.push(libc::SYS_mknod);
    for syscall in denied {
        filter.extend([
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: syscall as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
            },
        ]);
    }
    if network {
        for (syscall, first, second) in [
            (libc::SYS_socket, libc::AF_INET, libc::AF_INET6),
            (libc::SYS_socketpair, libc::AF_UNIX, libc::AF_UNIX),
        ] {
            filter.extend([
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 4,
                    k: syscall as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 16,
                },
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 2,
                    jf: 0,
                    k: first as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 1,
                    jf: 0,
                    k: second as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
            ]);
        }
    }
    // Datagram socketpairs can be reconnected to host Unix sockets. Preserve
    // stream IPC only, whether Internet networking is enabled or disabled.
    filter.extend([
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 4,
            k: libc::SYS_socketpair as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 24,
        },
        libc::sock_filter {
            code: (libc::BPF_ALU | libc::BPF_AND | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: 0xf,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 1,
            jf: 0,
            k: libc::SOCK_STREAM as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0,
        },
    ]);
    for (syscall, offset, value, bitset) in [
        (libc::SYS_prctl, 16, libc::PR_SET_PDEATHSIG as u32, false),
        (libc::SYS_mmap, 40, libc::MAP_HUGETLB as u32, true),
    ] {
        filter.extend([
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 3,
                k: syscall as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: offset,
            },
            libc::sock_filter {
                code: (libc::BPF_JMP
                    | if bitset {
                        libc::BPF_JSET
                    } else {
                        libc::BPF_JEQ
                    }
                    | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: value,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 0,
            },
        ]);
    }
    filter.push(libc::sock_filter {
        code: (libc::BPF_RET | libc::BPF_K) as u16,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    Ok(filter)
}

pub(super) fn prepare<'a>(
    request: &ExecutionRequest,
    preflight: &'a ValidatedPreflight,
    ruleset: LandlockRuleset,
) -> Result<OwnedExecutionAttempt<'a>, PreparationError> {
    let cgroup = Cgroup::prepare(request.policy().limits()).map_err(PreparationError::from)?;
    let view = filesystem::View::reserve(&preflight.policy).map_err(PreparationError::from)?;
    Ok(OwnedExecutionAttempt::new(
        preflight,
        Box::new(Lifecycle {
            request: request.clone(),
            preflight,
            ruleset,
            cgroup,
            view,
            child: None,
        }),
    ))
}
struct Lifecycle<'a> {
    request: ExecutionRequest,
    preflight: &'a ValidatedPreflight,
    ruleset: LandlockRuleset,
    cgroup: Cgroup,
    view: filesystem::View,
    child: Option<Child>,
}
impl Lifecycle<'_> {
    fn finish_tree(&mut self) -> Result<(), ExecutionError> {
        self.cgroup.kill()?;
        if let Some(child) = &mut self.child {
            child
                .wait()
                .map_err(|e| unsupported(&format!("cannot reap managed supervisor: {e}")))?;
        }
        self.cgroup.wait_empty()
    }
    fn completion(&self) -> CompletionEvidence {
        let confirmed =
            EnforcementDimensions::completion_required(self.preflight.support.requested());
        let evidence = evidence_for_dimensions(
            &confirmed,
            "namespace init ownership and cgroup.kill followed by populated=0 and supervisor reaping",
            LIMITATIONS,
        );
        CompletionEvidence::checked(
            self.preflight,
            confirmed,
            evidence,
            CleanupConfidence::KernelOwnedComplete,
        )
        .expect("complete kernel-owned cleanup evidence")
    }
}
impl ExecutionLifecycle for Lifecycle<'_> {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        let view_identity = self.view.identity()?;
        let (mut command, report) = private_memory_stats_command(
            &self.request,
            &self.ruleset.0,
            Some(self.preflight),
            Some((&self.view.path, view_identity.0, view_identity.1)),
        )?;
        let membership = self.cgroup.open("cgroup.procs", true)?;
        let parent_pid = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent_pid
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "managed caller died before launch",
                    ));
                }
                let fd = libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) as libc::c_int;
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::dup2(fd, MANAGED_SUPERVISOR_FD) < 0 {
                    libc::close(fd);
                    return Err(io::Error::last_os_error());
                }
                if fd != MANAGED_SUPERVISOR_FD {
                    libc::close(fd);
                }
                if libc::fcntl(MANAGED_SUPERVISOR_FD, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::write(membership.as_raw_fd(), b"0\n".as_ptr().cast(), 2) != 2 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
            .current_dir(self.request.working_directory())
            .env_clear()
            .envs(&self.preflight.child_environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .map_err(|e| unsupported(&format!("cannot launch managed supervisor: {e}")))?;
        self.child = Some(child);
        drop(command);
        let child = self.child.as_mut().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
            {
                return Err(unsupported("cannot establish bounded output draining"));
            }
        }
        let limit = self.request.policy().limits().max_output_bytes().unwrap() as usize;
        let timeout =
            Duration::from_secs(self.request.policy().limits().timeout_seconds().unwrap());
        let start = Instant::now();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut termination;
        loop {
            if drain(&mut stdout, &mut out, err.len(), limit)?
                || drain(&mut stderr, &mut err, out.len(), limit)?
            {
                termination = Termination::OutputLimitExceeded;
                break;
            }
            if start.elapsed() >= timeout {
                termination = Termination::TimedOut;
                break;
            }
            if self.cgroup.exceeded("pids.events", "max")? {
                termination = Termination::ProcessLimitExceeded;
                break;
            }
            if self.cgroup.exceeded("memory.events", "max")? {
                termination = Termination::MemoryLimitExceeded;
                break;
            }
            if let Some(status) = self
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|e| unsupported(&format!("cannot inspect managed supervisor: {e}")))?
            {
                termination = status.code().map_or_else(
                    || Termination::Signaled(status.signal().unwrap_or(0)),
                    Termination::Exited,
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.finish_tree()?;
        if drain_complete(&mut stdout, &mut out, err.len(), limit)?
            || drain_complete(&mut stderr, &mut err, out.len(), limit)?
        {
            termination = Termination::OutputLimitExceeded;
        }
        // Forward buffered setup diagnostics even when the private launcher fails.
        let _ = io::stdout().write_all(&out);
        let _ = io::stderr().write_all(&err);
        parse_private_report(report)?;
        let receipt = EnforcementReceipt::checked(
            self.preflight,
            self.preflight.support.requested().clone(),
            evidence_for_dimensions(
                self.preflight.support.requested(),
                "target exec gated by verified PID/mount namespace setup, held supervisor pidfd, Landlock/seccomp and prior cgroup assignment",
                LIMITATIONS,
            ),
        )?;
        // No target remains while potentially blocking terminal output is written.
        Ok(Box::new(ExecutionOutcome::checked(
            termination,
            Vec::new(),
            Vec::new(),
            receipt,
            self.completion(),
        )?))
    }
    fn cleanup(&mut self) -> CompletionEvidence {
        self.finish_tree()
            .expect("managed process-tree cleanup could not be established");
        self.completion()
    }
}
fn drain(
    reader: &mut impl Read,
    destination: &mut Vec<u8>,
    other_bytes: usize,
    limit: usize,
) -> Result<bool, ExecutionError> {
    let mut buffer = [0u8; 4096];
    // Bound each turn so an active writer cannot starve deadline/accounting checks.
    for _ in 0..4 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(count) => {
                let remaining = limit.saturating_sub(destination.len().saturating_add(other_bytes));
                destination.extend_from_slice(&buffer[..count.min(remaining)]);
                if count > remaining {
                    return Ok(true);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(unsupported(&format!(
                    "cannot drain managed output: {error}"
                )));
            }
        }
    }
    Ok(false)
}

fn drain_complete(
    reader: &mut impl Read,
    destination: &mut Vec<u8>,
    other_bytes: usize,
    limit: usize,
) -> Result<bool, ExecutionError> {
    loop {
        let before = destination.len();
        if drain(reader, destination, other_bytes, limit)? {
            return Ok(true);
        }
        if destination.len() == before {
            return Ok(false);
        }
    }
}
