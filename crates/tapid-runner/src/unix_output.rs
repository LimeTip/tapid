//! Bounded live output forwarding through owned sink processes.

use super::{ExecutionError, ExecutionErrorCategory};
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const LIVE_SINK_BACKPRESSURE_GRACE: Duration = Duration::from_millis(250);
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn set_nonblocking<T: std::os::fd::AsRawFd>(value: &T) -> Result<(), ExecutionError> {
    let fd = value.as_raw_fd();
    // SAFETY: fd is owned and live for both fcntl calls; no pointer arguments are involved.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::Internal,
            format!(
                "cannot make output pipe nonblocking: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

pub(super) fn write_live_output(
    sink: &mut ChildStdin,
    bytes: &[u8],
    stop: &AtomicBool,
    backpressure: &AtomicBool,
) -> Result<(), ExecutionError> {
    let mut written = 0;
    let deadline = Instant::now() + LIVE_SINK_BACKPRESSURE_GRACE;
    while written < bytes.len() {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            backpressure.store(true, Ordering::Release);
            return Err(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "live output sink remained backpressured past the bounded grace period",
            ));
        }
        match sink.write(&bytes[written..]) {
            Ok(0) => {
                backpressure.store(true, Ordering::Release);
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "live output sink closed before accepting child output",
                ));
            }
            Ok(count) => {
                written += count;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                backpressure.store(true, Ordering::Release);
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    format!("cannot stream child output: {error}"),
                ));
            }
        }
    }
    Ok(())
}

pub(super) struct LiveSinks {
    children: Vec<Child>,
}

impl LiveSinks {
    pub(super) fn spawn<const N: usize>(
        destinations: [BorrowedFd<'_>; N],
        sanitize: fn() -> std::io::Result<()>,
    ) -> Result<(Self, [ChildStdin; N]), ExecutionError> {
        let mut sinks = Self {
            children: Vec::with_capacity(N),
        };
        let mut inputs = Vec::with_capacity(N);
        for destination in destinations {
            let (child, input) = spawn_live_sink(destination, sanitize)?;
            sinks.children.push(child);
            inputs.push(input);
        }
        let inputs = inputs.try_into().map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "live output sink count does not match requested destinations",
            )
        })?;
        Ok((sinks, inputs))
    }

    pub(super) fn finish(&mut self) -> Result<(), ExecutionError> {
        let deadline = Instant::now() + LIVE_SINK_BACKPRESSURE_GRACE;
        loop {
            let mut complete = true;
            for child in &mut self.children {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => {}
                    Ok(Some(_)) => {
                        return Err(ExecutionError::new(
                            ExecutionErrorCategory::Internal,
                            "live output sink failed",
                        ));
                    }
                    Ok(None) => complete = false,
                    Err(error) => {
                        return Err(ExecutionError::new(
                            ExecutionErrorCategory::Internal,
                            format!("cannot supervise live output sink: {error}"),
                        ));
                    }
                }
            }
            if complete {
                self.children.clear();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "live output sink remained backpressured past the bounded grace period",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for LiveSinks {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn spawn_live_sink(
    destination: BorrowedFd<'_>,
    sanitize: fn() -> std::io::Result<()>,
) -> Result<(Child, ChildStdin), ExecutionError> {
    let output = destination.try_clone_to_owned().map_err(|error| {
        ExecutionError::new(
            ExecutionErrorCategory::Internal,
            format!("cannot duplicate live output sink: {error}"),
        )
    })?;
    let mut command = Command::new("/bin/cat");
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output))
        .stderr(Stdio::null());
    // SAFETY: std has installed the relay's assigned stdio before this closure runs.
    // Mark every remaining descriptor close-on-exec, including Rust's spawn-error channel;
    // that channel must survive until exec and is closed atomically by a successful exec.
    unsafe {
        command.pre_exec(sanitize);
    }
    let mut child = command.spawn().map_err(|error| {
        ExecutionError::new(
            ExecutionErrorCategory::Internal,
            format!("cannot start bounded live output sink: {error}"),
        )
    })?;
    let setup = (|| {
        let input = child.stdin.take().ok_or_else(|| {
            ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "live output sink input is unavailable",
            )
        })?;
        set_nonblocking(&input)?;
        Ok(input)
    })();
    match setup {
        Ok(input) => Ok((child, input)),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}
