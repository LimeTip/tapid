use super::{
    AssuranceLevel, BackendIdentity, CleanupConfidence, CompletionEvidence, ContainmentSupport,
    DimensionEvidence, EnforcementDimensions, EnforcementReceipt, ExecutionBackend, ExecutionError,
    ExecutionErrorCategory, ExecutionLifecycle, ExecutionOutcome, ExecutionRequest,
    FilesystemAccess, FilesystemBindings, FilesystemGrantKind, OwnedExecutionAttempt,
    PreparationError, ResolvedSandboxPolicy, RuntimeFilesystemAdditions, Termination,
    ValidatedPreflight, evidence_for_dimensions,
};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};

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
const LIMITATIONS: &[&str] = &[
    "Restricted only; ManagedTree and configured resource limits remain unsupported",
    "Landlock grants use path bindings checked against held filesystem identities before setup",
    "network denial blocks socket syscalls; enabled networking is unrestricted",
    "Restricted does not own or guarantee cleanup of detached descendants",
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
            libc::SYS_pivot_root,
            libc::SYS_setns,
            libc::SYS_unshare,
            libc::SYS_keyctl,
            libc::SYS_perf_event_open,
            libc::SYS_open_by_handle_at,
            libc::SYS_io_uring_setup,
        ];
        if !network {
            // Node/libuv uses getsockname to classify inherited socket-backed stdio.
            // Keep metadata queries available; socket creation and traffic remain denied.
            denied.extend([
                libc::SYS_socket,
                libc::SYS_socketpair,
                libc::SYS_connect,
                libc::SYS_accept,
                libc::SYS_accept4,
                libc::SYS_bind,
                libc::SYS_listen,
                libc::SYS_sendto,
                libc::SYS_recvfrom,
                libc::SYS_sendmsg,
                libc::SYS_recvmsg,
                libc::SYS_sendmmsg,
                libc::SYS_recvmmsg,
                libc::SYS_shutdown,
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

fn install_restrictions(ruleset: libc::c_int, filter: &[libc::sock_filter]) -> io::Result<()> {
    if unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } < 0 {
        return Err(io::Error::last_os_error());
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

fn backend_identity() -> BackendIdentity {
    BackendIdentity::new(
        "tapid-runner/linux-landlock-seccomp-restricted",
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
    let identity = backend_identity();
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
    if request.policy().assurance() != AssuranceLevel::Restricted {
        return unsupported("Linux ManagedTree containment is unavailable");
    }
    if landlock_abi().is_none_or(|abi| abi < 3) {
        return unsupported("Landlock ABI 3 or newer is unavailable");
    }
    let limits = request.policy().limits();
    if limits.timeout_seconds().is_some()
        || limits.max_output_bytes().is_some()
        || limits.max_processes().is_some()
        || limits.max_memory_bytes().is_some()
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
impl ExecutionLifecycle for LinuxLifecycle<'_> {
    fn execute(&mut self) -> Result<Box<ExecutionOutcome>, ExecutionError> {
        let mut command = Command::new(self.request.program());
        command
            .args(self.request.arguments())
            .current_dir(self.request.project_root())
            .env_clear()
            .envs(&self.preflight.child_environment)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let ruleset = self.ruleset.0.try_clone().map_err(|e| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot duplicate Landlock ruleset: {e}"),
            )
        })?;
        let filter = self.filter.clone();
        unsafe {
            command.pre_exec(move || install_restrictions(ruleset.as_raw_fd(), &filter));
        }
        let status = command.status().map_err(|e| {
            ExecutionError::new(
                ExecutionErrorCategory::Spawn,
                format!("cannot launch restricted project script: {e}"),
            )
        })?;
        self.termination = Some(if let Some(code) = status.code() {
            Termination::Exited(code)
        } else {
            Termination::Signaled(status.signal().unwrap_or(0))
        });
        let requested = self.preflight.support.requested();
        let receipt = EnforcementReceipt::checked(
            self.preflight,
            requested.clone(),
            evidence_for_dimensions(
                requested,
                "Landlock and seccomp restrictions installed before target exec",
                LIMITATIONS,
            ),
        )?;
        let completion = completion_for(self.preflight)?;
        Ok(Box::new(ExecutionOutcome::checked(
            self.termination.clone().unwrap(),
            Vec::new(),
            Vec::new(),
            receipt,
            completion,
        )?))
    }

    fn cleanup(&mut self) -> CompletionEvidence {
        completion_for(self.preflight).expect("Restricted completion evidence is valid")
    }
}

fn completion_for(preflight: &ValidatedPreflight) -> Result<CompletionEvidence, ExecutionError> {
    CompletionEvidence::checked(
        preflight,
        EnforcementDimensions::completion_required(preflight.support.requested()),
        Vec::<DimensionEvidence>::new(),
        CleanupConfidence::NotGuaranteed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssuranceLevel, ExecutionLimits, ExecutionRequest, FilesystemPolicy, SandboxMode,
        SandboxPolicy, Termination,
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
    fn managed_tree_stays_fail_closed_before_target_execution() {
        let root = root();
        let marker = root.join("marker");
        let req = request(
            &root,
            &format!("touch '{}'", marker.display()),
            SandboxPolicy::default(),
        );
        let error = execute(&req).unwrap_err();
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
