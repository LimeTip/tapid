#![cfg(windows)]

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_runner::{
    AssuranceLevel, CleanupConfidence, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest,
    FilesystemPolicy, SandboxMode, SandboxPolicy, Termination, execute,
};

fn temporary_project(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("tapid-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    path
}

fn managed_policy() -> SandboxPolicy {
    managed_policy_with_limits(
        ExecutionLimits::new(Some(30), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    )
}

fn managed_policy_with_limits(limits: ExecutionLimits) -> SandboxPolicy {
    managed_policy_with_flags(limits, false, false)
}

fn managed_policy_with_flags(
    limits: ExecutionLimits,
    allow_subprocess: bool,
    network: bool,
) -> SandboxPolicy {
    SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).unwrap(),
        network,
        vec!["TAPID_TEST_MARKER".into()],
        allow_subprocess,
        limits,
    )
    .unwrap()
}

fn command_request(root: &PathBuf, command: &str, limits: ExecutionLimits) -> ExecutionRequest {
    command_request_with_flags(root, command, limits, false, false)
}

fn command_request_with_network(
    root: &PathBuf,
    command: &str,
    limits: ExecutionLimits,
    network: bool,
) -> ExecutionRequest {
    command_request_with_flags(root, command, limits, false, network)
}

fn command_request_with_flags(
    root: &PathBuf,
    command: &str,
    limits: ExecutionLimits,
    allow_subprocess: bool,
    network: bool,
) -> ExecutionRequest {
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let mut arguments: Vec<OsString> = vec!["/D".into(), "/S".into(), "/C".into()];
    arguments.push(command.into());
    ExecutionRequest::builder(system32.join("cmd.exe").into_os_string())
        .args(arguments)
        .executable_search_path(&system32)
        .project_root(root)
        .policy(managed_policy_with_flags(limits, allow_subprocess, network))
        .windows_verbatim_arguments(true)
        .build()
        .unwrap()
}

#[test]
fn windows_managed_tree_executes_with_checked_appcontainer_and_job_evidence() {
    let root = temporary_project("managed-tree");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let command = system32.join("cmd.exe");
    let request = ExecutionRequest::builder(command.as_os_str())
        .args([
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
            OsString::from("echo stdout-marker & echo stderr-marker 1>&2 & echo TAPID_WINDOWS_MANAGED_TREE>allowed.txt"),
        ])
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .windows_verbatim_arguments(true)
        .env("TAPID_TEST_MARKER", "allowed-value")
        .build()
        .unwrap();

    let outcome = execute(&request).expect("Windows ManagedTree must execute natively");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains("stdout-marker"));
    assert!(String::from_utf8_lossy(outcome.stderr()).contains("stderr-marker"));
    assert_eq!(
        fs::read_to_string(root.join("allowed.txt")).unwrap().trim(),
        "TAPID_WINDOWS_MANAGED_TREE"
    );
    assert_eq!(
        outcome.enforcement().assurance(),
        AssuranceLevel::ManagedTree
    );
    assert_eq!(
        outcome.enforcement().backend().name(),
        "tapid-runner/windows-appcontainer-job"
    );
    for dimension in [
        tapid_runner::EnforcementDimension::FilesystemRead,
        tapid_runner::EnforcementDimension::FilesystemWrite,
        tapid_runner::EnforcementDimension::Network,
        tapid_runner::EnforcementDimension::EnvironmentSanitization,
        tapid_runner::EnforcementDimension::DescriptorHygiene,
        tapid_runner::EnforcementDimension::DescendantAuthorityPropagation,
        tapid_runner::EnforcementDimension::DescendantLifecycle,
        tapid_runner::EnforcementDimension::ProcessTreeMembership,
        tapid_runner::EnforcementDimension::CompleteCleanup,
        tapid_runner::EnforcementDimension::Timeout,
        tapid_runner::EnforcementDimension::Output,
        tapid_runner::EnforcementDimension::ProcessCount,
        tapid_runner::EnforcementDimension::Memory,
    ] {
        assert!(
            outcome
                .enforcement()
                .established_evidence()
                .iter()
                .any(|evidence| evidence.dimension() == dimension),
            "missing launch evidence for {dimension:?}"
        );
    }
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_cannot_read_files_outside_the_project_grant() {
    let root = temporary_project("deny-read");
    let outside = root.with_extension("private.txt");
    fs::write(&outside, "TAPID_PRIVATE_OUTSIDE_MARKER").unwrap();
    let command = format!(r#"type "{}""#, outside.display());
    let request = command_request(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("read denial should be an ordinary contained exit");
    assert!(matches!(outcome.termination(), Termination::Exited(code) if *code != 0));
    assert!(!String::from_utf8_lossy(outcome.stdout()).contains("TAPID_PRIVATE_OUTSIDE_MARKER"));
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_cannot_write_outside_the_project_grant() {
    let root = temporary_project("deny-write");
    let outside = root.with_extension("should-not-exist.txt");
    let command = format!(r#"echo TAPID_OUTSIDE_WRITE>{}"#, outside.display());
    let request = command_request(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("write denial should be an ordinary contained exit");
    assert!(matches!(outcome.termination(), Termination::Exited(_)));
    assert!(
        !outside.exists(),
        "AppContainer wrote outside its policy grant"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_enforces_the_combined_output_limit() {
    let root = temporary_project("output-limit");
    let request = command_request(
        &root,
        "for /L %i in (1,1,100000) do @echo TAPID_OUTPUT_LINE",
        ExecutionLimits::new(Some(10), Some(128), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("output limit should yield a checked outcome");
    assert_eq!(outcome.termination(), &Termination::OutputLimitExceeded);
    assert!(outcome.stdout().len() + outcome.stderr().len() <= 128);
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_timeout_terminates_the_managed_job() {
    let root = temporary_project("timeout");
    let request = command_request(
        &root,
        "for /L %i in (1,1,2147483647) do @rem",
        ExecutionLimits::new(Some(1), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("timeout should yield a checked outcome");
    assert_eq!(outcome.termination(), &Termination::TimedOut);
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_disabled_subprocess_policy_prevents_child_process_creation() {
    let root = temporary_project("process-limit");
    let marker = "child-process-created.txt";
    let command = format!(r#"start "" /B cmd.exe /D /S /C "echo CHILD>{marker}""#);
    let request = command_request(
        &root,
        &command,
        ExecutionLimits::new(Some(3), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("subprocess restriction should remain contained");
    assert!(matches!(
        outcome.termination(),
        Termination::Exited(_) | Termination::TimedOut
    ));
    assert!(
        !root.join(marker).exists(),
        "a child process escaped the active-process limit"
    );
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_can_launch_an_executable_subprocess_from_runtime_paths() {
    let root = temporary_project("runtime-child-process");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = PathBuf::from(system_root).join("System32");
    let command = format!(
        r#"cd /d "{}" & cmd.exe /D /S /C echo CHILD_PROCESS_MARKER"#,
        system32.display()
    );
    let request = command_request_with_flags(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
        true,
        false,
    );
    let outcome =
        execute(&request).expect("runtime subprocess should remain within the Job Object");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains("CHILD_PROCESS_MARKER"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_node_runtime_can_be_launched_directly_when_test_runtime_is_configured() {
    let Some(local_app_data) = std::env::var_os("TAPID_TEST_LOCALAPPDATA").map(PathBuf::from)
    else {
        eprintln!("skipping: TAPID_TEST_LOCALAPPDATA is not set");
        return;
    };
    fs::create_dir_all(&local_app_data).unwrap();
    let root = fs::canonicalize(local_app_data).unwrap();
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let runtime_bin = node.parent().unwrap().to_path_buf();
    let system32 = fs::canonicalize(
        PathBuf::from(std::env::var_os("SystemRoot").expect("Windows SystemRoot is required"))
            .join("System32"),
    )
    .unwrap();
    let request = ExecutionRequest::builder(node.as_os_str())
        .arg("--version")
        .executable_search_paths([runtime_bin.as_path(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy())
        .build()
        .unwrap();
    let outcome = execute(&request);
    let outcome = outcome.expect("Node should launch inside its AppContainer");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).starts_with('v'));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_runtime_directory_ace_allows_launching_read_execute_binaries() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let runtime_bin = node.parent().unwrap().to_path_buf();
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let probe = runtime_bin.join("cmd.exe");
    assert!(
        !probe.exists(),
        "test runtime unexpectedly contains cmd.exe"
    );
    fs::copy(PathBuf::from(system_root).join("System32/cmd.exe"), &probe).unwrap();
    let root = temporary_project("runtime-executable-acl");
    let request = ExecutionRequest::builder(probe.as_os_str())
        .args(["/D", "/S", "/C", "echo RUNTIME_EXECUTABLE_MARKER"])
        .executable_search_path(&runtime_bin)
        .project_root(&root)
        .policy(managed_policy())
        .windows_verbatim_arguments(true)
        .build()
        .unwrap();
    let outcome = execute(&request).expect("runtime executable should start under AppContainer");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains("RUNTIME_EXECUTABLE_MARKER"));
    fs::remove_file(probe).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_network_enabled_policy_fails_closed_before_spawn() {
    let root = temporary_project("network-allowed-unavailable");
    let marker = root.join("must-not-spawn.txt");
    let command = format!("echo should-not-run>\"{}\"", marker.display());
    let request = command_request_with_network(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
        true,
    );
    let error = execute(&request).expect_err("network-enabled policy is not yet representable");
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(
        !marker.exists(),
        "untrusted code must not start for unsupported network policy"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_network_probe_is_denied_with_a_reachable_host_positive_control() {
    let root = temporary_project("network-denial-control");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let curl = system32.join("curl.exe");
    let version = std::process::Command::new(&curl)
        .arg("--version")
        .output()
        .expect("Windows curl must run outside the AppContainer");
    assert!(
        version.status.success(),
        "curl --version failed: {version:?}"
    );

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("host positive-control listener must bind");
    let address = listener.local_addr().unwrap();
    let positive_control = std::net::TcpStream::connect(address)
        .expect("ordinary Windows process must reach the listener");
    drop(positive_control);
    drop(
        listener
            .accept()
            .expect("host positive-control connection must arrive"),
    );

    let request = ExecutionRequest::builder(curl.into_os_string())
        .args([
            OsString::from("--connect-timeout"),
            OsString::from("2"),
            OsString::from("--max-time"),
            OsString::from("2"),
            OsString::from("--show-error"),
            OsString::from(format!("http://127.0.0.1:{}/", address.port())),
        ])
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .build()
        .unwrap();
    let outcome = execute(&request).expect("network probe should be contained");
    let stdout = String::from_utf8_lossy(outcome.stdout());
    let stderr = String::from_utf8_lossy(outcome.stderr());

    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ),
        "the AppContainer established a TCP connection to the reachable host listener; stdout={stdout:?}, stderr={stderr:?}"
    );
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(28),
        "curl did not report a connection timeout for the blocked listener; stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stderr.contains("curl: (28)"),
        "expected curl's timeout diagnostic; stdout={stdout:?}, stderr={stderr:?}"
    );
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}
