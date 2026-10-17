//! Per-execution socket authority. A single polling owner bounds every client and I/O operation.
use super::{native::OwnedHandle, network_protocol as wire};
use crate::execution::windows_job::token_information;
use crate::execution::{ExecutionError, ExecutionErrorCategory};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_NO_DATA, GetLastError,
    PSID, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::*;
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, IsValidSid,
    TOKEN_APPCONTAINER_INFORMATION, TOKEN_GROUPS, TOKEN_QUERY, TokenAppContainerSid,
    TokenCapabilities, TokenIsAppContainer,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Pipes::{
    CreatePipe, PIPE_NOWAIT, PeekNamedPipe, SetNamedPipeHandleState,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, OpenProcessToken, PROCESS_DUP_HANDLE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

#[path = "windows_lookup.rs"]
mod lookup;

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(2);
const PROCESS_RIGHTS: u32 =
    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;

pub(in crate::execution) struct Channel {
    pub request: isize,
    pub response: isize,
}
pub(in crate::execution) struct Broker {
    commands: mpsc::SyncSender<Attach>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
struct Attach {
    process: OwnedHandle,
    reply: mpsc::SyncSender<Result<Channel, u32>>,
}
struct Authority {
    job: OwnedHandle,
    sid: Vec<u32>,
}
struct Winsock;
impl Winsock {
    fn start() -> Result<Self, u32> {
        let mut data = unsafe { zeroed() };
        // SAFETY: data is the native writable WSADATA structure.
        let error = unsafe { WSAStartup(0x202, &mut data) };
        if error == 0 {
            Ok(Self)
        } else {
            Err(error as u32)
        }
    }
}
impl Drop for Winsock {
    fn drop(&mut self) {
        unsafe {
            WSACleanup();
        }
    }
}
struct Socket(usize);
impl Drop for Socket {
    fn drop(&mut self) {
        unsafe {
            closesocket(self.0);
        }
    }
}
struct Pending {
    _socket: Socket,
    since: Instant,
}
struct Client {
    process: OwnedHandle,
    request: OwnedHandle,
    response: OwnedHandle,
    frame: [u8; wire::REQUEST_SIZE],
    filled: usize,
    id: u32,
    pending: Option<Pending>,
    lookup: Option<lookup::Lookup>,
    output: Option<([u8; wire::RESPONSE_SIZE], usize, Instant)>,
}
fn failure(error: u32) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!("Windows socket broker failed: {error}"),
    )
}
fn duplicate(
    source: isize,
    handle: isize,
    target: isize,
    rights: u32,
    options: u32,
) -> Result<OwnedHandle, u32> {
    let mut result = 0;
    // SAFETY: DuplicateHandle validates the supplied object and access; result is uniquely owned.
    if unsafe { DuplicateHandle(source, handle, target, &mut result, rights, 0, options) } == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(OwnedHandle(result))
    }
}
impl Authority {
    fn new(job: isize, sid: PSID) -> Result<Self, u32> {
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(WSAEACCES as u32);
        }
        let length = unsafe { GetLengthSid(sid) };
        let mut copied = vec![0u32; (length as usize).div_ceil(4)];
        if unsafe { CopySid(length, copied.as_mut_ptr().cast(), sid) } == 0 {
            return Err(unsafe { GetLastError() });
        }
        let job = duplicate(
            unsafe { GetCurrentProcess() },
            job,
            unsafe { GetCurrentProcess() },
            0,
            DUPLICATE_SAME_ACCESS,
        )?;
        Ok(Self { job, sid: copied })
    }
    fn validate(&self, process: isize) -> Result<u32, u32> {
        // Derive identity exclusively from the held process object, never a client PID.
        let pid = unsafe { GetProcessId(process) };
        if pid == 0 || unsafe { WaitForSingleObject(process, 0) } != WAIT_TIMEOUT {
            return Err(WSAEACCES as u32);
        }
        let mut in_job = 0;
        if unsafe { IsProcessInJob(process, self.job.0, &mut in_job) } == 0 || in_job == 0 {
            return Err(WSAEACCES as u32);
        }
        let mut token = 0;
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(WSAEACCES as u32);
        }
        let token = OwnedHandle(token);
        let mut container = 0u32;
        let mut length = 0;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenIsAppContainer,
                (&mut container as *mut u32).cast(),
                4,
                &mut length,
            )
        } == 0
            || container != 1
        {
            return Err(WSAEACCES as u32);
        }
        let information = token_information(token.0, TokenAppContainerSid)?;
        let container = unsafe {
            &*information
                .as_ptr()
                .cast::<TOKEN_APPCONTAINER_INFORMATION>()
        };
        if container.TokenAppContainer.is_null()
            || unsafe { EqualSid(container.TokenAppContainer, self.sid.as_ptr() as PSID) } == 0
        {
            return Err(WSAEACCES as u32);
        }
        let groups = token_information(token.0, TokenCapabilities)?;
        if unsafe { (*groups.as_ptr().cast::<TOKEN_GROUPS>()).GroupCount } != 0 {
            return Err(WSAEACCES as u32);
        }
        Ok(pid)
    }
}
impl Broker {
    pub(in crate::execution) fn new(job: isize, sid: PSID) -> Result<Self, ExecutionError> {
        let authority = Authority::new(job, sid).map_err(failure)?;
        let winsock = Winsock::start().map_err(failure)?;
        let (commands, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("tapid-socket-broker".into())
            .spawn(move || {
                let _winsock = winsock;
                service(authority, receiver, &thread_stop);
            })
            .map_err(|error| failure(error.raw_os_error().unwrap_or(WSAENOBUFS) as u32))?;
        Ok(Self {
            commands,
            stop,
            worker: Some(worker),
        })
    }
    pub(in crate::execution) fn attach(&self, process: isize) -> Result<Channel, ExecutionError> {
        let process = duplicate(
            unsafe { GetCurrentProcess() },
            process,
            unsafe { GetCurrentProcess() },
            PROCESS_RIGHTS,
            0,
        )
        .map_err(failure)?;
        let (reply, receiver) = mpsc::sync_channel(1);
        self.commands
            .send(Attach { process, reply })
            .map_err(|_| failure(WSAENOBUFS as u32))?;
        receiver
            .recv()
            .map_err(|_| failure(WSAENOBUFS as u32))?
            .map_err(failure)
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn pipe() -> Result<(OwnedHandle, OwnedHandle), u32> {
    let (mut read, mut write) = (0, 0);
    if unsafe { CreatePipe(&mut read, &mut write, null(), 4096) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    Ok((OwnedHandle(read), OwnedHandle(write)))
}
fn nonblocking(handle: isize) -> Result<(), u32> {
    if unsafe { SetNamedPipeHandleState(handle, &PIPE_NOWAIT, null(), null()) } == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(())
    }
}
fn close_remote(process: isize, handle: isize) {
    // Closing a source-table entry is used only during private setup failure, before publishing it.
    if let Ok(local) = duplicate(
        process,
        handle,
        unsafe { GetCurrentProcess() },
        0,
        DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
    ) {
        drop(local);
    }
}
fn attach(
    authority: &Authority,
    process: OwnedHandle,
    count: usize,
) -> Result<(Client, Channel), u32> {
    if count >= wire::MAX_CLIENTS {
        return Err(WSAENOBUFS as u32);
    }
    authority.validate(process.0)?;
    let (request, child_write) = pipe()?;
    let (child_read, response) = pipe()?;
    // Both host ends are nonblocking: a malicious writer/reader cannot stall shutdown.
    nonblocking(request.0)?;
    nonblocking(response.0)?;
    let remote_write = duplicate(
        unsafe { GetCurrentProcess() },
        child_write.0,
        process.0,
        0,
        DUPLICATE_SAME_ACCESS,
    )?;
    // A recipient-table handle must never be dropped as a host-table OwnedHandle.
    let raw_write = remote_write.0;
    std::mem::forget(remote_write);
    let remote_write = raw_write;
    let remote_read = match duplicate(
        unsafe { GetCurrentProcess() },
        child_read.0,
        process.0,
        0,
        DUPLICATE_SAME_ACCESS,
    ) {
        Ok(handle) => {
            let raw = handle.0;
            std::mem::forget(handle);
            raw
        }
        Err(error) => {
            close_remote(process.0, remote_write);
            return Err(error);
        }
    };
    Ok((
        Client {
            process,
            request,
            response,
            frame: [0; wire::REQUEST_SIZE],
            filled: 0,
            id: 1,
            pending: None,
            lookup: None,
            output: None,
        },
        Channel {
            request: remote_write,
            response: remote_read,
        },
    ))
}
fn service(authority: Authority, commands: mpsc::Receiver<Attach>, stop: &AtomicBool) {
    let mut clients = Vec::<Client>::new();
    while !stop.load(Ordering::Acquire) {
        if let Ok(command) = commands.try_recv() {
            let result =
                attach(&authority, command.process, clients.len()).map(|(client, channel)| {
                    clients.push(client);
                    channel
                });
            if let Err(mpsc::SendError(Ok(channel))) = command.reply.send(result)
                && let Some(client) = clients.pop()
            {
                close_remote(client.process.0, channel.request);
                close_remote(client.process.0, channel.response);
            }
        }
        let mut index = 0;
        while index < clients.len() && !stop.load(Ordering::Acquire) {
            let count = clients.len();
            match poll_client(&authority, &mut clients[index], count) {
                Ok(child) => {
                    if let Some(child) = child {
                        clients.push(child);
                    }
                    index += 1;
                }
                Err(()) => {
                    clients.swap_remove(index);
                }
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
fn poll_client(
    authority: &Authority,
    client: &mut Client,
    count: usize,
) -> Result<Option<Client>, ()> {
    if unsafe { WaitForSingleObject(client.process.0, 0) } != WAIT_TIMEOUT {
        return Err(());
    }
    if client
        .pending
        .as_ref()
        .is_some_and(|pending| pending.since.elapsed() >= TRANSFER_TIMEOUT)
    {
        return Err(());
    }
    if let Some(lookup) = &mut client.lookup {
        let Some(response) = lookup.poll(client.id) else {
            return Ok(None);
        };
        client.lookup = None;
        client.output = Some((response, 0, Instant::now()));
        advance(client)?;
    }
    if let Some((bytes, written, since)) = &mut client.output {
        if since.elapsed() >= TRANSFER_TIMEOUT {
            return Err(());
        }
        let mut amount = 0;
        let success = unsafe {
            WriteFile(
                client.response.0,
                bytes[*written..].as_ptr(),
                (bytes.len() - *written) as u32,
                &mut amount,
                null_mut(),
            )
        };
        if success == 0 && unsafe { GetLastError() } != ERROR_NO_DATA {
            return Err(());
        }
        *written += amount as usize;
        if *written != bytes.len() {
            return Ok(None);
        }
        client.output = None;
    }
    let mut available = 0;
    if unsafe {
        PeekNamedPipe(
            client.request.0,
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        )
    } == 0
    {
        return Err(());
    }
    if available == 0 {
        return Ok(None);
    }
    let length = (wire::REQUEST_SIZE - client.filled).min(available as usize);
    let mut amount = 0;
    if unsafe {
        ReadFile(
            client.request.0,
            client.frame[client.filled..].as_mut_ptr(),
            length as u32,
            &mut amount,
            null_mut(),
        )
    } == 0
    {
        return Err(());
    }
    client.filled += amount as usize;
    if client.filled != wire::REQUEST_SIZE {
        return Ok(None);
    }
    client.filled = 0;
    let request = wire::Request::decode(&client.frame).ok_or(())?;
    if request.id != client.id || (client.pending.is_some() != (request.operation == wire::ACK)) {
        return Err(());
    }
    let mut response = wire::response(request.id, 0, 0, 0);
    let mut child = None;
    match request.operation {
        wire::SOCKET => {
            let result = create_socket(authority, client, &request, &mut response);
            if let Err(error) = result {
                response = wire::response(request.id, error, 0, 0);
                advance(client)?;
            }
        }
        wire::ACK => {
            client.pending = None;
            advance(client)?;
        }
        wire::LOOKUP => {
            authority.validate(client.process.0).map_err(|_| ())?;
            let query = super::lookup_protocol::Query::decode(&client.frame, &request).ok_or(())?;
            match lookup::Lookup::start(query) {
                Ok(lookup) => {
                    client.lookup = Some(lookup);
                    return Ok(None);
                }
                Err(error) => {
                    response = wire::response(request.id, error, 0, 0);
                    advance(client)?;
                }
            }
        }
        wire::REGISTER => {
            let result = duplicate(
                client.process.0,
                request.child as isize,
                unsafe { GetCurrentProcess() },
                PROCESS_RIGHTS,
                0,
            )
            .and_then(|process| attach(authority, process, count));
            match result {
                Ok((registered, channel)) => {
                    response = wire::response(request.id, 0, channel.request, channel.response);
                    child = Some(registered);
                }
                Err(error) => response = wire::response(request.id, error, 0, 0),
            }
            advance(client)?;
        }
        _ => return Err(()),
    }
    client.output = Some((response, 0, Instant::now()));
    Ok(child)
}
fn advance(client: &mut Client) -> Result<(), ()> {
    client.id = client.id.checked_add(1).ok_or(())?;
    Ok(())
}
fn create_socket(
    authority: &Authority,
    client: &mut Client,
    request: &wire::Request,
    response: &mut [u8; wire::RESPONSE_SIZE],
) -> Result<(), u32> {
    if !request.supported_socket() {
        return Err(WSAEINVAL as u32);
    }
    let pid = authority.validate(client.process.0)?;
    // A null provider and zero group prevent arbitrary provider/protocol-chain selection.
    let socket = unsafe {
        WSASocketW(
            request.family,
            request.kind,
            request.protocol,
            null(),
            0,
            request.flags | WSA_FLAG_NO_HANDLE_INHERIT,
        )
    };
    if socket == INVALID_SOCKET {
        return Err(unsafe { WSAGetLastError() } as u32);
    }
    let socket = Socket(socket);
    let mut protocol: WSAPROTOCOL_INFOW = unsafe { zeroed() };
    // Recheck liveness against the same held object immediately before transfer.
    if unsafe { WaitForSingleObject(client.process.0, 0) } != WAIT_TIMEOUT {
        return Err(WSAEACCES as u32);
    }
    if unsafe { WSADuplicateSocketW(socket.0, pid, &mut protocol) } != 0 {
        return Err(unsafe { WSAGetLastError() } as u32);
    }
    const {
        assert!(size_of::<WSAPROTOCOL_INFOW>() == wire::PROTOCOL_SIZE);
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&protocol as *const WSAPROTOCOL_INFOW).cast::<u8>(),
            wire::PROTOCOL_SIZE,
        )
    };
    response[32..660].copy_from_slice(bytes);
    client.pending = Some(Pending {
        _socket: socket,
        since: Instant::now(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecutionLimits;
    use crate::execution::windows_job::{WindowsAppContainer, WindowsJob, WindowsSuspendedChild};
    use std::os::windows::ffi::OsStrExt;

    fn suspended(container: &WindowsAppContainer, job: &WindowsJob) -> WindowsSuspendedChild {
        let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let program = root.join("System32/cmd.exe");
        let program = program.as_os_str().encode_wide().collect::<Vec<_>>();
        let args = ["/D", "/C", "exit 0"].map(|arg| arg.encode_utf16().collect());
        let command =
            crate::execution::serialize_windows_command_line_units(&program, &args, true).unwrap();
        let mut application = program;
        application.push(0);
        let working = root
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let environment =
            crate::execution::windows_environment_block_units(&std::collections::BTreeMap::from([
                (
                    std::ffi::OsString::from("SystemRoot"),
                    root.into_os_string(),
                ),
            ]))
            .unwrap();
        let child = WindowsSuspendedChild::create(
            container,
            &application,
            &command,
            &environment,
            &working,
        )
        .unwrap();
        job.assign_suspended_process(child.process_handle())
            .unwrap();
        child
    }
    fn job() -> WindowsJob {
        WindowsJob::new(&ExecutionLimits::default(), false).unwrap()
    }
    fn host_client() -> (Client, OwnedHandle, OwnedHandle) {
        let process = duplicate(
            unsafe { GetCurrentProcess() },
            unsafe { GetCurrentProcess() },
            unsafe { GetCurrentProcess() },
            PROCESS_RIGHTS,
            0,
        )
        .unwrap();
        let (request, writer) = pipe().unwrap();
        let (reader, response) = pipe().unwrap();
        nonblocking(request.0).unwrap();
        nonblocking(response.0).unwrap();
        (
            Client {
                process,
                request,
                response,
                frame: [0; wire::REQUEST_SIZE],
                filled: 0,
                id: 1,
                pending: None,
                lookup: None,
                output: None,
            },
            writer,
            reader,
        )
    }
    fn write(handle: isize, bytes: &[u8]) -> u32 {
        let mut written = 0;
        assert_ne!(
            unsafe {
                WriteFile(
                    handle,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    null_mut(),
                )
            },
            0
        );
        written
    }
    fn invalid_authority() -> Authority {
        // Valid S-1-15-2-1 SID; direct protocol tests never request transfer authority.
        let sid = [0x0000_0201u32, 0x0f00_0000, 2, 1];
        let job = job();
        Authority::new(job.handle(), sid.as_ptr() as PSID).unwrap()
    }
    #[test]
    fn recipient_requires_live_process_exact_job_and_appcontainer() {
        let mut container = WindowsAppContainer::create().unwrap();
        let mut other = WindowsAppContainer::create().unwrap();
        let job = job();
        let other_job = super::tests::job();
        let child = suspended(&container, &job);
        let authority = Authority::new(job.handle(), container.sid()).unwrap();
        assert_eq!(
            authority.validate(child.process_handle()).unwrap(),
            unsafe { GetProcessId(child.process_handle()) }
        );
        assert!(
            Authority::new(other_job.handle(), container.sid())
                .unwrap()
                .validate(child.process_handle())
                .is_err()
        );
        assert!(
            Authority::new(job.handle(), other.sid())
                .unwrap()
                .validate(child.process_handle())
                .is_err()
        );
        assert!(authority.validate(unsafe { GetCurrentProcess() }).is_err());
        let (read, _write) = pipe().unwrap();
        assert!(authority.validate(read.0).is_err());
        assert!(authority.validate(0).is_err());
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(child.process_handle(), 1);
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                child.process_handle(),
                5000,
            );
        }
        assert!(authority.validate(child.process_handle()).is_err());
        drop(child);
        drop(job);
        drop(other_job);
        drop(authority);
        container.cleanup().unwrap();
        other.cleanup().unwrap();
    }
    #[test]
    fn anonymous_pipe_reply_is_nonblocking_when_reader_never_drains() {
        let (read, write) = pipe().unwrap();
        nonblocking(write.0).unwrap();
        let bytes = [1u8; 4096];
        assert_eq!(self::write(write.0, &bytes), 4096);
        let started = Instant::now();
        let mut amount = 0;
        let result = unsafe { WriteFile(write.0, bytes.as_ptr(), 1, &mut amount, null_mut()) };
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(amount, 0);
        assert!(result != 0 || unsafe { GetLastError() } == ERROR_NO_DATA);
        drop(read);
    }
    #[test]
    fn malformed_frames_and_out_of_sequence_ack_close_the_channel() {
        let authority = invalid_authority();
        for request in [
            wire::Request {
                operation: wire::ACK,
                id: 1,
                family: 0,
                kind: 0,
                protocol: 0,
                flags: 0,
                child: 0,
            }
            .encode(),
            wire::Request {
                operation: wire::SOCKET,
                id: 2,
                family: 2,
                kind: 1,
                protocol: 6,
                flags: 1,
                child: 0,
            }
            .encode(),
            [0; wire::REQUEST_SIZE],
        ] {
            let (mut client, writer, _reader) = host_client();
            assert_eq!(write(writer.0, &request), wire::REQUEST_SIZE as u32);
            assert!(poll_client(&authority, &mut client, 1).is_err());
        }
    }
    #[test]
    fn partial_frames_wait_and_rejected_sockets_advance_request_id() {
        let authority = invalid_authority();
        let (mut client, writer, reader) = host_client();
        let request = wire::Request {
            operation: wire::SOCKET,
            id: 1,
            family: 1,
            kind: 3,
            protocol: 0,
            flags: 0,
            child: 0,
        }
        .encode();
        write(writer.0, &request[..17]);
        assert!(poll_client(&authority, &mut client, 1).unwrap().is_none());
        assert_eq!(client.filled, 17);
        write(writer.0, &request[17..]);
        poll_client(&authority, &mut client, 1).unwrap();
        assert_eq!(client.id, 2);
        assert!(client.pending.is_none());
        poll_client(&authority, &mut client, 1).unwrap();
        let mut reply = [0u8; wire::RESPONSE_SIZE];
        let mut amount = 0;
        assert_ne!(
            unsafe {
                ReadFile(
                    reader.0,
                    reply.as_mut_ptr(),
                    reply.len() as u32,
                    &mut amount,
                    null_mut(),
                )
            },
            0
        );
        assert_eq!(wire::response_error(&reply, 1), Ok(WSAEINVAL as u32));
    }
    #[test]
    fn pending_socket_requires_matching_ack_and_is_closed_on_timeout() {
        let _winsock = Winsock::start().unwrap();
        let authority = invalid_authority();
        let (mut client, writer, _reader) = host_client();
        let socket = unsafe { WSASocketW(2, 1, 6, null(), 0, WSA_FLAG_NO_HANDLE_INHERIT) };
        assert_ne!(socket, INVALID_SOCKET);
        client.pending = Some(Pending {
            _socket: Socket(socket),
            since: Instant::now(),
        });
        let ack = wire::Request {
            operation: wire::ACK,
            id: 1,
            family: 0,
            kind: 0,
            protocol: 0,
            flags: 0,
            child: 0,
        };
        write(writer.0, &ack.encode());
        poll_client(&authority, &mut client, 1).unwrap();
        assert!(client.pending.is_none());
        assert_eq!(client.id, 2);
        let mut protocol = unsafe { zeroed() };
        assert_ne!(
            unsafe { WSADuplicateSocketW(socket, std::process::id(), &mut protocol) },
            0
        );
        let socket = unsafe { WSASocketW(2, 1, 6, null(), 0, WSA_FLAG_NO_HANDLE_INHERIT) };
        client.pending = Some(Pending {
            _socket: Socket(socket),
            since: Instant::now() - TRANSFER_TIMEOUT,
        });
        assert!(poll_client(&authority, &mut client, 1).is_err());
        drop(client);
        assert_ne!(
            unsafe { WSADuplicateSocketW(socket, std::process::id(), &mut protocol) },
            0
        );
    }
    #[test]
    fn reply_backpressure_and_broker_drop_have_bounded_work() {
        let authority = invalid_authority();
        let (mut client, _writer, _reader) = host_client();
        write(client.response.0, &[0; 4096]);
        client.output = Some((wire::response(1, 0, 0, 0), 0, Instant::now()));
        let start = Instant::now();
        poll_client(&authority, &mut client, 1).unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(client.output.is_some());
        client.output.as_mut().unwrap().2 = Instant::now() - TRANSFER_TIMEOUT;
        assert!(poll_client(&authority, &mut client, 1).is_err());
        let mut container = WindowsAppContainer::create().unwrap();
        let job = job();
        let broker = Broker::new(job.handle(), container.sid()).unwrap();
        assert!(broker.attach(unsafe { GetCurrentProcess() }).is_err());
        let start = Instant::now();
        drop(broker);
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(job);
        container.cleanup().unwrap();
    }
    #[test]
    fn maximum_clients_rejects_setup_before_creating_remote_handles() {
        let authority = invalid_authority();
        let process = duplicate(
            unsafe { GetCurrentProcess() },
            unsafe { GetCurrentProcess() },
            unsafe { GetCurrentProcess() },
            PROCESS_RIGHTS,
            0,
        )
        .unwrap();
        assert!(
            matches!(attach(&authority,process,wire::MAX_CLIENTS),Err(error) if error==WSAENOBUFS as u32)
        );
    }
    #[test]
    fn registration_uses_source_handle_table_and_attaches_before_publishing_endpoints() {
        let mut container = WindowsAppContainer::create().unwrap();
        let job = job();
        let source = suspended(&container, &job);
        let target = suspended(&container, &job);
        let authority = Authority::new(job.handle(), container.sid()).unwrap();
        let (mut client, writer, _reader) = host_client();
        client.process = duplicate(
            unsafe { GetCurrentProcess() },
            source.process_handle(),
            unsafe { GetCurrentProcess() },
            PROCESS_RIGHTS,
            0,
        )
        .unwrap();
        let remote_target = duplicate(
            unsafe { GetCurrentProcess() },
            target.process_handle(),
            source.process_handle(),
            PROCESS_RIGHTS,
            0,
        )
        .unwrap();
        let child = remote_target.0;
        std::mem::forget(remote_target);
        let request = wire::Request {
            operation: wire::REGISTER,
            id: 1,
            family: 0,
            kind: 0,
            protocol: 0,
            flags: 0,
            child: child as u64,
        };
        write(writer.0, &request.encode());
        let registered = poll_client(&authority, &mut client, 1)
            .unwrap()
            .expect("registered client");
        assert_eq!(unsafe { GetProcessId(registered.process.0) }, unsafe {
            GetProcessId(target.process_handle())
        });
        let output = &client.output.as_ref().unwrap().0;
        assert_eq!(wire::response_error(output, 1), Ok(0));
        let remote_write = u64::from_le_bytes(output[16..24].try_into().unwrap()) as isize;
        let remote_read = u64::from_le_bytes(output[24..32].try_into().unwrap()) as isize;
        assert_ne!(remote_write, 0);
        assert_ne!(remote_read, 0);
        assert_ne!(remote_write, remote_read);
        let target_write = duplicate(
            target.process_handle(),
            remote_write,
            unsafe { GetCurrentProcess() },
            0,
            DUPLICATE_SAME_ACCESS,
        )
        .unwrap();
        let mut registered = registered;
        let malformed = [0u8; wire::REQUEST_SIZE];
        write(target_write.0, &malformed);
        assert!(poll_client(&authority, &mut registered, 2).is_err());
        close_remote(source.process_handle(), child);
        close_remote(target.process_handle(), remote_write);
        close_remote(target.process_handle(), remote_read);
        drop(registered);
        drop(client);
        drop(authority);
        drop(source);
        drop(target);
        drop(job);
        container.cleanup().unwrap();
    }
    #[test]
    fn registered_host_process_is_rejected_and_valid_socket_stays_pending_until_ack() {
        let _winsock = Winsock::start().unwrap();
        let mut container = WindowsAppContainer::create().unwrap();
        let job = job();
        let source = suspended(&container, &job);
        let authority = Authority::new(job.handle(), container.sid()).unwrap();
        let (mut client, writer, _reader) = host_client();
        client.process = duplicate(
            unsafe { GetCurrentProcess() },
            source.process_handle(),
            unsafe { GetCurrentProcess() },
            PROCESS_RIGHTS,
            0,
        )
        .unwrap();
        let remote_host = duplicate(
            unsafe { GetCurrentProcess() },
            unsafe { GetCurrentProcess() },
            source.process_handle(),
            PROCESS_RIGHTS,
            0,
        )
        .unwrap();
        let remote_host_raw = remote_host.0;
        std::mem::forget(remote_host);
        let registration = wire::Request {
            operation: wire::REGISTER,
            id: 1,
            family: 0,
            kind: 0,
            protocol: 0,
            flags: 0,
            child: remote_host_raw as u64,
        };
        write(writer.0, &registration.encode());
        assert!(poll_client(&authority, &mut client, 1).unwrap().is_none());
        assert_eq!(
            wire::response_error(&client.output.as_ref().unwrap().0, 1),
            Ok(WSAEACCES as u32)
        );
        assert_eq!(client.id, 2);
        close_remote(source.process_handle(), remote_host_raw);
        for (family, kind, protocol) in [(2, 1, 6), (2, 2, 17), (23, 1, 6), (23, 2, 17)] {
            let request = wire::Request {
                operation: wire::SOCKET,
                id: client.id,
                family,
                kind,
                protocol,
                flags: 1,
                child: 0,
            };
            let mut response = wire::response(client.id, 0, 0, 0);
            create_socket(&authority, &mut client, &request, &mut response).unwrap();
            let transferred = unsafe {
                std::ptr::read_unaligned(response[32..660].as_ptr().cast::<WSAPROTOCOL_INFOW>())
            };
            assert_eq!(transferred.iAddressFamily, family);
            assert_eq!(transferred.iSocketType, kind);
            assert_eq!(transferred.iProtocol, protocol);
            assert!(client.pending.is_some());
            client.output = None;
            let ack = wire::Request {
                operation: wire::ACK,
                id: client.id,
                family: 0,
                kind: 0,
                protocol: 0,
                flags: 0,
                child: 0,
            };
            write(writer.0, &ack.encode());
            poll_client(&authority, &mut client, 1).unwrap();
            assert!(client.pending.is_none());
            assert_eq!(
                wire::response_error(&client.output.as_ref().unwrap().0, ack.id),
                Ok(0)
            );
        }
        // Active contained clients do not prevent stop/join; the suspended child never reads.
        let broker = Broker::new(job.handle(), container.sid()).unwrap();
        let channel = broker.attach(source.process_handle()).unwrap();
        assert_ne!(channel.request, channel.response);
        let start = Instant::now();
        drop(broker);
        assert!(start.elapsed() < Duration::from_secs(1));
        close_remote(source.process_handle(), channel.request);
        close_remote(source.process_handle(), channel.response);
        drop(client);
        drop(authority);
        drop(source);
        drop(job);
        container.cleanup().unwrap();
    }
}
