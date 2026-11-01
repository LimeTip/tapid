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
        Ok(_) if !denied => {
            println!("authority_operation={label:?} observed=allowed expected=allowed");
            Ok(())
        }
        Err(error) if denied && error.kind() == io::ErrorKind::PermissionDenied => {
            println!("authority_operation={label:?} observed=permission-denied expected=denied");
            Ok(())
        }
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
    if !matches!(expectation, "positive" | "denied")
        || !matches!(depth, "root" | "leaf" | "detached")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid authority control",
        ));
    }
    if depth == "detached" && unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    println!(
        "authority_payload_begin depth={depth} pid={} session={}",
        std::process::id(),
        unsafe { libc::getsid(0) }
    );
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
        for descendant in ["leaf", "detached"] {
            let status = Command::new(std::env::current_exe()?)
                .args([
                    "authority-payload",
                    expectation,
                    outside,
                    tcp,
                    unix,
                    descendant,
                ])
                .stdin(Stdio::null())
                .status()?;
            if !status.success() {
                return Err(io::Error::other("descendant authority fixture failed"));
            }
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

pub fn descriptor_payload(
    fd: &str,
    device: &str,
    inode: &str,
    expectation: &str,
) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid descriptor control");
    let fd: i32 = fd.parse().ok().filter(|fd| *fd > 2).ok_or_else(invalid)?;
    let device: u64 = device.parse().map_err(|_| invalid())?;
    let inode: u64 = inode.parse().map_err(|_| invalid())?;
    if !matches!(expectation, "positive" | "denied") {
        return Err(invalid());
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let same = if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == 0 {
        let stat = unsafe { stat.assume_init() };
        stat.st_dev as u64 == device && stat.st_ino == inode
    } else if io::Error::last_os_error().raw_os_error() == Some(libc::EBADF) {
        false
    } else {
        return Err(io::Error::last_os_error());
    };
    if same != (expectation == "positive") {
        return Err(io::Error::other("inherited descriptor authority mismatch"));
    }
    if same {
        let mut bytes = [0; 16];
        if unsafe { libc::pread(fd, bytes.as_mut_ptr().cast(), bytes.len(), 0) } != 16
            || &bytes != b"synthetic-secret"
        {
            return Err(io::Error::other("descriptor positive read failed"));
        }
    }
    println!("descriptor_control expectation={expectation} synthetic_object_inherited={same}");
    Ok(())
}

pub fn descriptors() -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    let root = TempProject::new("descriptor-root")?;
    let outside = TempProject::new("descriptor-outside")?;
    let secret = outside.path().join("synthetic-only");
    fs::write(&secret, b"synthetic-secret")?;
    let file = fs::File::open(secret)?;
    let stat = file.metadata()?;
    let fd = file.as_raw_fd();
    // This command runs in its own disposable controller. File ownership closes
    // the intentionally inheritable descriptor on every return path.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let identity = [
        fd.to_string(),
        stat.dev().to_string(),
        stat.ino().to_string(),
    ];
    let executable = std::env::current_exe()?;
    let args = [
        "descriptor-payload",
        &identity[0],
        &identity[1],
        &identity[2],
    ];
    let status = Command::new(&executable)
        .args(args)
        .arg("positive")
        .env_clear()
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other("descriptor positive fixture failed"));
    }
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::Restricted,
        FilesystemPolicy::new(vec![".".into()], vec![".".into()]).map_err(io::Error::other)?,
        false,
        vec![],
        false,
        ExecutionLimits::default(),
    )
    .map_err(io::Error::other)?;
    let request = ExecutionRequest::builder(executable)
        .args(args)
        .arg("denied")
        .project_root(root.path())
        .policy(policy)
        .executable_search_path("/usr/bin")
        .build()
        .map_err(io::Error::other)?;
    let result = execute(&request);
    let mut record = Observation::diagnostic(
        "inherited-descriptor",
        "outside synthetic descriptor cannot propagate",
    );
    record.candidate = "seatbelt-public-execute";
    record.positive_control = true;
    record.candidate_available = !matches!(&result, Err(error) if error.category() == ExecutionErrorCategory::UnsupportedContainment);
    record.setup_ready =
        matches!(&result, Ok(result) if result.termination() == &Termination::Exited(0));
    record.observed = format!(
        "positive={status}; candidate={result:?}; identity comparison uses fstat, not pathname; no lifecycle claim"
    );
    record.emit()?;
    if !record.setup_ready {
        return Err(io::Error::other(
            "descriptor negative fixture unavailable or failed",
        ));
    }
    Ok(())
}
