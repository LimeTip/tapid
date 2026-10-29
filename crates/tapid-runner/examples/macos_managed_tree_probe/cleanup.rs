//! Bounded counterexamples to treating root completion as tree completion.
use super::record::Observation;
use std::{
    fs, io,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tapid_test_support::TempProject;

fn spawn(args: &[&str]) -> io::Result<Child> {
    Command::new(std::env::current_exe()?)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn ready(path: &Path, child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!("premature exit: {status}")));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "readiness deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn worker(action: &str) -> io::Result<()> {
    if !matches!(action, "normal" | "error" | "hold" | "crash" | "controller") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown failure injection",
        ));
    }
    if action == "controller" {
        let mut worker = spawn(&["cleanup-worker", "hold"])?;
        ready(Path::new("ready"), &mut worker)?;
        fs::write("controller-ready", b"acknowledged worker")?;
        std::thread::sleep(Duration::from_millis(1700));
        worker.wait()?;
        return Ok(());
    }
    let mut leaf = spawn(&["child", "double-fork", "600"])?;
    // Intermediate double-fork parents exit successfully before the leaf ack.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !Path::new("ready").exists() {
        if let Some(status) = leaf.try_wait()?
            && !status.success()
        {
            return Err(io::Error::other(format!("leaf setup failed: {status}")));
        }
        if Instant::now() >= deadline {
            if leaf.try_wait()?.is_none() {
                leaf.kill()?;
            }
            leaf.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "leaf readiness deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    leaf.wait()?;
    match action {
        "error" => Err(io::Error::other("acknowledged injected worker error")),
        "crash" => {
            // Kill only this disposable fixture, never a discovered PID.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            Err(io::Error::other("self SIGKILL unexpectedly returned"))
        }
        "hold" => {
            std::thread::sleep(Duration::from_millis(1500));
            Ok(())
        }
        _ => Ok(()),
    }
}

pub fn run() -> io::Result<()> {
    for case in [
        "normal",
        "error",
        "cancellation",
        "timeout",
        "worker-failure",
        "controller-kill",
    ] {
        let root = TempProject::new("cleanup-research")?;
        let action = match case {
            "cancellation" | "timeout" => "hold",
            "worker-failure" => "crash",
            "controller-kill" => "controller",
            other => other,
        };
        let mut result = Observation::diagnostic(case, "complete descendant cleanup");
        result.threshold = 600;
        let mut child = Command::new(std::env::current_exe()?)
            .args(["cleanup-worker", action])
            .env_clear()
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let ack = if case == "controller-kill" {
            "controller-ready"
        } else {
            "ready"
        };
        if let Err(error) = ready(&root.path().join(ack), &mut child) {
            result.observed = error.to_string();
            if child.try_wait()?.is_none() {
                child.kill()?;
                child.wait()?;
            }
            result.rescue = "harness killed direct child; leaf finite work only";
            result.emit()?;
            return Err(error);
        }
        result.setup_ready = true;
        if matches!(case, "cancellation" | "timeout" | "controller-kill") {
            if case == "timeout" {
                std::thread::sleep(Duration::from_millis(30));
            }
            child.kill()?;
            result.rescue = "harness killed direct root only; no tree cleanup";
        }
        let status = child.wait()?;
        let premature_marker = root.path().join("delayed-marker").exists();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !root.path().join("delayed-marker").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let marker = root.path().join("delayed-marker").exists();
        result.positive_control = marker && !premature_marker;
        result.violation_observed = result.positive_control;
        result.observed = format!(
            "root_status={status}; marker_at_root_exit={premature_marker}; delayed_marker={marker}; authoritative_emptiness=unavailable"
        );
        // The controller-death control also leaves a finite holding worker.
        // Wait for its bounded work before removing the fixture directory.
        if case == "controller-kill" {
            std::thread::sleep(Duration::from_millis(1700));
        }
        result.emit()?;
        if !result.positive_control {
            return Err(io::Error::other("cleanup positive control failed"));
        }
    }
    let mut supervisor =
        Observation::diagnostic("supervisor-failure", "candidate supervisor cleanup");
    supervisor.candidate_available = false;
    supervisor.observed =
        "no independently supervising candidate established; not simulated by PID polling".into();
    supervisor.emit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premature_exit_is_not_a_readiness_acknowledgement() {
        let root = TempProject::new("missing-ack").unwrap();
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let error = ready(&root.path().join("ready"), &mut child).unwrap_err();
        assert!(error.to_string().contains("premature exit"));
        assert!(child.wait().unwrap().success());
    }
}
