//! Native acceptance at the CLI/result-file seam; no runner-only receipts.
use super::*;
use std::io::Read;
use std::process::{Child, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Running {
    child: Child,
    stdout: Receiver<Vec<u8>>,
    stderr: Receiver<Vec<u8>>,
}

fn drain(pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        let _ = sender.send(bytes);
    });
    receiver
}

impl Running {
    fn spawn(command: &mut Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = drain(child.stdout.take().unwrap());
        let stderr = drain(child.stderr.take().unwrap());
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn finish(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(45);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI/helper exceeded outer deadline"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // Bound reader completion too: inherited pipes must never hang the test.
        Output {
            status,
            stdout: self
                .stdout
                .recv_timeout(Duration::from_secs(5))
                .expect("stdout EOF"),
            stderr: self
                .stderr
                .recv_timeout(Duration::from_secs(5))
                .expect("stderr EOF"),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // On assertion failure, closing Tapid also closes its kill-on-close Job.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn assert_interruption(output: &Output, path: &Path, termination: &str, timeout: u64) {
    assert_eq!(
        output.status.code(),
        Some(if termination == "Cancelled" { 130 } else { 1 }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"CHILD_READY\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("CHILD_ERROR\n"), "{stderr}");
    assert!(
        !stderr.contains("schema_version"),
        "result must not be on stderr"
    );
    let bytes = fs::read(path).expect("published result");
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "run");
    assert_eq!(result["outcome"], "completed");
    assert!(result["error"].is_null());
    let receipt = &result["receipt"];
    assert_eq!(receipt["termination"], termination);
    assert_eq!(receipt["assurance"], "ManagedTree");
    assert_eq!(
        receipt["backend"]["name"],
        if cfg!(windows) {
            "tapid-runner/windows-appcontainer-job"
        } else {
            "tapid-runner/linux-pidns-cgroup-v2-managed"
        }
    );
    assert_eq!(
        receipt["configured_limits"],
        serde_json::json!({"timeout_seconds": timeout, "max_output_bytes": 65536, "max_processes": null, "max_memory_bytes": null})
    );
    for dimension in [
        "timeout",
        "output",
        "process_tree_membership",
        "descendant_lifecycle",
        "complete_cleanup",
    ] {
        assert_eq!(receipt["requested"][dimension], true, "{dimension}");
        assert_eq!(receipt["enforced"][dimension], true, "{dimension}");
    }
    assert!(
        receipt["established_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["dimension"] == "Timeout"
                && !item["mechanism"].as_str().unwrap().is_empty())
    );
    for dimension in ["memory", "process_count"] {
        assert_eq!(receipt["requested"][dimension], false);
        assert_eq!(receipt["enforced"][dimension], false);
    }
    let completion = &receipt["completion"];
    assert_eq!(completion["cleanup_confidence"], "KernelOwnedComplete");
    for (dimension, confirmed) in completion["confirmed"].as_object().unwrap() {
        assert_eq!(
            confirmed,
            matches!(
                dimension.as_str(),
                "descendant_lifecycle" | "process_tree_membership" | "complete_cleanup"
            ),
            "{dimension}"
        );
    }
    let evidence = completion["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 3);
    for dimension in [
        "DescendantLifecycle",
        "ProcessTreeMembership",
        "CompleteCleanup",
    ] {
        assert!(evidence.iter().any(|item| {
            item["dimension"] == dimension
                && item["scope"] == "ManagedTree"
                && item["mechanism"]
                    .as_str()
                    .unwrap()
                    .contains(if cfg!(windows) {
                        "Job Object active-process count reached zero"
                    } else {
                        "cgroup.kill followed by populated=0"
                    })
        }));
    }
    let policy = &result["policy_inputs"];
    assert_eq!(policy["source"], "script-profile");
    assert_eq!(policy["effective"], policy["configuration"]);
    assert_eq!(policy["effective"]["assurance"], "ManagedTree");
    assert_eq!(policy["effective"]["read"], serde_json::json!(["."]));
    assert_eq!(policy["effective"]["write"], serde_json::json!(["ready"]));
    assert_eq!(policy["effective"]["network"], false);
    assert_eq!(policy["effective"]["limits"]["timeout_seconds"], timeout);
    assert_eq!(policy["invocation"]["read"], serde_json::json!([]));
}

#[allow(
    unsafe_code,
    reason = "isolated console event exercises supported native cancellation"
)]
fn interruption(cancel: bool) {
    let project = project();
    let results = tapid_test_support::TempProject::new("interruption-results").unwrap();
    let temporary = tapid_test_support::TempProject::new("interruption-temp").unwrap();
    let (_runtime, node) = private_node_runtime();
    let path = results.path().join("result.json");
    let timeout = if cancel { 30 } else { 5 };
    project.write("ready", b"").unwrap();
    project.write("probe.js", b"process.stdout.write('CHILD_READY\\n');process.stderr.write('CHILD_ERROR\\n');require('node:fs').writeFileSync('ready','ready');setInterval(()=>{},1000);").unwrap();
    project.write("tapid.toml", format!("[run.scripts.probe]\nassurance='managed-tree'\nread=['.']\nwrite=['ready']\nsubprocess=true\nnetwork=false\ntimeout_seconds={timeout}\nmax_output_bytes=65536\n").as_bytes()).unwrap();
    let mut command = isolated_command(
        project.path(),
        &[
            "run",
            "probe",
            "--node-runtime",
            node.to_str().unwrap(),
            "--result-file",
            path.to_str().unwrap(),
        ],
        true,
    );
    command
        .env("TEMP", temporary.path())
        .env("TMP", temporary.path());
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
    let mut running = Running::spawn(&mut command);
    if cancel {
        let deadline = Instant::now() + Duration::from_secs(15);
        while fs::read(project.path().join("ready")).unwrap() != b"ready" {
            assert!(
                running.child.try_wait().unwrap().is_none(),
                "CLI exited before readiness"
            );
            assert!(Instant::now() < deadline, "child readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Console::{CTRL_C_EVENT, GenerateConsoleCtrlEvent};
            // SAFETY: the helper owns this console; readiness proves Tapid's cancellation scope is active.
            assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
        }
    }
    let output = running.finish();
    assert_interruption(
        &output,
        &path,
        if cancel { "Cancelled" } else { "TimedOut" },
        timeout,
    );
}

#[cfg(windows)]
#[test]
fn run_result_file_windows_timeout_publishes_checked_completion() {
    interruption(false);
}

#[cfg(windows)]
#[test]
#[allow(
    unsafe_code,
    reason = "isolated console event exercises supported native cancellation"
)]
fn run_result_file_windows_cancellation_publishes_checked_completion() {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    const HELPER: &str = "TAPID_RESULT_CANCELLATION_HELPER";
    if std::env::var_os(HELPER).is_some() {
        // Handle events in this helper without setting inherited Ctrl+C-ignore state.
        unsafe extern "system" fn preserve_helper(_: u32) -> i32 {
            1
        }
        // SAFETY: callback lives for the entire isolated helper process.
        assert_ne!(
            unsafe { SetConsoleCtrlHandler(Some(preserve_helper), 1) },
            0
        );
        interruption(true);
        return;
    }
    let name = format!(
        "{}::run_result_file_windows_cancellation_publishes_checked_completion",
        module_path!().split_once("::").unwrap().1
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(HELPER, "1")
        .creation_flags(0x00000010); // CREATE_NEW_CONSOLE: never broadcast into the test runner's console.
    let output = Running::spawn(&mut command).finish();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[cfg(target_os = "linux")]
#[test]
fn run_result_file_linux_managed_timeout_publishes_checked_completion() {
    if std::env::var_os("TAPID_REQUIRE_MANAGED_ASSERTIONS").is_none() {
        eprintln!(
            "unavailable: set TAPID_REQUIRE_MANAGED_ASSERTIONS=1 with delegated cgroup v2 and namespaces to require native timeout delivery"
        );
        return;
    }
    interruption(false);
}
