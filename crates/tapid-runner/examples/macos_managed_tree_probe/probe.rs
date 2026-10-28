#[cfg(target_os = "macos")]
#[path = "xpc.rs"]
mod xpc;
use std::io;

#[cfg(target_os = "macos")]
#[path = "authority.rs"]
mod authority;
#[cfg(target_os = "macos")]
#[path = "cleanup.rs"]
mod cleanup;
#[path = "record.rs"]
#[cfg(any(test, target_os = "macos"))]
mod record;
#[cfg(target_os = "macos")]
#[path = "workload.rs"]
mod workload;

pub fn run(args: Vec<String>) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if std::env::current_exe()?
        .file_name()
        .is_some_and(|name| name == "Worker")
    {
        return xpc::service();
    }
    match args.as_slice() {
        #[cfg(target_os = "macos")]
        [command] if command == "authority" => authority::run(),
        #[cfg(target_os = "macos")]
        [command, expected, outside, tcp, unix, depth] if command == "authority-payload" => {
            authority::payload(expected, outside, tcp, unix, depth)
        }
        #[cfg(target_os = "macos")]
        [command] if command == "limits" => workload::run(),
        #[cfg(target_os = "macos")]
        [command, kind, transition] if command == "workload" => workload::child(kind, transition),
        #[cfg(target_os = "macos")]
        [command] if command == "cleanup" => cleanup::run(),
        #[cfg(target_os = "macos")]
        [command, action] if command == "cleanup-worker" => cleanup::worker(action),
        #[cfg(target_os = "macos")]
        [command] if command == "xpc" => xpc::client(),
        #[cfg(target_os = "macos")]
        [command] if command == "lineage" => lineage(),
        #[cfg(target_os = "macos")]
        [command, transition] if command == "worker" => worker(transition),
        #[cfg(target_os = "macos")]
        [command, transition, delay] if command == "child" => child(transition, delay),
        [command] if command == "rejection" => rejection(),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: macos_managed_tree_probe rejection|lineage|cleanup|limits|authority|xpc (native macOS only)",
        )),
    }
}

#[cfg(not(target_os = "macos"))]
fn rejection() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native macOS required",
    ))
}

#[cfg(target_os = "macos")]
fn rejection() -> io::Result<()> {
    use sha2::{Digest, Sha256};
    use std::{fs, process::Command, time::Instant};
    use tapid_runner::{
        AssuranceLevel, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest,
        FilesystemPolicy, SandboxMode, SandboxPolicy, Termination, execute,
    };
    use tapid_test_support::{TempHome, TempProject};

    let root = TempProject::new("managed-tree-research")?;
    let home = TempHome::new("managed-tree-research")?;
    let project = fs::canonicalize(root.path())?;
    let executable = std::env::current_exe()?;
    println!("development_evidence=true managed_tree_supported=false");
    println!("binary_sha256={:x}", Sha256::digest(fs::read(executable)?));
    for (program, args) in [
        ("/usr/bin/sw_vers", vec![]),
        ("/usr/bin/uname", vec!["-a"]),
        ("/usr/bin/xcrun", vec!["--show-sdk-version"]),
        ("/usr/bin/id", vec!["-u"]),
    ] {
        let output = Command::new(program).args(&args).output()?;
        println!(
            "metadata command={program} args={args:?} status={} stdout={:?} stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            return Err(io::Error::other("environment metadata command failed"));
        }
    }
    let cases = [
        (
            "positive-marker",
            AssuranceLevel::Restricted,
            ExecutionLimits::default(),
        ),
        (
            "managed-tree",
            AssuranceLevel::ManagedTree,
            ExecutionLimits::default(),
        ),
        (
            "timeout-seconds",
            AssuranceLevel::Restricted,
            ExecutionLimits::new(Some(1), None, None, None).map_err(io::Error::other)?,
        ),
        (
            "combined-output-bytes",
            AssuranceLevel::Restricted,
            ExecutionLimits::new(None, Some(1024), None, None).map_err(io::Error::other)?,
        ),
        (
            "process-count",
            AssuranceLevel::Restricted,
            ExecutionLimits::new(None, None, Some(4), None).map_err(io::Error::other)?,
        ),
        (
            "memory-bytes",
            AssuranceLevel::Restricted,
            ExecutionLimits::new(None, None, None, Some(16 * 1024 * 1024))
                .map_err(io::Error::other)?,
        ),
    ];
    let mut failures = 0;
    for (case, assurance, limits) in cases {
        let marker = project.join("executed");
        if marker.exists() {
            fs::remove_file(&marker)?;
        }
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            assurance,
            FilesystemPolicy::new(vec![".".into()], vec![".".into()]).map_err(io::Error::other)?,
            false,
            vec!["HOME".into()],
            false,
            limits.clone(),
        )
        .map_err(io::Error::other)?;
        let request = ExecutionRequest::builder("/bin/sh")
            .args(["-c", "printf generated > executed"])
            .project_root(&project)
            .policy(policy)
            .envs([("HOME", home.path().as_os_str())])
            .executable_search_path("/usr/bin")
            .build()
            .map_err(io::Error::other)?;
        let start = Instant::now();
        let result = execute(&request);
        println!(
            "case={case} requested={assurance:?} limits={limits:?} elapsed_us={} result={result:?} marker={} candidate_cleanup=unverified harness_rescue=fixture-removal",
            start.elapsed().as_micros(),
            marker.exists()
        );
        if case == "positive-marker" {
            if !matches!(result, Ok(ref outcome) if outcome.termination() == &Termination::Exited(0))
                || fs::read(&marker).unwrap_or_default() != b"generated"
            {
                println!("case={case} verdict=unverified reason=positive-fixture-failure");
                failures += 1;
            }
        } else if !matches!(result, Err(ref e) if e.category() == ExecutionErrorCategory::UnsupportedContainment)
            || marker.exists()
        {
            println!("case={case} verdict=unverified reason=unexpected-rejection-outcome");
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(io::Error::other(format!(
            "{failures} fixture or rejection failures; no containment verdict"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_configuration_never_dispatches_a_payload() {
        for args in [vec![], vec!["unknown".into()], vec!["worker".into()]] {
            assert_eq!(run(args).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn fixture_failure_and_rescue_never_establish_candidate_enforcement() {
        use record::{Observation, Verdict};
        let mut observation = Observation::diagnostic("normal-exit", "descendant cleanup");
        assert_eq!(observation.verdict(), Verdict::HarnessFailure);
        observation.setup_ready = true;
        observation.positive_control = true;
        assert_eq!(observation.verdict(), Verdict::Unverified);
        observation.violation_observed = true;
        assert_eq!(observation.verdict(), Verdict::Counterexample);
        observation.rescue = "root killed by harness";
        assert_eq!(observation.verdict(), Verdict::Counterexample);
        observation.cleanup_error = Some("wait failed".into());
        assert_eq!(observation.verdict(), Verdict::HarnessFailure);
        assert!(observation.emit().is_ok());
        observation.cleanup_error = None;
        observation.candidate_available = false;
        assert_eq!(observation.verdict(), Verdict::Unsupported);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cleanup_fixture_rejects_unknown_failure_before_spawn() {
        assert_eq!(
            cleanup::worker("unknown").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn workload_rejects_unknown_kind_or_transition_before_allocation() {
        for (kind, transition) in [("unknown", "ordinary"), ("memory", "unknown")] {
            assert_eq!(
                workload::child(kind, transition).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn authority_rejects_unknown_expectation_without_file_access() {
        assert_eq!(
            authority::payload("unknown", "missing", "invalid", "missing", "leaf")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod native_tests {
    #[test]
    fn child_payload_rejects_unbounded_delay_and_unknown_transition() {
        for (transition, delay) in [("setsid", "999999"), ("unknown", "1"), ("setsid", "0")] {
            assert!(super::child(transition, delay).is_err());
        }
    }
}

#[cfg(target_os = "macos")]
fn child(transition: &str, delay: &str) -> io::Result<()> {
    use std::{fs, time::Duration};
    let delay: u64 = delay
        .parse()
        .map_err(|_| io::Error::other("invalid delay"))?;
    if !(1..=2000).contains(&delay)
        || !matches!(
            transition,
            "ordinary" | "setsid" | "setpgid" | "double-fork"
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bounded child: ordinary|setsid|setpgid|double-fork, 1..2000 ms",
        ));
    }
    if transition == "double-fork" {
        return double_fork_exec(delay);
    }
    // Each mode runs in its own fresh single-threaded executable. These calls
    // deliberately demonstrate transitions, not authoritative tree membership.
    let result = match transition {
        "setsid" => unsafe { libc::setsid() },
        "setpgid" => unsafe { libc::setpgid(0, 0) },
        _ => 0,
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    fs::write(
        "ready.pending",
        format!(
            "pid={} ppid={} pgid={} sid={}\n",
            std::process::id(),
            unsafe { libc::getppid() },
            unsafe { libc::getpgrp() },
            unsafe { libc::getsid(0) }
        ),
    )?;
    fs::rename("ready.pending", "ready")?;
    std::thread::sleep(Duration::from_millis(delay));
    fs::write("delayed-marker", b"observed; not authoritative emptiness")
}

#[cfg(target_os = "macos")]
fn worker(transition: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};
    if !matches!(
        transition,
        "ordinary" | "setsid" | "setpgid" | "double-fork"
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown transition",
        ));
    }
    let mut child = Command::new(std::env::current_exe()?)
        .args(["child", transition, "300"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let start = std::time::Instant::now();
    while !std::path::Path::new("ready").exists() {
        if let Some(status) = child.try_wait()?
            && !status.success()
        {
            return Err(io::Error::other(format!(
                "child failed before acknowledgement: {status}"
            )));
        }
        if start.elapsed() > std::time::Duration::from_secs(2) {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::other("child acknowledgement deadline"));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    println!(
        "worker acknowledged child={}; exiting without candidate cleanup",
        child.id()
    );
    // The leaf self-expires; dropping Child intentionally does not kill it.
    Ok(())
}

#[cfg(target_os = "macos")]
fn lineage() -> io::Result<()> {
    use std::{
        fs,
        process::Command,
        time::{Duration, Instant},
    };
    use tapid_test_support::TempProject;
    println!(
        "candidate=unsandboxed-diagnostic requested=observed-lineage enforced=none authoritative_boundary=none candidate_cleanup=none"
    );
    for transition in ["ordinary", "setsid", "setpgid", "double-fork"] {
        let mut observation = record::Observation::diagnostic(transition, "cleanup on worker exit");
        let root = TempProject::new("lineage-diagnostic")?;
        let start = Instant::now();
        let status = Command::new(std::env::current_exe()?)
            .args(["worker", transition])
            .env_clear()
            .current_dir(root.path())
            .status()?;
        if !status.success() {
            observation.observed = format!("worker exited: {status}");
            observation.emit()?;
            return Err(io::Error::other("worker fixture failed"));
        }
        if root.path().join("delayed-marker").exists() {
            return Err(io::Error::other(
                "inconclusive fixture: marker already present at worker exit",
            ));
        }
        let acknowledged = fs::read_to_string(root.path().join("ready"))?;
        observation.setup_ready = true;
        while !root.path().join("delayed-marker").exists()
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let observed = root.path().join("delayed-marker").exists();
        observation.positive_control = observed;
        observation.violation_observed = observed;
        observation.observed = format!("{acknowledged} delayed_marker={observed}");
        observation.emit()?;
        println!(
            "case={transition} worker_status={status} acknowledgement={acknowledged:?} delayed_marker={observed} elapsed_us={} verdict=diagnostic-only harness_rescue=finite-child-self-expiry",
            start.elapsed().as_micros()
        );
        if !observed {
            return Err(io::Error::other("self-expiring child fixture failed"));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn double_fork_exec(delay: u64) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    // Prepare every allocation before fork. The child branches use only
    // async-signal-safe syscalls until exec, even if the runtime used threads.
    let executable =
        CString::new(std::env::current_exe()?.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let delay = CString::new(delay.to_string()).map_err(io::Error::other)?;
    let argv = [
        executable.as_ptr(),
        c"child".as_ptr(),
        c"setsid".as_ptr(),
        delay.as_ptr(),
        std::ptr::null(),
    ];
    unsafe {
        let first = libc::fork();
        if first < 0 {
            return Err(io::Error::last_os_error());
        }
        if first > 0 {
            libc::_exit(0);
        }
        let second = libc::fork();
        if second < 0 {
            libc::_exit(126);
        }
        if second > 0 {
            libc::_exit(0);
        }
        libc::execv(executable.as_ptr(), argv.as_ptr());
        libc::_exit(127);
    }
}
