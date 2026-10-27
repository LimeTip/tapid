use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Keeps the positive-control endpoint alive until its execution attempt ends.
pub struct TcpHost {
    address: SocketAddr,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), String>>>,
}

impl TcpHost {
    pub fn start(address: &str) -> Self {
        let listener = TcpListener::bind((address, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let host_cancel = cancel.clone();
        let worker = thread::spawn(move || {
            while !host_cancel.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .map_err(|e| e.to_string())?;
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .map_err(|e| e.to_string())?;
                        let mut payload = [0; 4];
                        stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
                        if &payload != b"NODE" {
                            return Err("wrong client payload".into());
                        }
                        stream.write_all(b"HOST").map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err("control cancelled".into())
        });
        Self {
            address,
            cancel,
            worker: Some(worker),
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Release);
        self.worker
            .take()
            .expect("host joined once")
            .join()
            .unwrap_or_else(|_| Err("TCP host worker panicked".into()))
    }
}

impl Drop for TcpHost {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.finish();
        }
    }
}
