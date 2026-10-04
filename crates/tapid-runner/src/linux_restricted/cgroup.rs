use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(0);
const CGROUP_ROOT_ENV: &str = "TAPID_CGROUP_ROOT";
const CGROUP_EMPTY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LimitEvents {
    pub process_limit_hits: u64,
    pub memory_limit_hits: u64,
}

/// A per-execution cgroup below an explicitly delegated, controller-enabled v2 subtree.
pub(super) struct ExecutionCgroup {
    path: PathBuf,
    processes: Option<File>,
    initial_events: LimitEvents,
    process_limit_enabled: bool,
    memory_limit_enabled: bool,
    removed: bool,
}

impl ExecutionCgroup {
    pub(super) fn create(
        process_limit: Option<u32>,
        memory_limit_bytes: Option<u64>,
    ) -> Result<Self, String> {
        let root = std::env::var_os(CGROUP_ROOT_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| format!("{CGROUP_ROOT_ENV} is not configured"))?;
        Self::create_at(&root, process_limit, memory_limit_bytes)
    }

    pub(super) fn probe(
        process_limit: Option<u32>,
        memory_limit_bytes: Option<u64>,
    ) -> Result<(), String> {
        if process_limit.is_none() && memory_limit_bytes.is_none() {
            return Ok(());
        }
        let cgroup = Self::create(process_limit, memory_limit_bytes)?;
        cgroup.probe_process_migration()?;
        cgroup.cleanup()
    }

    fn spawn_process_in_cgroup(&self, program: &str, arguments: &[&str]) -> Result<Child, String> {
        let fd = self.processes_fd();
        let mut command = Command::new(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || write_current_pid_to_cgroup(fd));
        }
        command.spawn().map_err(|error| {
            format!("cannot migrate preflight process into execution cgroup: {error}")
        })
    }

    fn probe_process_migration(&self) -> Result<(), String> {
        self.spawn_process_in_cgroup("/bin/true", &[])?
            .wait()
            .map_err(|error| format!("cannot wait for cgroup migration probe: {error}"))?;
        Ok(())
    }

    fn create_at(
        delegated_root: &Path,
        process_limit: Option<u32>,
        memory_limit_bytes: Option<u64>,
    ) -> Result<Self, String> {
        let delegated_root = validate_delegated_root(
            delegated_root,
            process_limit.is_some(),
            memory_limit_bytes.is_some(),
        )?;
        let path = create_unique_child(&delegated_root)?;
        let initialized = (|| {
            if let Some(limit) = process_limit {
                write_and_check_limit(&path.join("pids.max"), limit)?;
            }
            if let Some(limit) = memory_limit_bytes {
                write_and_check_limit(&path.join("memory.max"), limit)?;
            }
            if !path.join("cgroup.kill").is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "delegated cgroup lacks cgroup.kill",
                ));
            }
            let initial_events =
                read_limit_events(&path, process_limit.is_some(), memory_limit_bytes.is_some())?;
            let processes = OpenOptions::new()
                .write(true)
                .open(path.join("cgroup.procs"))?;
            Ok((processes, initial_events))
        })();
        match initialized {
            Ok((processes, initial_events)) => Ok(Self {
                path,
                processes: Some(processes),
                initial_events,
                process_limit_enabled: process_limit.is_some(),
                memory_limit_enabled: memory_limit_bytes.is_some(),
                removed: false,
            }),
            Err(error) => {
                let _ = remove_empty_cgroup(&path);
                Err(format!("cannot prepare execution cgroup: {error}"))
            }
        }
    }

    pub(super) fn processes_fd(&self) -> RawFd {
        self.processes
            .as_ref()
            .expect("execution cgroup remains open until cleanup")
            .as_raw_fd()
    }

    pub(super) fn process_limit_exceeded(&self) -> Result<bool, String> {
        if !self.process_limit_enabled {
            return Ok(false);
        }
        let events = read_event_counter(&self.path.join("pids.events"), "max")
            .map_err(|error| format!("cannot read pids.events: {error}"))?;
        Ok(events > self.initial_events.process_limit_hits)
    }

    pub(super) fn memory_limit_exceeded(&self) -> Result<bool, String> {
        if !self.memory_limit_enabled {
            return Ok(false);
        }
        let events = read_event_counter(&self.path.join("memory.events"), "max")
            .map_err(|error| format!("cannot read memory.events: {error}"))?;
        Ok(events > self.initial_events.memory_limit_hits)
    }

    pub(super) fn cleanup(mut self) -> Result<(), String> {
        self.cleanup_inner()
            .map_err(|error| format!("cannot clean up execution cgroup: {error}"))
    }

    fn cleanup_inner(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        write_control(&self.path.join("cgroup.kill"), "1")?;
        wait_until_empty(&self.path)?;
        self.processes.take();
        fs::remove_dir(&self.path)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for ExecutionCgroup {
    fn drop(&mut self) {
        let _ = self.cleanup_inner();
    }
}

pub(super) fn write_current_pid_to_cgroup(fd: RawFd) -> io::Result<()> {
    let pid = unsafe { libc::getpid() };
    if pid <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut digits = [0_u8; 24];
    let mut cursor = digits.len();
    cursor -= 1;
    digits[cursor] = b'\n';
    let mut value = pid as u32;
    while value != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let bytes = &digits[cursor..];
    let mut written = 0;
    while written < bytes.len() {
        let result =
            unsafe { libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "cgroup.procs write returned zero",
            ));
        }
        written += result as usize;
    }
    Ok(())
}

fn validate_delegated_root(
    root: &Path,
    need_processes: bool,
    need_memory: bool,
) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err(format!("{CGROUP_ROOT_ENV} must be an absolute path"));
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("cannot resolve {CGROUP_ROOT_ENV}: {error}"))?;
    if !canonical_root.is_dir() {
        return Err(format!("{CGROUP_ROOT_ENV} is not a directory"));
    }
    let mountpoints = cgroup2_mountpoints(
        &fs::read_to_string("/proc/self/mountinfo")
            .map_err(|error| format!("cannot inspect cgroup mounts: {error}"))?,
    )?;
    if !mountpoints.iter().any(|mountpoint| {
        fs::canonicalize(mountpoint)
            .is_ok_and(|mount| canonical_root.starts_with(&mount) && canonical_root != mount)
    }) {
        return Err(format!(
            "{CGROUP_ROOT_ENV} must be a dedicated directory below a cgroup v2 mount"
        ));
    }
    let kind = read_trimmed(&canonical_root.join("cgroup.type"))?;
    if kind != "domain" {
        return Err(format!(
            "delegated cgroup root is not a domain cgroup: {kind}"
        ));
    }
    if !read_trimmed(&canonical_root.join("cgroup.procs"))?.is_empty() {
        return Err("delegated cgroup root must contain no processes".into());
    }
    let available = read_words(&canonical_root.join("cgroup.controllers"))?;
    let enabled = read_words(&canonical_root.join("cgroup.subtree_control"))?;
    for (requested, name) in [(need_processes, "pids"), (need_memory, "memory")] {
        if requested
            && (!available.iter().any(|controller| controller == name)
                || !enabled.iter().any(|controller| controller == name))
        {
            return Err(format!(
                "delegated cgroup root must expose and enable the {name} controller"
            ));
        }
    }
    Ok(canonical_root)
}

fn cgroup2_mountpoints(mountinfo: &str) -> Result<Vec<PathBuf>, String> {
    let mut mountpoints = Vec::new();
    for line in mountinfo.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(separator) = fields.iter().position(|field| *field == "-") else {
            continue;
        };
        if fields.get(separator + 1) != Some(&"cgroup2") {
            continue;
        }
        let Some(mountpoint) = fields.get(4) else {
            return Err("malformed cgroup2 mount entry".into());
        };
        mountpoints.push(PathBuf::from(decode_mount_field(mountpoint)?));
    }
    if mountpoints.is_empty() {
        return Err("no cgroup v2 mount is available".into());
    }
    Ok(mountpoints)
}

fn decode_mount_field(raw: &str) -> Result<std::ffi::OsString, String> {
    let input = raw.as_bytes();
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'\\' {
            decoded.push(input[index]);
            index += 1;
            continue;
        }
        if index + 3 >= input.len()
            || !input[index + 1..index + 4]
                .iter()
                .all(|digit| (b'0'..=b'7').contains(digit))
        {
            return Err("malformed escape in cgroup mount path".into());
        }
        let value = (input[index + 1] - b'0') as u16 * 64
            + (input[index + 2] - b'0') as u16 * 8
            + (input[index + 3] - b'0') as u16;
        if value == 0 || value > u8::MAX as u16 {
            return Err("invalid byte in cgroup mount path".into());
        }
        decoded.push(value as u8);
        index += 4;
    }
    Ok(std::ffi::OsString::from_vec(decoded))
}

fn create_unique_child(root: &Path) -> Result<PathBuf, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
        .as_nanos();
    for _ in 0..32 {
        let sequence = NEXT_CGROUP_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "tapid-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create execution cgroup: {error}")),
        }
    }
    Err("cannot allocate a unique execution cgroup name".into())
}

fn write_and_check_limit(path: &Path, value: impl std::fmt::Display) -> io::Result<()> {
    let value = value.to_string();
    write_control(path, &value)?;
    let actual = read_trimmed(path).map_err(io::Error::other)?;
    if actual != value {
        return Err(io::Error::other(format!(
            "{} read back as {actual:?}, expected {value:?}",
            path.display()
        )));
    }
    Ok(())
}

fn write_control(path: &Path, value: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(value.as_bytes())
}

fn read_trimmed(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn read_words(path: &Path) -> Result<Vec<String>, String> {
    Ok(read_trimmed(path)?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

fn read_limit_events(path: &Path, pids: bool, memory: bool) -> io::Result<LimitEvents> {
    Ok(LimitEvents {
        process_limit_hits: if pids {
            read_event_counter(&path.join("pids.events"), "max")?
        } else {
            0
        },
        memory_limit_hits: if memory {
            read_event_counter(&path.join("memory.events"), "max")?
        } else {
            0
        },
    })
}

fn read_event_counter(path: &Path, key: &str) -> io::Result<u64> {
    let contents = fs::read_to_string(path)?;
    let mut found = None;
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(value) = fields.next() else {
            return Err(io::Error::other(format!(
                "malformed event line in {}",
                path.display()
            )));
        };
        if fields.next().is_some() {
            return Err(io::Error::other(format!(
                "malformed event line in {}",
                path.display()
            )));
        }
        if name == key {
            if found.is_some() {
                return Err(io::Error::other(format!(
                    "duplicate {key} event in {}",
                    path.display()
                )));
            }
            found = Some(value.parse::<u64>().map_err(|error| {
                io::Error::other(format!(
                    "invalid {key} event in {}: {error}",
                    path.display()
                ))
            })?);
        }
    }
    found.ok_or_else(|| io::Error::other(format!("missing {key} event in {}", path.display())))
}

fn wait_until_empty(path: &Path) -> io::Result<()> {
    let started = Instant::now();
    loop {
        let events = fs::read_to_string(path.join("cgroup.events"))?;
        let populated = events.lines().find_map(|line| {
            let (name, value) = line.split_once(' ')?;
            (name == "populated").then_some(value)
        });
        match populated {
            Some("0") => return Ok(()),
            Some("1") => {}
            _ => return Err(io::Error::other("malformed cgroup.events populated state")),
        }
        if started.elapsed() >= CGROUP_EMPTY_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "execution cgroup remained populated after cgroup.kill",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn remove_empty_cgroup(path: &Path) -> io::Result<()> {
    if path.join("cgroup.kill").exists() {
        let _ = write_control(&path.join("cgroup.kill"), "1");
        let _ = wait_until_empty(path);
    }
    fs::remove_dir(path)
}

#[cfg(test)]
pub(super) fn parse_unified_path(contents: &str) -> Option<PathBuf> {
    use std::path::Component;

    let mut entries = contents.lines().filter_map(|line| line.strip_prefix("0::"));
    let raw = entries.next()?;
    if entries.next().is_some()
        || raw.is_empty()
        || !raw.starts_with('/')
        || raw.contains("//")
        || (raw.len() > 1 && raw.ends_with('/'))
    {
        return None;
    }
    let path = Path::new(raw);
    if path.components().any(|component| {
        matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::Prefix(_)
        )
    }) || path.as_os_str().to_str() != Some(raw)
    {
        return None;
    }
    Some(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn delegated_cgroup_process_limit_is_applied_and_child_can_migrate_itself() {
        let Some(root) = std::env::var_os(CGROUP_ROOT_ENV).map(PathBuf::from) else {
            if std::env::var_os("TAPID_REQUIRE_CGROUP_TESTS").is_some() {
                panic!("{CGROUP_ROOT_ENV} must be configured for this test lane");
            }
            eprintln!("skipping: {CGROUP_ROOT_ENV} is not configured");
            return;
        };
        let cgroup = ExecutionCgroup::create_at(&root, Some(8), None).unwrap();
        assert_eq!(read_trimmed(&cgroup.path.join("pids.max")).unwrap(), "8");
        let mut child = cgroup
            .spawn_process_in_cgroup("/bin/sleep", &["30"])
            .unwrap();
        let members = read_trimmed(&cgroup.path.join("cgroup.procs")).unwrap();
        assert!(
            members.lines().any(|pid| pid == child.id().to_string()),
            "spawned child {0} was not found in workload cgroup: {members:?}",
            child.id()
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let path = cgroup.path.clone();
        cgroup.cleanup().unwrap();
        assert!(
            !path.exists(),
            "execution cgroup should be removed after cleanup"
        );
    }

    #[test]
    fn pids_max_blocks_fork_and_records_the_limit_event() {
        let Some(root) = std::env::var_os(CGROUP_ROOT_ENV).map(PathBuf::from) else {
            if std::env::var_os("TAPID_REQUIRE_CGROUP_TESTS").is_some() {
                panic!("{CGROUP_ROOT_ENV} must be configured for this test lane");
            }
            eprintln!("skipping: {CGROUP_ROOT_ENV} is not configured");
            return;
        };
        let cgroup = ExecutionCgroup::create_at(&root, Some(1), None).unwrap();
        let fd = cgroup.processes_fd();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "/bin/sleep 0.1 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || write_current_pid_to_cgroup(fd));
        }

        let output = command.output().unwrap();

        assert!(
            !output.status.success(),
            "a cgroup at pids.max=1 must deny the shell's child process"
        );
        assert!(
            cgroup.process_limit_exceeded().unwrap(),
            "pids.events must record the rejected fork"
        );
        assert!(!cgroup.memory_limit_exceeded().unwrap());
        cgroup.cleanup().unwrap();
    }

    #[test]
    fn memory_max_terminates_the_child_and_records_the_limit_event() {
        let Some(root) = std::env::var_os(CGROUP_ROOT_ENV).map(PathBuf::from) else {
            if std::env::var_os("TAPID_REQUIRE_CGROUP_TESTS").is_some() {
                panic!("{CGROUP_ROOT_ENV} must be configured for this test lane");
            }
            eprintln!("skipping: {CGROUP_ROOT_ENV} is not configured");
            return;
        };
        const MEMORY_LIMIT: u64 = 32 * 1024 * 1024;
        let cgroup = ExecutionCgroup::create_at(&root, None, Some(MEMORY_LIMIT)).unwrap();
        assert_eq!(
            read_trimmed(&cgroup.path.join("memory.max")).unwrap(),
            MEMORY_LIMIT.to_string()
        );
        assert!(!cgroup.process_limit_exceeded().unwrap());
        let fd = cgroup.processes_fd();
        let mut command = Command::new("python3");
        command
            .args([
                "-c",
                "import time; data=bytearray(128*1024*1024); data[::4096]=b'x'*(len(data)//4096); time.sleep(0.1)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || write_current_pid_to_cgroup(fd));
        }

        let _ = command.output().unwrap();

        assert!(
            cgroup.memory_limit_exceeded().unwrap(),
            "memory.events must record the enforced memory.max boundary"
        );
        cgroup.cleanup().unwrap();
    }

    #[test]
    fn cgroup_mount_parser_selects_v2_mount_and_decodes_escapes() {
        let mountinfo = concat!(
            "30 20 0:26 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw\n",
            "31 20 0:27 / /run/cgroup\\040delegated rw - cgroup2 cgroup rw\n",
            "32 20 0:28 / /sys/fs/cgroup-v1 rw - cgroup cgroup rw,cpu\n",
        );
        assert_eq!(
            cgroup2_mountpoints(mountinfo).unwrap(),
            vec![
                PathBuf::from("/sys/fs/cgroup"),
                PathBuf::from("/run/cgroup delegated")
            ]
        );
    }
}
