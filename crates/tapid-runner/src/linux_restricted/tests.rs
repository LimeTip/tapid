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
fn managed_tree_enforces_memory_limit_with_rust_workload() {
    const WORKLOAD: &str = "TAPID_MANAGED_MEMORY_WORKLOAD";
    if std::env::var_os(WORKLOAD).is_some() {
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
    let policy = SandboxPolicy::new(
        SandboxMode::Required,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        false,
        vec![WORKLOAD.into()],
        true,
        ExecutionLimits::new(Some(10), Some(4096), Some(32), Some(64 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let req = ExecutionRequest::builder(std::env::current_exe().unwrap())
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
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        return;
    }
    const ROLE: &str = "TAPID_TEST_MANAGED_CRASH_PROJECT";
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
        let _ = execute(&request(
            &root,
            "setsid sh -c 'sh -c \"echo started > started; sleep 3; echo escaped > escaped\" & exit 0' & sleep 15",
            policy,
        ));
        panic!("crash fixture unexpectedly returned");
    }
    let project = tapid_test_support::TempProject::new("managed-caller-crash").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "execution::platform_backend::tests::managed_tree_caller_crash_kills_double_forked_descendants", "--nocapture"])
        .env(ROLE, project.path()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !project.path().join("started").exists() && std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("crash fixture exited before descendant started: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(project.path().join("started").exists());
    child.kill().unwrap();
    child.wait().unwrap();
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
