//! Finite inputs for future candidate accounting experiments. No limit backend.
use super::record::Observation;
use std::{
    fs,
    io::{self, Write},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tapid_test_support::TempProject;

const MEMORY: usize = 2 * 1024 * 1024;
const OUTPUT_PER_STREAM: usize = 8192;

pub fn child(kind: &str, transition: &str) -> io::Result<()> {
    if !matches!(kind, "time" | "output" | "processes" | "memory")
        || !matches!(transition, "ordinary" | "setsid")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown bounded workload",
        ));
    }
    if transition == "setsid" && unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    fs::write("ready", b"workload ready")?;
    match kind {
        "memory" => {
            let mut bytes = vec![0u8; MEMORY];
            // Touch every byte and retain the allocation across the overlap.
            bytes.fill(0x5a);
            std::hint::black_box(&bytes);
            fs::write("allocated", MEMORY.to_string())?;
            std::thread::sleep(Duration::from_millis(200));
            std::hint::black_box(bytes);
        }
        "output" => {
            let bytes = [b'x'; OUTPUT_PER_STREAM];
            io::stdout().write_all(&bytes)?;
            io::stderr().write_all(&bytes)?;
        }
        _ => std::thread::sleep(Duration::from_millis(200)),
    }
    fs::write("completed", b"finite workload completed")
}

struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        // Direct children only; rescue never establishes tree cleanup.
        for child in &mut self.0 {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

pub fn run() -> io::Result<()> {
    for kind in ["time", "output", "processes", "memory"] {
        for count in [1, 3] {
            for transition in ["ordinary", "setsid"] {
                let root = TempProject::new("bounded-accounting")?;
                let output = fs::File::create(root.path().join("combined-output"))?;
                let mut children = Children(Vec::new());
                let start = Instant::now();
                for index in 0..count {
                    let directory = root.path().join(index.to_string());
                    fs::create_dir(&directory)?;
                    // Session-changing controls close both streams. Workload
                    // completion/ack files remain independent of output EOF.
                    let (stdout, stderr) = if transition == "setsid" {
                        (Stdio::null(), Stdio::null())
                    } else {
                        (
                            Stdio::from(output.try_clone()?),
                            Stdio::from(output.try_clone()?),
                        )
                    };
                    children.0.push(
                        Command::new(std::env::current_exe()?)
                            .args(["workload", kind, transition])
                            .env_clear()
                            .current_dir(directory)
                            .stdin(Stdio::null())
                            .stdout(stdout)
                            .stderr(stderr)
                            .spawn()?,
                    );
                }
                let deadline = start + Duration::from_secs(3);
                loop {
                    let mut complete = true;
                    for child in &mut children.0 {
                        match child.try_wait()? {
                            Some(status) if !status.success() => {
                                return Err(io::Error::other(format!("workload failed: {status}")));
                            }
                            None => complete = false,
                            _ => {}
                        }
                    }
                    if complete {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "harness workload deadline",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                let mut record = Observation::diagnostic(kind, "tree-wide limit enforcement");
                record.setup_ready =
                    (0..count).all(|i| root.path().join(i.to_string()).join("ready").exists());
                record.positive_control =
                    (0..count).all(|i| root.path().join(i.to_string()).join("completed").exists());
                let captured = fs::metadata(root.path().join("combined-output"))?.len();
                if kind == "output"
                    && transition == "ordinary"
                    && captured != (count * OUTPUT_PER_STREAM * 2) as u64
                {
                    return Err(io::Error::other(
                        "combined output fixture byte count mismatch",
                    ));
                }
                if kind == "memory" {
                    for i in 0..count {
                        if fs::read_to_string(root.path().join(i.to_string()).join("allocated"))?
                            != MEMORY.to_string()
                        {
                            return Err(io::Error::other(
                                "memory fixture acknowledgement mismatch",
                            ));
                        }
                    }
                }
                (record.threshold, record.units) = match kind {
                    "time" => (50, "milliseconds requested; not configured"),
                    "output" => (4096, "combined bytes requested; not configured"),
                    "processes" => (2, "tree processes requested; not configured"),
                    _ => (
                        4 * 1024 * 1024,
                        "tree memory bytes requested; not configured",
                    ),
                };
                record.observed = format!(
                    "transition={transition}; leaf_count={count}; elapsed_us={}; captured_bytes={captured}; touched_bytes_per_memory_leaf={MEMORY}; resident_accounting=unavailable; admission=unavailable; overshoot=unmeasured",
                    start.elapsed().as_micros()
                );
                // This validates the workload only. No candidate was assigned,
                // so exceeding a requested value is not an enforcement result.
                record.candidate_available = false;
                record.candidate_cleanup = "no candidate";
                record.rescue = "direct-child wait/kill guard; no detached-tree claim";
                record.emit()?;
                if !record.setup_ready || !record.positive_control {
                    return Err(io::Error::other("workload acknowledgement missing"));
                }
            }
        }
    }
    Ok(())
}
