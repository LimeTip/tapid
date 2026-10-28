use super::*;
use crate::{
    AssuranceLevel, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest, FilesystemPolicy,
    SandboxMode, SandboxPolicy, Termination,
};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn managed_tree_enforces_timeout_and_cleans_detached_descendants() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("linux-managed-timeout").unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(1), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let result = execute(&request(
        project.path(),
        "setsid sh -c 'echo started > started; sleep 2; echo escaped > escaped' & sleep 5",
        policy,
    ))
    .unwrap();
    assert_eq!(result.termination(), &Termination::TimedOut);
    assert!(
        project.path().join("started").exists(),
        "detached descendant did not start"
    );
    assert_eq!(
        result.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(!project.path().join("escaped").exists());
}

fn execute(request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
    super::super::execute_with_backend(request, &PlatformBackend)
}

fn project_workload(project: &tapid_test_support::TempProject) -> PathBuf {
    // Self-executing probes need the same explicit read authority as any target.
    // Keep the executable outside writable subtrees without granting Cargo's
    // artifact directory (or the sibling outside-secret fixture).
    let target = project.path().join("workload");
    fs::copy(std::env::current_exe().unwrap(), &target).unwrap();
    fs::canonicalize(target).unwrap()
}

// Only the trusted outer test driver retries transient executable-busy errors.
// Production target execution retains its original fail-closed behavior.
fn spawn_test_driver(
    command: &mut Command,
    deadline: std::time::Instant,
) -> io::Result<std::process::Child> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("test driver launch exhausted after {attempts} attempts: {error}"),
                    ));
                }
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
            }
            Err(error) => return Err(error),
        }
    }
}

#[test]
fn managed_driver_spawn_recovers_after_executable_writer_closes() {
    let project = tapid_test_support::TempProject::new("driver-busy-release").unwrap();
    let executable = project.path().join("true");
    fs::copy("/bin/true", &executable).unwrap();
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .unwrap();
    assert_eq!(
        Command::new(&executable)
            .spawn()
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ETXTBSY)
    );
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(writer);
    });
    let mut child = spawn_test_driver(
        &mut Command::new(&executable),
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
    release.join().unwrap();
}

#[test]
fn managed_driver_spawn_bounds_persistent_executable_writer() {
    let project = tapid_test_support::TempProject::new("driver-busy-deadline").unwrap();
    let executable = project.path().join("true");
    fs::copy("/bin/true", &executable).unwrap();
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .unwrap();
    let started = std::time::Instant::now();
    let error = spawn_test_driver(
        &mut Command::new(&executable),
        started + std::time::Duration::from_millis(50),
    )
    .unwrap_err();
    assert!(error.to_string().contains("attempts"), "{error}");
    assert!(error.to_string().contains("os error 26"), "{error}");
    assert!(started.elapsed() >= std::time::Duration::from_millis(50));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    drop(writer);
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
fn managed_driver_spawn_preserves_other_errors() {
    let project = tapid_test_support::TempProject::new("driver-missing").unwrap();
    let started = std::time::Instant::now();
    let error = spawn_test_driver(
        &mut Command::new(project.path().join("missing")),
        started + std::time::Duration::from_secs(5),
    )
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
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
fn live_sink_exec_closes_unrelated_inheritable_descriptors() {
    const CHILD: &str = "TAPID_TEST_LINUX_LIVE_SINK_FDS";
    if std::env::var_os(CHILD).is_some() {
        assert!(fs::read_link("/proc/self/fd/9").is_ok());
        let (mut read, write) = UnixStream::pair().unwrap();
        read.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let (mut sink, mut input) =
            super::super::unix_output::spawn_live_sink(write.as_fd(), sanitize_output_descriptors)
                .unwrap();
        drop(write);
        input.write_all(b"x").unwrap();
        let mut ready = [0];
        read.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"x");
        let inherited = fs::read_dir(format!("/proc/{}/fd", sink.id()))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(!inherited.iter().any(|fd| fd == "9"));
        drop(input);
        assert!(sink.wait().unwrap().success());
        return;
    }
    let project = tapid_test_support::TempProject::new("linux-live-sink-fds").unwrap();
    let sentinel = project
        .write("sentinel", b"private fixture descriptor")
        .unwrap();
    let output = Command::new("/bin/sh")
        .args(["-c", r#"exec 9<"$1"; exec "$2" --exact "$3" --nocapture"#, "tapid-live-sink-fds"])
        .arg(sentinel)
        .arg(std::env::current_exe().unwrap())
        .arg("execution::platform_backend::tests::live_sink_exec_closes_unrelated_inheritable_descriptors")
        .env_clear().env(CHILD, "1")
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn restricted_completion_is_bounded_when_parent_stderr_is_unread() {
    const CHILD: &str = "TAPID_TEST_RESTRICTED_UNREAD_STDERR";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let result = execute(&request(
            &root,
            "chunk=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=0; while [ $i -lt 16384 ]; do printf '%s' \"$chunk\" >&2 || break; i=$((i+1)); done; exit 0",
            restricted(&root, vec![], false),
        ));
        fs::write(root.join("result"), format!("{result:?}")).unwrap();
        std::process::exit(match result {
            Err(error) if error.category() == ExecutionErrorCategory::Internal => 0,
            _ => 2,
        });
    }
    let project = tapid_test_support::TempProject::new("restricted-unread-stderr").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "execution::platform_backend::tests::restricted_completion_is_bounded_when_parent_stderr_is_unread",
            "--nocapture",
        ])
        .env(CHILD, project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(
        status.is_some(),
        "stderr forwarding blocked restricted completion"
    );
    assert!(
        status.unwrap().success(),
        "expected a bounded backpressure error: {}",
        fs::read_to_string(project.path().join("result")).unwrap_or_default()
    );
}

#[test]
fn restricted_completion_does_not_wait_for_inherited_descendant_stderr() {
    let project = tapid_test_support::TempProject::new("restricted-stderr-lifetime").unwrap();
    let started = std::time::Instant::now();
    let outcome = execute(&request(
        project.path(),
        "printf root-diagnostic >&2; sleep 3 >&2 & exit 0",
        restricted(project.path(), vec![], false),
    ))
    .unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "a descendant holding stderr must not keep the diagnostic worker alive"
    );
    assert_ne!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
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

#[test]
fn managed_tree_requires_private_launcher_with_otherwise_valid_limits() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-missing-launcher").unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(5), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let mut req = request(project.path(), "echo uncontained > marker", policy);
    // Model a consumer that did not install private launcher dispatch. Keep
    // every policy dimension valid so missing limits cannot mask this failure.
    req.launcher = None;
    let error = execute(&req).unwrap_err();
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(
        error
            .to_string()
            .contains("private launcher was not initialized"),
        "{error}"
    );
    assert!(error.completion().is_none());
    assert!(!project.path().join("marker").exists());
}

#[test]
fn seccomp_denies_process_memory_and_pidfds_even_for_self() {
    // Self-targets need no Yama permission. This proves seccomp supplies the
    // denial independently of host tracing policy.
    let filter = seccomp_filter(true, true).unwrap();
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
    if pid == 0 {
        unsafe {
            let target = libc::getpid();
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
                libc::_exit(3);
            }
            let mut byte = 42u8;
            let iov = libc::iovec {
                iov_base: (&mut byte as *mut u8).cast(),
                iov_len: 1,
            };
            for syscall in [libc::SYS_process_vm_readv, libc::SYS_process_vm_writev] {
                if libc::syscall(syscall, target, &iov, 1, &iov, 1, 0) != -1
                    || *libc::__errno_location() != libc::EPERM
                {
                    libc::_exit(4);
                }
            }
            if libc::syscall(libc::SYS_pidfd_open, target, 0) != -1
                || *libc::__errno_location() != libc::EPERM
            {
                libc::_exit(5);
            }
            // An invalid descriptor needs no pidfd support before filtering.
            // Seccomp must return EPERM even when the kernel would return
            // EBADF, EINVAL, or ENOSYS for the unfiltered syscall.
            if libc::syscall(libc::SYS_pidfd_getfd, -1, -1, u32::MAX) != -1
                || *libc::__errno_location() != libc::EPERM
            {
                libc::_exit(6);
            }
            #[cfg(target_arch = "x86_64")]
            for number in [539, 540, libc::SYS_pidfd_open, libc::SYS_pidfd_getfd] {
                // x32 shares AUDIT_ARCH_X86_64 but has its own syscall numbers.
                // Test rejection even on kernels that do not enable x32.
                if libc::syscall(0x4000_0000 | number, -1, 0, 0, 0, 0, 0) != -1
                    || *libc::__errno_location() != libc::EPERM
                {
                    libc::_exit(7);
                }
            }
            libc::_exit(0);
        }
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}

#[test]
fn managed_tree_enforces_output_and_process_limits() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-limits").unwrap();
    for (script, expected) in [
        (
            "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
            Termination::OutputLimitExceeded,
        ),
        (
            "while :; do sleep 10 & done",
            Termination::ProcessLimitExceeded,
        ),
    ] {
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(10), Some(512), Some(12), Some(64 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let result = execute(&request(project.path(), script, policy)).unwrap();
        assert_eq!(result.termination(), &expected, "{script}");
        assert_eq!(
            result.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
    }
}

#[test]
fn managed_tree_propagates_authority_to_detached_reparented_targets() {
    const ROLE: &str = "TAPID_TEST_MANAGED_AUTHORITY_ROLE";
    const PROJECT: &str = "TAPID_TEST_MANAGED_AUTHORITY_PROJECT";
    const OUTSIDE: &str = "TAPID_TEST_MANAGED_AUTHORITY_OUTSIDE";
    const NETWORK: &str = "TAPID_TEST_MANAGED_AUTHORITY_NETWORK";
    const TEST: &str = "execution::platform_backend::tests::managed_tree_propagates_authority_to_detached_reparented_targets";
    let role = std::env::var(ROLE).unwrap_or_default();
    if role == "target" || role == "descendant" {
        if role == "descendant" {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while unsafe { libc::getppid() } != 1 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "probe was not reparented to namespace init"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(unsafe { libc::getsid(0) }, unsafe { libc::getpid() });
        }
        let outside = PathBuf::from(std::env::var_os(OUTSIDE).unwrap());
        assert_eq!(unsafe { libc::fcntl(9, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        assert_eq!(fs::read("allowed").unwrap(), b"allowed read");
        for path in [
            outside.join("secret"),
            PathBuf::from("outside-link/secret"),
            PathBuf::from("..")
                .join(outside.file_name().unwrap())
                .join("secret"),
        ] {
            assert_eq!(
                fs::read(&path).unwrap_err().raw_os_error(),
                Some(libc::EACCES)
            );
            assert!(matches!(
                fs::write(&path, b"escape").unwrap_err().raw_os_error(),
                Some(libc::EACCES | libc::EROFS)
            ));
        }
        assert!(matches!(
            fs::write("denied", b"escape").unwrap_err().raw_os_error(),
            Some(libc::EACCES | libc::EROFS)
        ));
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0));
        if std::env::var(NETWORK).unwrap() == "true" {
            assert!(listener.is_ok(), "{listener:?}");
        } else {
            assert_eq!(listener.unwrap_err().raw_os_error(), Some(libc::EPERM));
        }
        fs::write(format!("writable/{role}"), b"authority checked").unwrap();
        if role == "target" {
            // setsid --fork exits while the new session continues under the
            // private namespace init. Wait for that reparented target's proof.
            assert!(
                Command::new("/usr/bin/setsid")
                    .arg("--fork")
                    .arg(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env(ROLE, "descendant")
                    .status()
                    .unwrap()
                    .success()
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while fs::read("writable/descendant").ok().as_deref() != Some(b"authority checked") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "detached authority probe failed"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        return;
    }
    if role == "driver" {
        assert!(unsafe { libc::fcntl(9, libc::F_GETFD) } >= 0);
        let project = PathBuf::from(std::env::var_os(PROJECT).unwrap());
        let outside = std::env::var_os(OUTSIDE).unwrap();
        for network in [false, true] {
            let policy = SandboxPolicy::new(
                SandboxMode::Required,
                FilesystemPolicy::new(vec![".".into()], vec!["writable".into()]).unwrap(),
                network,
                vec![ROLE.into(), OUTSIDE.into(), NETWORK.into()],
                true,
                ExecutionLimits::new(Some(10), Some(8192), Some(64), Some(128 * 1024 * 1024))
                    .unwrap(),
            )
            .unwrap();
            let req = ExecutionRequest::builder(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(ROLE, "target")
                .env(OUTSIDE, outside.clone())
                .env(NETWORK, network.to_string())
                .project_root(&project)
                .policy(policy)
                .build()
                .unwrap();
            let result = execute(&req).unwrap();
            assert_eq!(result.termination(), &Termination::Exited(0));
            assert_eq!(
                result.completion().cleanup_confidence(),
                CleanupConfidence::KernelOwnedComplete
            );
            for name in ["target", "descendant"] {
                let marker = project.join("writable").join(name);
                assert_eq!(fs::read(&marker).unwrap(), b"authority checked");
                fs::remove_file(marker).unwrap();
            }
        }
        return;
    }
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-authority").unwrap();
    let workload = project_workload(&project);
    let outside = tapid_test_support::TempProject::new("managed-authority-outside").unwrap();
    let secret = outside
        .write("secret", b"synthetic outside secret")
        .unwrap();
    project.write("allowed", b"allowed read").unwrap();
    fs::create_dir(project.path().join("writable")).unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join("outside-link")).unwrap();
    // The driver receives an intentionally inheritable descriptor containing
    // denied data. Target setup must close it, even though the caller did not.
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "exec 9<\"$1\"; exec \"$2\" --exact \"$3\" --nocapture",
            "managed-authority",
        ])
        .arg(&secret)
        .arg(workload)
        .arg(TEST)
        .env(ROLE, "driver")
        .env(PROJECT, project.path())
        .env(OUTSIDE, outside.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(secret).unwrap(), b"synthetic outside secret");
    assert!(!project.path().join("denied").exists());
}

#[test]
fn managed_tree_bounds_forwarded_combined_output_and_cleans_writers() {
    const OVERFLOW: &str = "TAPID_TEST_MANAGED_OUTPUT_OVERFLOW";
    const ROLE: &str = "TAPID_TEST_MANAGED_OUTPUT_PROJECT";
    if let Some(path) = std::env::var_os(ROLE) {
        let project = PathBuf::from(path);
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(10), Some(512), Some(32), Some(128 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let overflow = std::env::var_os(OVERFLOW).is_some();
        let stderr_bytes = if overflow { 4096 } else { 256 };
        let ending = if overflow { "sleep 5" } else { "exit 0" };
        let script = format!(
            "setsid sh -c 'echo started > started; sleep 2; echo escaped > escaped' & while [ ! -f started ]; do sleep 0.01; done; i=0; while [ $i -lt 256 ]; do printf '\\376'; i=$((i+1)); done; i=0; while [ $i -lt {stderr_bytes} ]; do printf '\\377' >&2; i=$((i+1)); done; {ending}"
        );
        let result = execute(&request(&project, &script, policy)).unwrap();
        let expected = if overflow {
            Termination::OutputLimitExceeded
        } else {
            Termination::Exited(0)
        };
        assert_eq!(result.termination(), &expected);
        assert_eq!(
            result.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        // ManagedTree forwards its bounded buffer after cleanup rather than
        // returning it in the outcome. The parent observes the actual streams.
        assert!(result.stdout().is_empty());
        assert!(result.stderr().is_empty());
        std::process::exit(0);
    }
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    for overflow in [false, true] {
        let project = tapid_test_support::TempProject::new("managed-forwarded-output").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "execution::platform_backend::tests::managed_tree_bounds_forwarded_combined_output_and_cleans_writers",
            "--nocapture",
        ])
        .env(ROLE, project.path())
        .envs(overflow.then_some((OVERFLOW, "1")))
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Non-UTF-8 sentinels distinguish target bytes from libtest's own output.
        let stdout_bytes = output.stdout.iter().filter(|&&byte| byte == 0xfe).count();
        let stderr_bytes = output.stderr.iter().filter(|&&byte| byte == 0xff).count();
        assert_eq!(stdout_bytes + stderr_bytes, 512);
        if !overflow {
            assert_eq!(stdout_bytes, 256);
            assert_eq!(stderr_bytes, 256);
        } else {
            // The combined ceiling does not promise which ready stream wins the
            // final bytes when the supervisor is descheduled between drain calls.
            assert!(stdout_bytes <= 256);
            assert!(stderr_bytes <= 512);
        }
        assert!(project.path().join("started").exists());
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(!project.path().join("escaped").exists());
    }
}

#[test]
fn managed_tree_enforces_memory_limit_with_rust_workload() {
    const WORKLOAD: &str = "TAPID_MANAGED_MEMORY_WORKLOAD";
    if std::env::var_os(WORKLOAD).is_some() {
        fs::write("started", b"memory workload started").unwrap();
        // Commit pages rather than only reserving virtual address space. Keep
        // every allocation live until the cgroup's memory controller intervenes.
        let mut chunks = Vec::new();
        for _ in 0..256 {
            let mut chunk = vec![0u8; 1024 * 1024];
            for offset in (0..chunk.len()).step_by(4096) {
                unsafe { std::ptr::write_volatile(chunk.as_mut_ptr().add(offset), 1) };
            }
            chunks.push(chunk);
            std::hint::black_box(&chunks);
        }
        panic!("memory workload escaped its 64 MiB ceiling");
    }
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-memory-limit").unwrap();
    let workload = project_workload(&project);
    project.write("started", b"").unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec!["started".into()]).unwrap(),
        false,
        vec![WORKLOAD.into()],
        true,
        ExecutionLimits::new(Some(10), Some(4096), Some(32), Some(64 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let req = ExecutionRequest::builder(workload)
        .args([
            "--exact",
            "execution::platform_backend::tests::managed_tree_enforces_memory_limit_with_rust_workload",
            "--nocapture",
        ])
        .env(WORKLOAD, "1")
        .project_root(project.path())
        .executable_search_paths(["/usr/bin"])
        .policy(policy)
        .build()
        .unwrap();
    let result = execute(&req).unwrap();
    assert_eq!(result.termination(), &Termination::MemoryLimitExceeded);
    assert_eq!(
        fs::read(project.path().join("started")).unwrap(),
        b"memory workload started",
        "memory target must reach its workload before controller termination"
    );
    assert_eq!(
        result.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
}

#[test]
fn managed_tree_preserves_exit_codes_and_cleans_descendants_on_root_exit() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    for code in [0, 37] {
        let project = tapid_test_support::TempProject::new("managed-root-exit").unwrap();
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(10), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let script = format!(
            "setsid sh -c 'sh -c \"echo started > started; sleep 2; echo escaped > escaped\" & wait' & while [ ! -f started ]; do sleep 0.01; done; exit {code}"
        );
        let result = execute(&request(project.path(), &script, policy)).unwrap();
        assert_eq!(result.termination(), &Termination::Exited(code));
        assert!(project.path().join("started").exists());
        assert_eq!(
            result.enforcement().assurance(),
            AssuranceLevel::ManagedTree
        );
        assert_eq!(
            result.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(!project.path().join("escaped").exists());
    }
}

#[test]
fn managed_tree_denies_metadata_mutation_outside_write_grants() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let project = tapid_test_support::TempProject::new("managed-metadata").unwrap();
    project.write("readonly", b"protected").unwrap();
    project.write("build/file", b"allowed").unwrap();
    fs::set_permissions(
        project.path().join("readonly"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec!["build".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(5), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let result = execute(&request(
        project.path(),
        "chmod 777 readonly && exit 42; chmod 700 build/file && echo done > build/result",
        policy,
    ))
    .unwrap();
    assert_eq!(result.termination(), &Termination::Exited(0));
    assert_eq!(
        fs::metadata(project.path().join("readonly"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read(project.path().join("build/result")).unwrap(),
        b"done\n"
    );
}

#[test]
fn managed_tree_rejects_kernel_control_filesystem_write_grants() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    for (root, write) in [
        ("/sys/fs/cgroup", "."),
        ("/sys/fs/cgroup", "cgroup.procs"),
        ("/proc", "."),
        ("/sys", "."),
    ] {
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![write.into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(5), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let error = execute(&request(Path::new(root), ":", policy))
            .expect_err("ManagedTree accepted writable kernel controls");
        assert!(
            error.to_string().contains("kernel control filesystems"),
            "{root}/{write}: {error}"
        );
    }
}

#[test]
fn managed_tree_memory_stats_opt_in_uses_read_only_private_procfs() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-memory-stats").unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(5), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let script = format!(
        "cat /proc/self/statm || exit 41; if [ -r /proc/{}/statm ]; then exit 42; fi; if printf x > /proc/self/comm 2>/dev/null; then exit 43; fi",
        std::process::id()
    );
    let req = ExecutionRequest::builder("/bin/sh")
        .args(["-c", &script])
        .project_root(project.path())
        .executable_search_paths(["/usr/bin"])
        .policy(policy)
        .allow_process_memory_stats(true)
        .build()
        .unwrap();
    let outcome = execute(&req).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
}

#[test]
fn managed_tree_caller_crash_kills_double_forked_descendants() {
    assert_managed_crash_cleanup(false);
}

#[test]
fn managed_tree_supervisor_crash_kills_double_forked_descendants() {
    assert_managed_crash_cleanup(true);
}

fn assert_managed_crash_cleanup(crash_supervisor: bool) {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    const ROLE: &str = "TAPID_TEST_MANAGED_CRASH_PROJECT";
    const SUPERVISOR: &str = "TAPID_TEST_MANAGED_SUPERVISOR_CRASH";
    if let Some(path) = std::env::var_os(ROLE) {
        let root = PathBuf::from(path);
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(20), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let result = execute(&request(
            &root,
            "setsid sh -c 'sh -c \"echo started > started; sleep 3; echo escaped > escaped\" & exit 0' & sleep 15",
            policy,
        ));
        if std::env::var_os(SUPERVISOR).is_some() {
            let outcome = result.unwrap();
            assert_eq!(outcome.termination(), &Termination::Signaled(libc::SIGKILL));
            assert_eq!(
                outcome.completion().cleanup_confidence(),
                CleanupConfidence::KernelOwnedComplete
            );
            return;
        }
        panic!("crash fixture unexpectedly returned");
    }
    let project = tapid_test_support::TempProject::new("managed-caller-crash").unwrap();
    struct CrashCaller(std::process::Child);
    impl Drop for CrashCaller {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut caller = CrashCaller(Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "execution::platform_backend::tests::managed_tree_caller_crash_kills_double_forked_descendants", "--nocapture"])
        .env(ROLE, project.path()).envs(crash_supervisor.then_some((SUPERVISOR, "1"))).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap());
    let child = &mut caller.0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !project.path().join("started").exists() && std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("crash fixture exited before descendant started: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(project.path().join("started").exists());
    // Discover the trusted supervisor's boundary while the caller is alive,
    // then retain the kernel events object across caller death. PID discovery
    // alone is not cleanup evidence; populated=0 on this held boundary is.
    let mut boundary = None;
    for task in fs::read_dir(format!("/proc/{}/task", child.id())).unwrap() {
        let children = fs::read_to_string(task.unwrap().path().join("children")).unwrap();
        for pid in children.split_whitespace() {
            let membership = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
            if let Some(relative) = membership
                .lines()
                .find_map(|line| line.strip_prefix("0::/"))
            {
                let path = Path::new(relative);
                assert!(
                    path.components()
                        .all(|component| matches!(component, std::path::Component::Normal(_)))
                );
                if path
                    .file_name()
                    .unwrap()
                    .as_encoded_bytes()
                    .starts_with(b"tapid-managed-")
                {
                    assert!(boundary.is_none(), "unexpected multiple managed boundaries");
                    let path = Path::new("/sys/fs/cgroup").join(path);
                    boundary = Some((
                        File::open(path.join("cgroup.events")).unwrap(),
                        path,
                        pid.parse::<i32>().unwrap(),
                    ));
                }
            }
        }
    }
    let (mut events, boundary_path, supervisor_pid) =
        boundary.expect("supervisor cgroup was not observed");
    let supervisor_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, supervisor_pid, 0) };
    assert!(supervisor_fd >= 0, "cannot hold supervisor identity");
    let supervisor = unsafe { File::from_raw_fd(supervisor_fd as libc::c_int) };
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstatfs(events.as_raw_fd(), &mut stat) }, 0);
    assert_eq!(stat.f_type, libc::CGROUP2_SUPER_MAGIC as libc::c_long);
    let mut state = String::new();
    events.read_to_string(&mut state).unwrap();
    assert!(state.lines().any(|line| line == "populated 1"));
    if crash_supervisor {
        // Hold the caller's cleanup while independently observing kernel-owned
        // descendant termination after supervisor death.
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGSTOP) }, 0);
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(child.id() as i32, &mut status, libc::WUNTRACED) },
            child.id() as i32
        );
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    supervisor.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            },
            0
        );
    } else {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    let empty_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        use std::io::{Seek, SeekFrom};
        events.seek(SeekFrom::Start(0)).unwrap();
        state.clear();
        events.read_to_string(&mut state).unwrap();
        if state.lines().any(|line| line == "populated 0") {
            break;
        }
        assert!(
            std::time::Instant::now() < empty_deadline,
            "caller crash left a populated kernel boundary: {state}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The killed caller cannot remove its now-empty cgroup directory. The
    // observer owns fixture teardown only after kernel emptiness is established.
    if crash_supervisor {
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGCONT) }, 0);
        assert!(
            child.wait().unwrap().success(),
            "caller failed to report supervisor crash cleanup"
        );
        assert!(
            !boundary_path.exists(),
            "caller did not release the empty boundary"
        );
    } else {
        fs::remove_dir(boundary_path).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_secs(4));
    assert!(
        !project.path().join("escaped").exists(),
        "detached descendant survived caller SIGKILL"
    );
}

#[test]
fn managed_filter_denies_clone3_and_reconnectable_unix_socketpairs() {
    for network in [false, true] {
        let filter = managed::filter(network, true).unwrap();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
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
                    libc::_exit(3);
                }
                if libc::syscall(libc::SYS_clone3, std::ptr::null::<u8>(), 0) != -1
                    || *libc::__errno_location() != libc::ENOSYS
                {
                    libc::_exit(4);
                }
                let mut descriptors = [-1; 2];
                if libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, descriptors.as_mut_ptr())
                    != -1
                    || *libc::__errno_location() != libc::EPERM
                {
                    libc::_exit(5);
                }
                if libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) != -1
                    || *libc::__errno_location() != libc::EPERM
                {
                    libc::_exit(6);
                }
                if libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM,
                    0,
                    descriptors.as_mut_ptr(),
                ) != 0
                {
                    libc::_exit(7);
                }
                for fd in descriptors {
                    libc::close(fd);
                }
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn managed_tree_rejects_independently_denied_kernel_prerequisites_before_target() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    const FAULT: &str = "TAPID_TEST_MANAGED_KERNEL_FAULT";
    let project = tapid_test_support::TempProject::new("managed-prerequisites").unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(5), Some(4096), Some(64), Some(256 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let req = request(project.path(), "echo target > marker", policy);
    let Some(fault) = std::env::var_os(FAULT) else {
        // Prove the identical request can execute before denying one native
        // prerequisite in each fresh subprocess. Unsupported-host rejection is
        // not evidence that any particular prerequisite was tested.
        let outcome = execute(&req).unwrap();
        assert_eq!(outcome.termination(), &Termination::Exited(0));
        assert_eq!(
            outcome.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        assert!(project.path().join("marker").exists());
        for fault in [
            "landlock",
            "pidfd",
            "unshare",
            "mount",
            "seccomp",
            "delegation",
            "controllers",
            "launcher-exec",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution::platform_backend::tests::managed_tree_rejects_independently_denied_kernel_prerequisites_before_target", "--nocapture"])
                .env(FAULT, fault).output().unwrap();
            assert!(
                output.status.success(),
                "{fault}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("kernel prerequisite rejection verified"),
                "{fault}: helper assertions did not execute"
            );
        }
        return;
    };
    let (syscall, argument) = match fault.to_str().unwrap() {
        "landlock" => (libc::SYS_landlock_create_ruleset, None),
        "pidfd" => (libc::SYS_pidfd_open, None),
        "unshare" => (libc::SYS_unshare, None),
        "mount" => (libc::SYS_mount, None),
        "seccomp" => (libc::SYS_prctl, Some((16, PR_SET_SECCOMP as u32))),
        "delegation" => (libc::SYS_mkdir, None),
        "controllers" => (
            libc::SYS_openat,
            Some((
                32,
                (libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u32,
            )),
        ),
        "launcher-exec" => (libc::SYS_execve, None),
        _ => panic!("unknown prerequisite fault"),
    };
    // This filter changes actual kernel results rather than mocking backend
    // declarations. The distinctive errno proves the intended syscall was
    // intercepted; all unrelated calls retain the supported-host behavior.
    let statement = |code, k| libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    };
    let mut filter = vec![
        statement((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0),
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: if argument.is_some() { 3 } else { 1 },
            k: syscall as u32,
        },
    ];
    if let Some((offset, argument)) = argument {
        filter.push(statement(
            (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            offset,
        ));
        filter.push(libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: argument,
        });
    }
    filter.push(statement(
        (libc::BPF_RET | libc::BPF_K) as u16,
        SECCOMP_RET_ERRNO | libc::ENOMEDIUM as u32,
    ));
    filter.push(statement(
        (libc::BPF_RET | libc::BPF_K) as u16,
        SECCOMP_RET_ALLOW,
    ));
    assert_eq!(unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }, 0);
    assert_eq!(
        unsafe {
            libc::prctl(
                PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_mut_ptr(),
                },
            )
        },
        0
    );
    let mut arguments = [0 as libc::c_long; 6];
    if let Some((offset, value)) = argument {
        arguments[((offset - 16) / 8) as usize] = value as libc::c_long;
    }
    assert_eq!(
        unsafe {
            libc::syscall(
                syscall,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3],
                arguments[4],
                arguments[5],
            )
        },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ENOMEDIUM)
    );
    let error = execute(&req).unwrap_err();
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(
        !project.path().join("marker").exists(),
        "target ran without a prerequisite: {error}"
    );
    if fault == "landlock" || fault == "delegation" || fault == "controllers" {
        let cause = if fault == "landlock" {
            "Landlock ABI 3"
        } else if fault == "delegation" {
            "writable delegated cgroup v2 parent"
        } else {
            "required cgroup controller or interface unavailable"
        };
        assert!(error.to_string().contains(cause), "{error}");
        assert!(error.completion().is_none());
    } else {
        let stage = if fault == "pidfd" || fault == "launcher-exec" {
            "cannot launch managed supervisor"
        } else {
            "private"
        };
        assert!(error.to_string().contains(stage), "{fault:?}: {error}");
        assert_eq!(
            error
                .completion()
                .expect("partial launch must complete cleanup")
                .cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
    }
    println!("kernel prerequisite rejection verified: {fault:?}: {error}");
}

#[test]
fn managed_tree_failed_target_exec_reports_spawn_and_kernel_cleanup() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-exec-failure").unwrap();
    let target = project
        .write(
            "target",
            format!(
                "#!{}\necho escaped > marker\n",
                project.path().join("missing-interpreter").display()
            )
            .as_bytes(),
        )
        .unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        false,
        vec![],
        true,
        ExecutionLimits::new(Some(5), Some(1024), Some(32), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let req = ExecutionRequest::builder(target)
        .project_root(project.path())
        .policy(policy)
        .build()
        .unwrap();
    let error = execute(&req).unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::Spawn, "{error}");
    assert!(
        error
            .to_string()
            .contains("cannot exec restricted project script"),
        "{error}"
    );
    assert_eq!(
        error
            .completion()
            .expect("failed exec must clean its owned tree")
            .cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert!(!project.path().join("marker").exists());
}

#[test]
fn managed_tree_completion_is_bounded_when_parent_stdout_is_unread() {
    const DRIVER: &str = "TAPID_TEST_MANAGED_UNREAD_OUTPUT_PROJECT";
    const WORKLOAD: &str = "TAPID_TEST_MANAGED_UNREAD_OUTPUT_TARGET";
    const TEST: &str = "execution::platform_backend::tests::managed_tree_completion_is_bounded_when_parent_stdout_is_unread";
    if std::env::var_os(WORKLOAD).is_some() {
        io::stdout().write_all(&vec![b'x'; 1024 * 1024]).unwrap();
        return;
    }
    if let Some(path) = std::env::var_os(DRIVER) {
        let root = PathBuf::from(path);
        let policy = SandboxPolicy::new(
            SandboxMode::Required,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![WORKLOAD.into()],
            true,
            ExecutionLimits::new(
                Some(5),
                Some(2 * 1024 * 1024),
                Some(32),
                Some(128 * 1024 * 1024),
            )
            .unwrap(),
        )
        .unwrap();
        let req = ExecutionRequest::builder(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(WORKLOAD, "1")
            .project_root(&root)
            .policy(policy)
            .build()
            .unwrap();
        let error = execute(&req).expect_err("unread output must not silently succeed");
        eprintln!(
            "unread-output execution: category={:?}, completion={:?}, error={error}",
            error.category(),
            error.completion()
        );
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::Internal,
            "{error}"
        );
        assert!(
            error.to_string().contains("remained backpressured"),
            "driver must reach bounded output forwarding: {error}"
        );
        assert_eq!(
            error
                .completion()
                .expect("output failure must retain cleanup evidence")
                .cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        fs::write(root.join("verified"), "bounded output with kernel cleanup").unwrap();
        std::process::exit(0);
    }
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    let project = tapid_test_support::TempProject::new("managed-unread-output").unwrap();
    let workload = project_workload(&project);
    let diagnostics = project.path().join("driver-stderr");
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(10);
    let mut child = spawn_test_driver(
        Command::new(workload)
            .args(["--exact", TEST, "--nocapture"])
            .env(DRIVER, project.path())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&diagnostics).unwrap()),
        started + std::time::Duration::from_millis(500),
    )
    .unwrap();
    // Deliberately retain the unread caller pipe through completion. No reader
    // may unblock a forwarding worker and turn this into a normal-output test.
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "unread-output driver failed: {status}\n{}",
                fs::read_to_string(&diagnostics).unwrap()
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "ManagedTree completion blocked on the unread caller stdout\n{}",
                fs::read_to_string(&diagnostics).unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(project.path().join("verified")).unwrap(),
        "bounded output with kernel cleanup"
    );
}
