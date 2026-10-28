//! Paired synthetic authority controls; unavailable setup is never a denial.
use super::record::Observation;
use std::{
    fs, io,
    net::{TcpListener, TcpStream, UdpSocket},
    os::unix::{
        fs::symlink,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tapid_runner::{
    AssuranceLevel, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest, FilesystemPolicy,
    SandboxMode, SandboxPolicy, Termination, execute,
};
use tapid_test_support::{TempHome, TempProject};

fn expected<T>(result: io::Result<T>, denied: bool, label: &str) -> io::Result<()> {
    match result {
        Ok(_) if !denied => Ok(()),
        Err(error) if denied && error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Ok(_) => Err(io::Error::other(format!("unexpected authority: {label}"))),
        Err(error) => Err(io::Error::other(format!(
            "fixture or unexpected denial for {label}: {error}"
        ))),
    }
}

pub fn payload(
    expectation: &str,
    outside: &str,
    tcp: &str,
    unix: &str,
    depth: &str,
) -> io::Result<()> {
    if !matches!(expectation, "positive" | "denied") || !matches!(depth, "root" | "leaf") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid authority control",
        ));
    }
    let denied = expectation == "denied";
    let generated = format!("generated-{depth}");
    fs::write(&generated, b"generated")?;
    if fs::read(&generated)? != b"generated" {
        return Err(io::Error::other("generated-file positive control"));
    }
    expected(fs::read(outside), denied, "outside read")?;
    expected(
        fs::OpenOptions::new().write(true).open(outside),
        denied,
        "outside write",
    )?;
    expected(fs::read("escape-link"), denied, "symlink outside read")?;
    expected(
        fs::OpenOptions::new().write(true).open("escape-link"),
        denied,
        "symlink outside write",
    )?;
    // Include parent traversal without relying on fixed temp-directory layouts.
    let traversal = Path::new("traversal-link")
        .join("..")
        .join("synthetic-only");
    expected(fs::read(traversal), denied, "parent traversal read")?;
    let address = tcp
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid local address"))?;
    expected(
        TcpStream::connect_timeout(&address, Duration::from_millis(200)),
        denied,
        "local TCP connect",
    )?;
    expected(UdpSocket::bind("127.0.0.1:0"), denied, "local UDP bind")?;
    expected(UnixStream::connect(unix), denied, "local Unix connect")?;
    if std::env::var("TAPID_PROBE_ALLOWED").as_deref() != Ok("synthetic-visible")
        || std::env::var_os("TAPID_PROBE_HIDDEN").is_some()
    {
        return Err(io::Error::other(
            "synthetic controlled environment mismatch",
        ));
    }
    println!(
        "authority depth={depth} paired_controls=completed expectation={expectation} inherited_descriptors=unverified system_brokers=unverified privileged_helpers=unverified"
    );
    if depth == "root" {
        let status = Command::new(std::env::current_exe()?)
            .args(["authority-payload", expectation, outside, tcp, unix, "leaf"])
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other("descendant authority fixture failed"));
        }
    }
    Ok(())
}

pub fn run() -> io::Result<()> {
    // Darwin's sockaddr_un has a small pathname ceiling. A short fixture label
    // preserves the shared temporary-root contract without fixed /tmp paths.
    let root = TempProject::new("a")?;
    let outside = TempProject::new("authority-outside")?;
    let home = TempHome::new("authority-research")?;
    let project = fs::canonicalize(root.path())?;
    let outside = fs::canonicalize(outside.path())?;
    let secret = outside.join("synthetic-only");
    fs::write(&secret, b"synthetic-secret")?;
    fs::create_dir(outside.join("nested"))?;
    symlink(&secret, project.join("escape-link"))?;
    symlink(outside.join("nested"), project.join("traversal-link"))?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let tcp = listener.local_addr()?.to_string();
    // Keep Unix paths short on Darwin, independently of checkout length.
    let unix_path = root.path().join("s");
    let _unix = UnixListener::bind(&unix_path)?;
    let executable = std::env::current_exe()?;
    let secret = secret
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF8 fixture"))?;
    let unix_path = unix_path
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF8 fixture"))?;
    let mut positive = Command::new(&executable)
        .args([
            "authority-payload",
            "positive",
            secret,
            &tcp,
            unix_path,
            "root",
        ])
        .env_clear()
        .env("TAPID_PROBE_ALLOWED", "synthetic-visible")
        .current_dir(&project)
        .stdin(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    let positive_status = loop {
        if let Some(status) = positive.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(3) {
            positive.kill()?;
            positive.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "authority positive deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut record =
        Observation::diagnostic("authority", "Restricted filesystem/network propagation");
    record.candidate = "seatbelt-public-execute";
    record.threshold = 0;
    record.units = "no resource threshold requested";
    record.positive_control = positive_status.success();
    if !record.positive_control {
        record.observed = format!("positive fixture failed: {positive_status}");
        record.emit()?;
        return Err(io::Error::other("authority positive fixture failed"));
    }
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).map_err(io::Error::other)?,
        false,
        vec!["HOME".into(), "TAPID_PROBE_ALLOWED".into()],
        true,
        ExecutionLimits::default(),
    )
    .map_err(io::Error::other)?;
    let request = ExecutionRequest::builder(executable)
        .args([
            "authority-payload",
            "denied",
            secret,
            &tcp,
            unix_path,
            "root",
        ])
        .project_root(&project)
        .policy(policy)
        .env("HOME", home.path())
        .env("TAPID_PROBE_ALLOWED", "synthetic-visible")
        .executable_search_path("/usr/bin")
        .build()
        .map_err(io::Error::other)?;
    let outcome = execute(&request);
    record.candidate_available = !matches!(&outcome, Err(error) if error.category() == ExecutionErrorCategory::UnsupportedContainment);
    record.setup_ready =
        matches!(&outcome, Ok(result) if result.termination() == &Termination::Exited(0));
    record.observed = format!("positive={positive_status}; candidate={outcome:?}");
    record.emit()?;
    if !record.setup_ready {
        return Err(io::Error::other(
            "candidate authority controls unavailable or failed",
        ));
    }
    Ok(())
}
