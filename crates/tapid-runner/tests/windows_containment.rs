#![cfg(windows)]

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_runner::{
    AssuranceLevel, CleanupConfidence, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest,
    FilesystemPolicy, SandboxMode, SandboxPolicy, Termination, execute,
};

#[path = "support/windows_acl.rs"]
mod windows_acl;

fn temporary_project(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("tapid-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    // Initialize ONLY this disposable root before any exact restoration snapshot.
    windows_acl::initialize_inheritance(&path);
    path
}

#[derive(Debug, PartialEq, Eq)]
struct DaclSnapshot {
    listing: Vec<u8>,
    control_and_acl: (u16, Vec<u8>),
}

impl std::ops::Deref for DaclSnapshot {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.listing
    }
}

fn project_dacl(path: &PathBuf) -> DaclSnapshot {
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let icacls = PathBuf::from(system_root).join("System32/icacls.exe");
    let output = Command::new(icacls).arg(path).output().unwrap();
    assert!(
        output.status.success(),
        "icacls failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    DaclSnapshot {
        listing: output.stdout,
        control_and_acl: windows_acl::read_acl(path),
    }
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
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
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
    command_request_with_policy(
        root,
        command,
        managed_policy_with_flags(limits, allow_subprocess, network),
    )
}

fn command_request_with_policy(
    root: &PathBuf,
    command: &str,
    policy: SandboxPolicy,
) -> ExecutionRequest {
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let mut arguments: Vec<OsString> = vec!["/D".into(), "/S".into(), "/C".into()];
    arguments.push(command.into());
    ExecutionRequest::builder(system32.join("cmd.exe").into_os_string())
        .args(arguments)
        .executable_search_path(&system32)
        .project_root(root)
        .policy(policy)
        .windows_verbatim_arguments(true)
        .build()
        .unwrap()
}

#[test]
fn windows_managed_tree_executes_with_checked_appcontainer_and_job_evidence() {
    let root = temporary_project("managed-tree");
    let baseline_acl = project_dacl(&root);
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let command = system32.join("cmd.exe");
    let request = ExecutionRequest::builder(command.as_os_str())
        .args([
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
            OsString::from("echo stdout-marker & echo stderr-marker 1>&2"),
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
        outcome.enforcement().assurance(),
        AssuranceLevel::ManagedTree
    );
    assert_eq!(
        outcome.enforcement().backend().name(),
        "tapid-runner/windows-appcontainer-job"
    );
    for dimension in [
        tapid_runner::EnforcementDimension::FilesystemRead,
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
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "normal exit must restore the project DACL"
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
    let baseline_acl = project_dacl(&root);
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
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "output-limit cleanup must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_timeout_terminates_the_managed_job() {
    let root = temporary_project("timeout");
    let baseline_acl = project_dacl(&root);
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
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "timeout cleanup must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_ctrl_c_cancels_execution_and_restores_project_dacl() {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    const HELPER_ENV: &str = "TAPID_WINDOWS_CTRL_C_HELPER";
    if std::env::var_os(HELPER_ENV).is_some() {
        windows_ctrl_c_helper();
        return;
    }

    let started = Instant::now();
    let executable = std::env::current_exe().expect("current test executable path");
    let mut helper = Command::new(&executable)
        .args([
            "--exact",
            "windows_ctrl_c_cancels_execution_and_restores_project_dacl",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "1")
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while the helper runs: diagnostic output must not block its exit.
    let mut stdout_pipe = helper.stdout.take().unwrap();
    let mut stderr_pipe = helper.stderr.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout_pipe.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr_pipe.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let helper_pid = helper.id();
    eprintln!(
        "[windows-ctrl-c parent] spawned helper pid={helper_pid} executable={} flags=CREATE_NEW_CONSOLE elapsed={:?}",
        executable.display(),
        started.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match helper.try_wait() {
            Ok(Some(_)) => {
                let output = std::process::Output {
                    status: helper.wait().unwrap(),
                    stdout: stdout_reader.join().unwrap(),
                    stderr: stderr_reader.join().unwrap(),
                };
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "[windows-ctrl-c parent] helper exited after {:?}: status={}\nhelper stdout:\n{stdout}\nhelper stderr:\n{stderr}",
                    started.elapsed(),
                    output.status
                );
                assert!(
                    output.status.success(),
                    "isolated Ctrl+C helper failed: status={}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                    output.status
                );
                assert!(
                    stdout.contains("CTRL_C_HELPER_SUCCESS"),
                    "success marker missing; helper stdout:\n{stdout}\nhelper stderr:\n{stderr}"
                );
                break;
            }
            Ok(None) => {}
            Err(error) => {
                let kill = helper.kill();
                let output = std::process::Output {
                    status: helper.wait().unwrap(),
                    stdout: stdout_reader.join().unwrap(),
                    stderr: stderr_reader.join().unwrap(),
                };
                panic!(
                    "cannot poll isolated Ctrl+C helper pid={helper_pid}: {error}; kill={kill:?}; status={}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        if Instant::now() >= deadline {
            let kill = helper.kill();
            let output = std::process::Output {
                status: helper.wait().unwrap(),
                stdout: stdout_reader.join().unwrap(),
                stderr: stderr_reader.join().unwrap(),
            };
            panic!(
                "isolated Ctrl+C helper pid={helper_pid} did not finish before 30s deadline; kill={kill:?}; status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn windows_ctrl_c_helper() {
    use std::sync::mpsc::{self, TryRecvError};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Console::{
        CTRL_C_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList, GetConsoleWindow,
    };

    let started = Instant::now();
    let console_window = unsafe { GetConsoleWindow() } as usize;
    let mut console_processes = [0_u32; 16];
    let console_process_count = unsafe {
        GetConsoleProcessList(
            console_processes.as_mut_ptr(),
            console_processes.len() as u32,
        )
    };
    let copied_process_count = (console_process_count as usize).min(console_processes.len());
    eprintln!(
        "[windows-ctrl-c helper] start pid={} console_window={console_window:#x} console_process_count={console_process_count} console_pids={:?}",
        std::process::id(),
        &console_processes[..copied_process_count]
    );
    let root = temporary_project("ctrl-c");
    let baseline_acl = project_dacl(&root);
    eprintln!(
        "[windows-ctrl-c helper] baseline captured after {:?}: root={} dacl={:?}",
        started.elapsed(),
        root.display(),
        String::from_utf8_lossy(&baseline_acl)
    );
    let request = command_request(
        &root,
        "for /L %i in (1,1,2147483647) do @rem",
        ExecutionLimits::new(Some(30), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    );
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        eprintln!("[windows-ctrl-c worker] execute started");
        let result = execute(&request);
        eprintln!(
            "[windows-ctrl-c worker] execute returned after {:?}: {:?}",
            started.elapsed(),
            result.as_ref().map(|outcome| outcome.termination())
        );
        sender.send(result).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut active_acl = project_dacl(&root);
    while active_acl == baseline_acl {
        match receiver.try_recv() {
            Ok(result) => panic!(
                "execution returned before its project grant became visible: {result:?}; root={}",
                root.display()
            ),
            Err(TryRecvError::Disconnected) => panic!(
                "execution worker disconnected before applying its project grant; finished={}; root={}",
                worker.is_finished(),
                root.display()
            ),
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            let current_acl = project_dacl(&root);
            panic!(
                "execution did not apply its project grant before Ctrl+C; worker_finished={}; root={}; baseline_dacl={:?}; current_dacl={:?}",
                worker.is_finished(),
                root.display(),
                String::from_utf8_lossy(&baseline_acl),
                String::from_utf8_lossy(&current_acl)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
        active_acl = project_dacl(&root);
    }

    eprintln!(
        "[windows-ctrl-c helper] grant active after {:?}: root={} baseline_dacl={:?} active_dacl={:?}",
        started.elapsed(),
        root.display(),
        String::from_utf8_lossy(&baseline_acl),
        String::from_utf8_lossy(&active_acl)
    );
    println!("DACL_ACTIVE");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    // SAFETY: this helper owns an isolated console; the event is sent only after the project DACL
    // shows that the execution is active and its cancellation handler should be installed.
    let event_sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };
    if event_sent == 0 {
        let error = unsafe { GetLastError() };
        panic!(
            "GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) failed with Win32 error {error}; elapsed={:?}; console_window={console_window:#x}; console_pids={:?}",
            started.elapsed(),
            &console_processes[..copied_process_count]
        );
    }
    eprintln!(
        "[windows-ctrl-c helper] control event sent after {:?}; waiting up to 10s; console_pids={:?}",
        started.elapsed(),
        &console_processes[..copied_process_count]
    );
    let outcome = match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result.unwrap_or_else(|error| {
            panic!(
                "execution returned an error after Ctrl+C after {:?}: {error:?}; worker_finished={}",
                started.elapsed(),
                worker.is_finished()
            )
        }),
        Err(error) => {
            let current_acl = project_dacl(&root);
            panic!(
                "execution did not finish after Ctrl+C within 10s: {error:?}; elapsed={:?}; worker_finished={}; dacl_restored={}; root={}; baseline_dacl={:?}; current_dacl={:?}; console_pids={:?}",
                started.elapsed(),
                worker.is_finished(),
                current_acl == baseline_acl,
                root.display(),
                String::from_utf8_lossy(&baseline_acl),
                String::from_utf8_lossy(&current_acl),
                &console_processes[..copied_process_count]
            );
        }
    };
    eprintln!(
        "[windows-ctrl-c helper] outcome after {:?}: termination={:?} cleanup={:?} confirmed={:?} evidence={:?} stdout_bytes={} stderr_bytes={}",
        started.elapsed(),
        outcome.termination(),
        outcome.completion().cleanup_confidence(),
        outcome.completion().confirmed(),
        outcome.completion().evidence(),
        outcome.stdout().len(),
        outcome.stderr().len()
    );
    worker
        .join()
        .unwrap_or_else(|panic| panic!("execution worker panicked: {panic:?}"));
    assert_eq!(outcome.termination(), &Termination::Cancelled);
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    let restored_acl = project_dacl(&root);
    assert_eq!(
        restored_acl,
        baseline_acl,
        "cancellation must restore the exact project DACL; baseline={:?}; restored={:?}",
        String::from_utf8_lossy(&baseline_acl),
        String::from_utf8_lossy(&restored_acl)
    );
    eprintln!(
        "[windows-ctrl-c helper] cleanup verified after {:?}: DACL restored exactly",
        started.elapsed()
    );
    fs::remove_dir_all(root).unwrap();
    println!("CTRL_C_HELPER_SUCCESS elapsed={:?}", started.elapsed());
}

#[test]
fn windows_missing_executable_failure_restores_project_dacl() {
    let root = temporary_project("missing-executable");
    let baseline_acl = project_dacl(&root);
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let missing = root.join("missing.exe");
    let request = ExecutionRequest::builder(missing.as_os_str())
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .build()
        .unwrap();

    let error = execute(&request).expect_err("missing executable must fail before spawn");
    assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "pre-spawn failure must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_disabled_subprocess_policy_prevents_child_process_creation() {
    let root = temporary_project("process-limit");
    let command = r#"start "" /B cmd.exe /D /S /C "echo CHILD_PROCESS_MARKER""#;
    let request = command_request(
        &root,
        command,
        ExecutionLimits::new(Some(3), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("subprocess restriction should remain contained");
    assert!(matches!(
        outcome.termination(),
        Termination::Exited(_) | Termination::TimedOut
    ));
    assert!(!String::from_utf8_lossy(outcome.stdout()).contains("CHILD_PROCESS_MARKER"));
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_can_launch_an_executable_subprocess_from_runtime_paths() {
    let root = temporary_project("runtime-child-process");
    let executable =
        fs::canonicalize(std::env::current_exe().expect("containment test executable path"))
            .expect("canonical containment test executable path");
    let executable_directory = executable.parent().expect("test executable directory");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let request = ExecutionRequest::builder(executable.as_os_str())
        .args([
            OsString::from("--exact"),
            OsString::from("windows_child_process_probe_helper"),
            OsString::from("--nocapture"),
        ])
        .executable_search_path(executable_directory)
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(10), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).expect("probe process should remain within the Job Object");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(
        String::from_utf8_lossy(outcome.stdout()).contains("CHILD_PROCESS_MARKER"),
        "child process did not produce its marker"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_child_process_probe_helper() {
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let child = PathBuf::from(system_root).join("System32").join("cmd.exe");
    let status = Command::new(&child)
        .args(["/D", "/S", "/C", "echo CHILD_PROCESS_MARKER"])
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "CreateProcess for {} failed: kind={:?}, raw_os_error={:?}, error={error}",
                child.display(),
                error.kind(),
                error.raw_os_error()
            )
        });
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn windows_node_runtime_can_spawn_a_child_inside_the_appcontainer() {
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
    let root = temporary_project("node-child-process");
    let script = r#"const { spawnSync } = require('node:child_process'); const child = spawnSync(process.execPath, ['--version'], { encoding: 'utf8', stdio: 'inherit' }); if (child.error) { console.error(JSON.stringify({ code: child.error.code, errno: child.error.errno, syscall: child.error.syscall, message: child.error.message })); process.exit(1); } process.stdout.write(child.stdout || ''); process.stderr.write(child.stderr || ''); process.exit(child.status ?? 1);"#;
    let request = ExecutionRequest::builder(node.as_os_str())
        .args([OsString::from("-e"), OsString::from(script)])
        .executable_search_paths([runtime_bin.as_path(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(15), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).expect("Node child-process probe must be contained");
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
fn windows_node_runtime_can_resolve_a_script_file_inside_the_project_grant() {
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
    let root = temporary_project("node-script-file");
    let script = root.join("smoke.js");
    fs::write(&script, "process.stdout.write(require('./helper').value);").unwrap();
    fs::write(
        root.join("helper.js"),
        "exports.value = 'PROJECT_SCRIPT_MARKER';",
    )
    .unwrap();
    let request = ExecutionRequest::builder(node.as_os_str())
        .arg(script.as_os_str())
        .executable_search_paths([runtime_bin.as_path(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(15), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).expect("Node script-file probe must be contained");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"PROJECT_SCRIPT_MARKER");
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
fn windows_node_timeout_reaps_descendant_before_restoring_project_dacl() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let system32 =
        fs::canonicalize(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"))
            .unwrap();
    let root = temporary_project("node-descendant-timeout");
    let baseline_acl = project_dacl(&root);
    let script = r#"const {spawn} = require('node:child_process'); const child = spawn(process.execPath, ['-e', 'console.log("NODE_DESCENDANT_ACTIVE"); setInterval(() => {}, 1000);'], {stdio: 'inherit'}); child.on('error', error => {console.error(error); process.exit(1);}); child.on('exit', (code, signal) => console.error(JSON.stringify({code, signal})));"#;
    let request = ExecutionRequest::builder(node.as_os_str())
        .args(["-e", script])
        .executable_search_paths([node.parent().unwrap(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(3), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::TimedOut,
        "stderr={}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"NODE_DESCENDANT_ACTIVE\n");
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(project_dacl(&root), baseline_acl);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_node_preserves_argv_environment_exit_and_readonly_boundaries() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let runtime_bin = node.parent().unwrap();
    let system32 =
        fs::canonicalize(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"))
            .unwrap();
    let root = temporary_project("node-contract");
    let outside = root.with_extension("private.txt");
    fs::write(&outside, "PRIVATE_MARKER").unwrap();
    let forbidden_write = outside.with_extension("write.txt");
    fs::write(root.join("allowed.txt"), "READ_GRANTED").unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(std::net::TcpStream::connect(address).unwrap());
    drop(listener.accept().unwrap());
    let script = root.join("probe.js");
    fs::write(
        &script,
        r#"
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
assert.deepEqual(process.argv.slice(2, 6), ['space value', 'quote"value', 'tail\\', '']);
assert.equal(process.env.TAPID_TEST_MARKER, 'allowed-value');
assert.equal(process.env.TAPID_SECRET_NOT_ALLOWED, undefined);
assert.equal(fs.readFileSync('allowed.txt', 'utf8'), 'READ_GRANTED');
assert.throws(() => fs.readFileSync(process.argv[6]), error => ['EACCES', 'EPERM'].includes(error.code));
assert.throws(() => fs.writeFileSync(process.argv[7], 'escape'), error => ['EACCES', 'EPERM'].includes(error.code));
assert.throws(() => fs.writeFileSync('allowed.txt', 'overwrite'), error => ['EACCES', 'EPERM'].includes(error.code));
assert.throws(() => fs.writeFileSync('new.txt', 'create'), error => ['EACCES', 'EPERM'].includes(error.code));
const socket = net.connect({host: '127.0.0.1', port: Number(process.argv[8])});
socket.on('connect', () => { console.error('NETWORK_ESCAPE'); process.exit(1); });
socket.on('error', () => finish());
socket.setTimeout(1000, () => finish());
let finished = false;
function finish() {
  if (finished) return;
  finished = true;
  socket.destroy();
  process.stdout.write('NODE_CONTRACT_OK');
  process.stderr.write('NODE_STDERR_OK');
  process.exitCode = 23;
}
"#,
    )
    .unwrap();
    let baseline_acl = project_dacl(&root);
    let request = ExecutionRequest::builder(node.as_os_str())
        .args([
            script.as_os_str().to_owned(),
            "space value".into(),
            "quote\"value".into(),
            "tail\\".into(),
            "".into(),
            outside.as_os_str().to_owned(),
            forbidden_write.as_os_str().to_owned(),
            address.port().to_string().into(),
        ])
        .executable_search_paths([runtime_bin, system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .env("TAPID_TEST_MARKER", "allowed-value")
        .policy(managed_policy())
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(23),
        "stdout={} stderr={}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"NODE_CONTRACT_OK");
    assert_eq!(outcome.stderr(), b"NODE_STDERR_OK");
    assert_eq!(
        fs::read_to_string(root.join("allowed.txt")).unwrap(),
        "READ_GRANTED"
    );
    assert!(!forbidden_write.exists());
    assert!(!root.join("new.txt").exists());
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(project_dacl(&root), baseline_acl);
    fs::remove_file(outside).unwrap();
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
fn windows_write_policy_fails_closed_before_spawn_until_native_acceptance() {
    let root = temporary_project("write-policy-pending");
    let writable = root.join("writable");
    fs::create_dir(&writable).unwrap();
    let marker = writable.join("must-not-spawn.txt");
    let command = format!("echo should-not-run>\"{}\"", marker.display());
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec!["writable".into()]).unwrap(),
        false,
        vec!["TAPID_TEST_MARKER".into()],
        false,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let request = command_request_with_policy(&root, &command, policy);

    let error = execute(&request).expect_err("unverified Windows writes must fail closed");
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(!marker.exists(), "unsupported write policy started a child");
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "re-enable after AppContainer existing-file writes pass on Windows 11"]
fn windows_appcontainer_can_modify_existing_file_in_declared_subtree() {
    let root = temporary_project("write-existing-file");
    let writable = root.join("writable");
    fs::create_dir(&writable).unwrap();
    let authorized = writable.join("authorized.txt");
    fs::write(&authorized, "preexisting content").unwrap();
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let icacls = fs::canonicalize(PathBuf::from(system_root).join("System32/icacls.exe")).unwrap();
    let label = Command::new(&icacls)
        .arg(&writable)
        .args(["/setintegritylevel", "(OI)(CI)L", "/c"])
        .output()
        .unwrap();
    assert!(
        label.status.success(),
        "icacls failed to set the test low-integrity label: {}{}",
        String::from_utf8_lossy(&label.stdout),
        String::from_utf8_lossy(&label.stderr)
    );
    let command = format!("echo TAPID_WRITE_GRANTED>\"{}\"", authorized.display());
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec!["writable".into()]).unwrap(),
        false,
        vec!["TAPID_TEST_MARKER".into()],
        false,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let request = command_request_with_policy(&root, &command, policy);

    let outcome = execute(&request).expect("a declared write grant should execute");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(
        fs::read_to_string(&authorized).unwrap().trim(),
        "TAPID_WRITE_GRANTED"
    );
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
