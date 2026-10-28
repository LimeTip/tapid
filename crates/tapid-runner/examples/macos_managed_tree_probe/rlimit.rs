//! Per-process RSS comparison, not a ManagedTree backend.
use super::record::Observation;
use std::{
    fs, io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tapid_test_support::TempProject;

const REQUESTED_RSS: u64 = 1024 * 1024;
const TOUCHED: usize = 2 * 1024 * 1024;

pub fn child(mode: &str) -> io::Result<()> {
    if !matches!(mode, "off" | "rss") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown rlimit mode",
        ));
    }
    if mode == "rss" {
        let limit = libc::rlimit {
            rlim_cur: REQUESTED_RSS,
            rlim_max: REQUESTED_RSS,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_RSS, &limit) } != 0 {
            let error = io::Error::last_os_error();
            println!(
                "rss_setup=unavailable errno={:?} detail={error}",
                error.raw_os_error()
            );
            return Ok(());
        }
        let mut observed = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_RSS, &mut observed) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if observed.rlim_cur != REQUESTED_RSS || observed.rlim_max != REQUESTED_RSS {
            return Err(io::Error::other("RSS limit readback mismatch"));
        }
    }
    let mut memory = vec![0u8; TOUCHED];
    memory.fill(0x5a);
    std::hint::black_box(&memory);
    std::thread::sleep(Duration::from_millis(200));
    std::hint::black_box(memory);
    println!(
        "mode={mode} requested_rss={REQUESTED_RSS} touched_bytes={TOUCHED} completed=true resident_measurement=unavailable"
    );
    Ok(())
}

pub fn run() -> io::Result<()> {
    for mode in ["off", "rss"] {
        let root = TempProject::new("rlimit-comparison")?;
        let mut outputs = Vec::new();
        // Only the new disposable executable changes its own rlimit. No limit
        // is changed in Tapid, the test runner, or the user's account.
        for index in 0..3 {
            let output = root.path().join(index.to_string());
            let mut process = Command::new(std::env::current_exe()?)
                .args(["rlimit-child", mode])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(fs::File::create(&output)?)
                .stderr(Stdio::inherit())
                .spawn()?;
            let start = Instant::now();
            let status = loop {
                if let Some(status) = process.try_wait()? {
                    break status;
                }
                if start.elapsed() > Duration::from_secs(2) {
                    process.kill()?;
                    process.wait()?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "RSS comparison deadline",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            };
            outputs.push(format!("{status}: {}", fs::read_to_string(output)?));
            if !status.success() {
                return Err(io::Error::other(format!(
                    "RSS comparison setup/workload failed: {status}"
                )));
            }
        }
        let mut record =
            Observation::diagnostic(mode, "tree memory enforcement via RLIMIT_RSS comparison");
        record.candidate = "per-process-rlimit-rss";
        let setup_unavailable = mode == "rss"
            && outputs
                .iter()
                .all(|line| line.contains("rss_setup=unavailable"));
        record.setup_ready = !setup_unavailable;
        record.positive_control =
            setup_unavailable || outputs.iter().all(|line| line.contains("completed=true"));
        record.threshold = REQUESTED_RSS;
        record.units = "bytes configured in each fresh process; sequential controls";
        record.observed = outputs.join("; ");
        record.available =
            "setrlimit/getrlimit readback only; no authoritative tree or resident accounting";
        record.candidate_available = false;
        record.emit()?;
        if !record.positive_control {
            return Err(io::Error::other("RSS completion acknowledgement missing"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn memory_control_completes_without_a_candidate_limit() {
        super::child("off").unwrap();
    }
}
