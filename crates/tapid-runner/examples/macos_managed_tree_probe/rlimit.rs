//! Per-process RSS comparison, not a ManagedTree backend.
use super::record::Observation;
use std::{
    fs, io,
    io::Write,
    os::unix::process::ExitStatusExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tapid_test_support::TempProject;

const REQUESTED_RSS: u64 = 1024 * 1024;
const TOUCHED: usize = 2 * 1024 * 1024;

pub fn child(mode: &str) -> io::Result<()> {
    if matches!(
        mode,
        "nproc" | "nproc-off" | "inherited-files" | "inherited-leaf"
    ) {
        return inheritance_child(mode);
    }
    if matches!(mode, "cpu" | "cpu-off" | "fsize" | "fsize-off") {
        return scoped_child(mode);
    }
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

fn inheritance_child(mode: &str) -> io::Result<()> {
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    let process_limit = mode.starts_with("nproc");
    let resource = if process_limit {
        libc::RLIMIT_NPROC
    } else {
        libc::RLIMIT_FSIZE
    };
    let value = if process_limit { 1 } else { 1024 };
    if !matches!(mode, "nproc-off" | "inherited-leaf") {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            println!(
                "scoped_setup=unavailable error={}",
                io::Error::last_os_error()
            );
            return Ok(());
        }
    }
    if mode != "nproc-off" {
        let mut actual = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(resource, &mut actual) } != 0
            || actual.rlim_cur != value
            || actual.rlim_max != value
        {
            return Err(io::Error::other("inherited limit readback mismatch"));
        }
    }
    println!("scoped_ready mode={mode}");
    io::stdout().flush()?;
    if process_limit {
        // Only this disposable caller's fork admission is changed. The counter
        // covers its UID, so this cannot represent a request-owned tree limit.
        let spawned = Command::new(std::env::current_exe()?)
            .args(["rlimit-child", "off"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn();
        match spawned {
            Err(error) if mode == "nproc" && error.raw_os_error() == Some(libc::EAGAIN) => {
                println!("process_admission=denied scope=uid")
            }
            Err(error) => return Err(error),
            Ok(mut child) => {
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if let Some(status) = child.try_wait()? {
                        if !status.success() {
                            return Err(io::Error::other("admission child failed"));
                        }
                        break;
                    }
                    if Instant::now() >= deadline {
                        child.kill()?;
                        child.wait()?;
                        return Err(io::Error::other("admission child timeout"));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                println!("process_admission=allowed scope=uid");
            }
        }
    } else if mode == "inherited-leaf" {
        fs::write("leaf-output", [0x5a; 768])?;
    } else {
        // Two concurrent descendants inherit the same numeric limit but write
        // distinct files. The combined 1536 bytes exceed the per-file 1024.
        let mut children = Vec::new();
        for directory in ["a", "b"] {
            fs::create_dir(directory)?;
            let spawned = Command::new(std::env::current_exe()?)
                .args(["rlimit-child", "inherited-leaf"])
                .env_clear()
                .current_dir(directory)
                .stdin(Stdio::null())
                .spawn();
            match spawned {
                Ok(child) => children.push(child),
                Err(error) => {
                    for child in &mut children {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    return Err(error);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut failure = None;
        for child in &mut children {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            failure = Some(io::Error::other("inherited leaf failed"));
                        }
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Ok(None) => {
                        failure = Some(io::Error::other("inherited leaf deadline"));
                        break;
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
        }
        for child in &mut children {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
            }
            child.wait()?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let total = fs::metadata("a/leaf-output")?.len() + fs::metadata("b/leaf-output")?.len();
        if total != 1536 {
            return Err(io::Error::other("inherited output size mismatch"));
        }
        println!("inherited_files_total={total} per_file_limit=1024 authoritative_tree=none");
    }
    println!("scoped_completed mode={mode}");
    Ok(())
}

fn scoped_child(mode: &str) -> io::Result<()> {
    // The caller is a disposable fresh process. No account-wide limit changes.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    let cpu = mode.starts_with("cpu");
    if !mode.ends_with("-off") {
        let value = if cpu { 1 } else { 1024 };
        let resource = if cpu {
            libc::RLIMIT_CPU
        } else {
            libc::RLIMIT_FSIZE
        };
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            println!(
                "scoped_setup=unavailable error={}",
                io::Error::last_os_error()
            );
            return Ok(());
        }
        let mut actual = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(resource, &mut actual) } != 0
            || actual.rlim_cur != value
            || actual.rlim_max != value
        {
            return Err(io::Error::other("scoped limit readback failed"));
        }
    }
    println!(
        "scoped_ready mode={mode} pid={} session={}",
        std::process::id(),
        unsafe { libc::getsid(0) }
    );
    io::stdout().flush()?;
    if cpu {
        let deadline = Instant::now() + Duration::from_millis(1600);
        while Instant::now() < deadline {
            std::hint::black_box(17u64.wrapping_mul(31));
        }
    } else {
        // This regular-file limit says nothing about combined stdout/stderr.
        let mut file = fs::File::create("bounded-output")?;
        file.write_all(&[0x5a; 4096])?;
    }
    println!("scoped_completed mode={mode}");
    Ok(())
}

pub fn scoped() -> io::Result<()> {
    for mode in [
        "cpu-off",
        "cpu",
        "fsize-off",
        "fsize",
        "nproc-off",
        "nproc",
        "inherited-files",
    ] {
        let root = TempProject::new("scoped-limit")?;
        let log = root.path().join("log");
        let mut child = Command::new(std::env::current_exe()?)
            .args(["rlimit-child", mode])
            .env_clear()
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(fs::File::create(&log)?)
            .stderr(Stdio::inherit())
            .spawn()?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > Duration::from_secs(4) {
                child.kill()?;
                child.wait()?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "scoped limit rescue deadline",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let output = fs::read_to_string(log)?;
        let unavailable = output.contains("scoped_setup=unavailable");
        let completed = output.contains("scoped_completed");
        let positive = mode.ends_with("-off");
        let signaled = matches!(mode, "cpu" | "fsize");
        let expected_signal = if mode == "cpu" {
            libc::SIGXCPU
        } else {
            libc::SIGXFSZ
        };
        if (!unavailable && !output.contains("scoped_ready"))
            || (positive && (!status.success() || !completed))
            || (signaled && !unavailable && status.signal() != Some(expected_signal))
            || (!signaled && !unavailable && (!status.success() || !completed))
        {
            return Err(io::Error::other(format!(
                "scoped control mismatch mode={mode} status={status} output={output}"
            )));
        }
        let mut record =
            Observation::diagnostic(mode, "tree-wide limit via scoped rlimit comparison");
        record.candidate = "per-process-rlimit";
        record.candidate_available = false;
        record.setup_ready = !unavailable;
        record.positive_control = positive && completed;
        record.threshold = if mode.starts_with("cpu") || mode.starts_with("nproc") {
            1
        } else {
            1024
        };
        record.units = if mode.starts_with("cpu") {
            "CPU seconds per process; 1600ms finite wall workload"
        } else if mode.starts_with("nproc") {
            "processes per UID counted for this caller's admission; not a tree limit"
        } else if mode == "inherited-files" {
            "1024 bytes per file inherited by two descendants writing 768 bytes each"
        } else {
            "bytes per regular file; 4096-byte finite write"
        };
        record.observed = format!(
            "status={status}; elapsed_ms={}; signal={:?}; {output}; no aggregate accounting, tree process admission or combined pipe-output enforcement",
            started.elapsed().as_millis(),
            status.signal()
        );
        record.emit()?;
    }
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
    fn unknown_limit_rejects_before_changing_process_state() {
        assert_eq!(
            super::child("cpu-unknown").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    #[test]
    fn memory_control_completes_without_a_candidate_limit() {
        super::child("off").unwrap();
    }
}
