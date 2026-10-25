//! Single-owner bounded service loop. Disconnected leases are drained, never timed out open.
use super::{
    SERVICE,
    filtering::Engine,
    identity, pipe,
    protocol::{self, Request, SIZE},
    raw, wide,
};
use std::{
    os::windows::io::OwnedHandle,
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, AtomicIsize, Ordering},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        System::{JobObjects::TerminateJobObject, Services::*, Threading::WaitForSingleObject},
    },
    core::GUID,
};
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);
fn status(state: u32, error: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_STOP_PENDING {
            10000
        } else {
            0
        },
    };
    unsafe {
        SetServiceStatus(STATUS_HANDLE.load(Ordering::SeqCst), &status);
    }
}
unsafe extern "system" fn control(
    code: u32,
    _event: u32,
    _data: *mut std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> u32 {
    if code == SERVICE_CONTROL_STOP || code == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::SeqCst);
        status(SERVICE_STOP_PENDING, 0);
    }
    0
}
unsafe extern "system" fn main(_count: u32, _args: *mut *mut u16) {
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(wide(SERVICE).as_ptr(), Some(control), null()) };
    if handle == 0 {
        return;
    }
    STATUS_HANDLE.store(handle, Ordering::SeqCst);
    status(SERVICE_START_PENDING, 0);
    let result = run();
    status(SERVICE_STOPPED, result.err().unwrap_or(0));
}
pub(super) fn dispatch() -> Result<(), u32> {
    let mut name = wide(SERVICE);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(())
    }
}
struct Lease {
    target: identity::Target,
    keys: [GUID; 2],
    terminating: bool,
}
struct Connection {
    pipe: OwnedHandle,
    caller: Option<(OwnedHandle, identity::Identity)>,
    lease: Option<Lease>,
    input: [u8; SIZE],
    received: usize,
    output: Option<[u8; SIZE]>,
    sent: usize,
    deadline: Option<Instant>,
    closing: bool,
}
impl Connection {
    fn new(pipe: OwnedHandle) -> Self {
        Self {
            pipe,
            caller: None,
            lease: None,
            input: [0; SIZE],
            received: 0,
            output: None,
            sent: 0,
            deadline: Some(Instant::now() + Duration::from_secs(2)),
            closing: false,
        }
    }
    fn reply(&mut self, error: u32, flags: u32, active: u32, retained: u32) {
        self.output = Some(protocol::response(error, flags, active, retained));
        self.sent = 0;
        self.deadline = Some(Instant::now() + Duration::from_secs(2));
    }
}
fn run() -> Result<(), u32> {
    identity::prepare_server_authentication()?;
    let engine = Engine::open()?;
    engine.count()?;
    let mut listener = pipe::listener(true)?;
    let mut clients: Vec<Connection> = Vec::new();
    status(SERVICE_RUNNING, 0);
    loop {
        let stopping = STOP.load(Ordering::SeqCst);
        if !stopping && clients.len() < 254 && pipe::connected(raw(&listener))? {
            // Retain the connected instance until the next instance exists, keeping namespace ownership.
            let next = pipe::listener(false)?;
            clients.push(Connection::new(std::mem::replace(&mut listener, next)));
        }
        let mut index = 0;
        while index < clients.len() {
            let active = clients.iter().filter(|c| c.lease.is_some()).count() as u32;
            let mut counts = std::collections::HashMap::<Vec<u32>, usize>::new();
            for c in &clients {
                if c.lease.is_some()
                    && let Some((_, owner)) = &c.caller
                {
                    *counts.entry(owner.user.clone()).or_default() += 1;
                }
            }
            let client = &mut clients[index];
            if stopping
                || client.deadline.is_some_and(|d| Instant::now() >= d)
                || client
                    .caller
                    .as_ref()
                    .is_some_and(|(p, _)| unsafe { WaitForSingleObject(raw(p), 0) } != WAIT_TIMEOUT)
            {
                client.closing = true;
            }
            if !client.closing {
                let result = (|| -> Result<(), u32> {
                    if let Some(output) = &client.output {
                        client.sent += pipe::write(raw(&client.pipe), &output[client.sent..])?;
                        if client.sent == SIZE {
                            client.output = None;
                            client.deadline = if client.lease.is_some() {
                                None
                            } else {
                                Some(Instant::now() + Duration::from_secs(2))
                            };
                        }
                        return Ok(());
                    }
                    let n = pipe::read(raw(&client.pipe), &mut client.input[client.received..])?;
                    if n == 0 {
                        return Ok(());
                    }
                    if client.received == 0 {
                        client.deadline = Some(Instant::now() + Duration::from_secs(2));
                    }
                    client.received += n;
                    if client.received < SIZE {
                        return Ok(());
                    }
                    let request = Request::decode(&client.input).ok_or(ERROR_INVALID_DATA)?;
                    client.received = 0;
                    if client.caller.is_none() {
                        client.caller = Some(identity::caller(raw(&client.pipe))?);
                    }
                    let retained = engine.count()?.saturating_sub(active * 2);
                    let operation = (|| -> Result<u32, u32> {
                        match request.operation {
                            protocol::REGISTER => {
                                if client.lease.is_some() {
                                    return Err(ERROR_ALREADY_EXISTS);
                                }
                                if counts
                                    .get(&client.caller.as_ref().unwrap().1.user)
                                    .copied()
                                    .unwrap_or(0)
                                    >= 64
                                {
                                    return Err(ERROR_NOT_ENOUGH_QUOTA);
                                }
                                let (caller, owner) = client.caller.as_ref().unwrap();
                                let mut target =
                                    identity::target(caller, owner, request.process, request.job)?;
                                let keys = engine.deny(&mut target.sid)?;
                                client.lease = Some(Lease {
                                    target,
                                    keys,
                                    terminating: false,
                                });
                                Ok(protocol::ENFORCED)
                            }
                            protocol::FINISH => {
                                let lease = client.lease.as_ref().ok_or(ERROR_INVALID_STATE)?;
                                engine.remove_empty(raw(&lease.target.job), &lease.keys)?;
                                client.lease = None;
                                Ok(0)
                            }
                            protocol::STATUS => Ok(0),
                            _ => Err(ERROR_INVALID_DATA),
                        }
                    })();
                    match operation {
                        Ok(flags) => client.reply(0, flags, active, retained),
                        Err(error) => client.reply(error, 0, active, retained),
                    }
                    Ok(())
                })();
                if result.is_err() {
                    client.closing = true;
                }
            }
            if client.closing {
                let mut remove = true;
                if let Some(lease) = client.lease.as_mut() {
                    if !lease.terminating {
                        unsafe {
                            TerminateJobObject(raw(&lease.target.job), 1);
                        }
                        lease.terminating = true;
                    }
                    // Fresh accounting is required even after ACTIVE_PROCESS_ZERO notifications.
                    match engine.remove_empty(raw(&lease.target.job), &lease.keys) {
                        Ok(()) => {}
                        Err(ERROR_BUSY) => remove = false,
                        Err(_) => {
                            if !identity::job_empty(raw(&lease.target.job)).unwrap_or(false) {
                                remove = false;
                            }
                        }
                    }
                }
                if remove {
                    clients.swap_remove(index);
                    continue;
                }
            }
            index += 1;
        }
        if stopping && clients.is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
