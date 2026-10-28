//! Disposable broader-authority broker; never a production broker or tree boundary.
use super::record::Observation;
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    sync::mpsc,
    time::{Duration, Instant},
};
use tapid_test_support::TempProject;

const REQUESTS: usize = 4;
const IO_TIMEOUT: Duration = Duration::from_millis(250);

fn serve(
    listener: UnixListener,
    capability: [u8; 32],
    marker: &std::path::Path,
) -> io::Result<usize> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut accepted = 0;
    let mut granted = 0;
    while accepted < REQUESTS {
        let (mut stream, _) = match listener.accept() {
            Ok(peer) => peer,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "broker accept deadline",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(error) => return Err(error),
        };
        accepted += 1;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut uid = 0;
        let mut gid = 0;
        // Kernel peer identity is necessary but not sufficient: every fixture
        // client has the same UID. The per-run capability is also required.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut request = [0; 33];
        let complete = match stream.read_exact(&mut request) {
            Ok(()) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                false
            }
            Err(error) => return Err(error),
        };
        let allowed = complete
            && uid == unsafe { libc::geteuid() }
            && request[..32] == capability
            && request[32] == 1;
        if allowed {
            granted += 1;
            // The protocol has one fixed operation and accepts no paths.
            fs::write(marker, b"synthetic-broker-write")?;
        }
        stream.write_all(&[u8::from(allowed)])?;
    }
    Ok(granted)
}

pub fn run() -> io::Result<()> {
    let root = TempProject::new("b")?;
    let outside = TempProject::new("broker-authority")?;
    let marker = outside.path().join("broker-only");
    let socket = root.path().join("s");
    let listener = UnixListener::bind(&socket)?;
    let mut capability = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut capability)?;
    let (done_tx, done_rx) = mpsc::channel();
    let marker_for_server = marker.clone();
    let server = std::thread::spawn(move || {
        let result = serve(listener, capability, &marker_for_server);
        let _ = done_tx.send(result);
    });
    let controls = (|| -> io::Result<()> {
        for case in 0..REQUESTS {
            let mut stream = UnixStream::connect(&socket)?;
            stream.set_read_timeout(Some(Duration::from_secs(1)))?;
            stream.set_write_timeout(Some(IO_TIMEOUT))?;
            let mut request = [0; 33];
            request[..32].copy_from_slice(&capability);
            request[32] = 1;
            match case {
                0 => request[0] ^= 1, // same UID, wrong capability
                1 => request[32] = 2, // authenticated, ungranted operation
                2 => {}               // truncated frame
                _ => {}               // authorized positive control runs last
            }
            stream.write_all(if case == 2 { &request[..3] } else { &request })?;
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut reply = [0; 1];
            stream.read_exact(&mut reply)?;
            if reply[0] != u8::from(case == 3) || (case < 3 && marker.exists()) {
                return Err(io::Error::other("broker authorization control failed"));
            }
        }
        Ok(())
    })();
    // Always join: a failed client must not leave a detached fixture server.
    server
        .join()
        .map_err(|_| io::Error::other("broker fixture panicked"))?;
    controls?;
    let granted = done_rx.recv().map_err(io::Error::other)??;
    if granted != 1 || fs::read(marker)? != b"synthetic-broker-write" {
        return Err(io::Error::other("broker positive operation missing"));
    }
    let mut record =
        Observation::diagnostic("broker", "authenticated broader-authority broker control");
    record.setup_ready = true;
    record.positive_control = true;
    record.threshold = REQUESTS as u64;
    record.units = "fixed 33-byte requests; 250ms per stream; 3s accept deadline";
    record.observed = "kernel peer UID and per-run capability checked; wrong capability, ungranted operation and truncated request denied; authorized fixed write succeeded. Same-process synthetic clients only; sandboxed and detached clients unverified; system brokers and privileged helpers unverified".into();
    record.emit()
}

#[cfg(test)]
mod tests {
    #[test]
    fn broker_requires_caller_capability_and_rejects_ungranted_operations() {
        super::run().unwrap();
    }
}
