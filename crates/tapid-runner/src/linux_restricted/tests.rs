use super::*;
use crate::{
    AssuranceLevel, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest, FilesystemPolicy,
    SandboxMode, SandboxPolicy, Termination,
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

fn fails_closed_for_unavailable_kernel_enforcement(request: &ExecutionRequest) -> bool {
    let support = containment_support(request);
    if support.is_supported() {
        return false;
    }
    if std::env::var_os("TAPID_REQUIRE_KERNEL_ENFORCEMENT_TESTS").is_some() {
        panic!(
            "required test lane lacks positive Linux kernel enforcement: {:?}",
            support.unsupported_reason()
        );
    }
    assert!(
        matches!(
            support.unsupported_reason(),
            Some(
                "Landlock ABI 3 or newer is unavailable"
                    | "kernel cannot install the required seccomp filter"
            )
        ),
        "unexpected unsupported reason: {:?}",
        support.unsupported_reason()
    );
    let error =
        execute(request).expect_err("unsupported kernel enforcement must fail before spawn");
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    true
}

#[test]
fn network_denial_preserves_unix_ipc_but_blocks_inet_socket_creation() {
    let root = root();
    let request = request(&root, "exit 0", restricted(&root, vec![], true));
    if fails_closed_for_unavailable_kernel_enforcement(&request) {
        fs::remove_dir_all(root).unwrap();
        return;
    }
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
        unshare_namespace_arguments(false, false),
        [
            "--mount",
            "--pid",
            "--fork",
            "--kill-child",
            "--propagation",
            "unchanged",
            "--",
        ]
    );
    assert_eq!(
        &unshare_namespace_arguments(true, false)[..2],
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
    if fails_closed_for_unavailable_kernel_enforcement(&req) {
        fs::remove_dir_all(root).unwrap();
        return;
    }
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
    if fails_closed_for_unavailable_kernel_enforcement(&req) {
        fs::remove_dir_all(root).unwrap();
        return;
    }
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
    if fails_closed_for_unavailable_kernel_enforcement(&req) {
        fs::remove_dir_all(root).unwrap();
        return;
    }
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
    if fails_closed_for_unavailable_kernel_enforcement(&req) {
        assert!(!outside.exists());
        fs::remove_dir_all(root).unwrap();
        return;
    }
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
