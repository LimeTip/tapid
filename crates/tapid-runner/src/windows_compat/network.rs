//! Contained compatibility client. Kernel restrictions remain independent of these hooks.
use super::{SETUP, native::*, network_protocol as wire, protocol::Setup};
use std::{ffi::c_void, ptr, sync::Mutex};
pub(super) static REQUEST: Mutex<u32> = Mutex::new(1);
#[link(name = "kernel32")]
unsafe extern "system" {
    fn ReadFile(
        handle: Handle,
        buffer: *mut c_void,
        length: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        buffer: *const c_void,
        length: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}
#[link(name = "ws2_32")]
unsafe extern "system" {
    fn WSASocketW(
        af: i32,
        kind: i32,
        protocol: i32,
        info: *const c_void,
        group: u32,
        flags: u32,
    ) -> usize;
    fn WSASocketA(
        af: i32,
        kind: i32,
        protocol: i32,
        info: *const c_void,
        group: u32,
        flags: u32,
    ) -> usize;
    fn WSAGetLastError() -> i32;
    fn WSASetLastError(error: i32);
    fn closesocket(socket: usize) -> i32;
}
pub(super) fn transfer(handle: Handle, bytes: &mut [u8], write: bool) -> Result<(), u32> {
    let mut at = 0;
    while at < bytes.len() {
        let mut done = 0;
        // SAFETY: the owned setup handle and bounded byte slice remain valid for each synchronous call.
        let result = unsafe {
            if write {
                WriteFile(
                    handle,
                    bytes[at..].as_ptr().cast(),
                    (bytes.len() - at) as u32,
                    &mut done,
                    ptr::null_mut(),
                )
            } else {
                ReadFile(
                    handle,
                    bytes[at..].as_mut_ptr().cast(),
                    (bytes.len() - at) as u32,
                    &mut done,
                    ptr::null_mut(),
                )
            }
        };
        if result == 0 || done == 0 {
            return Err(10050);
        }
        at += done as usize;
    }
    Ok(())
}
fn exchange(setup: &Setup, request: wire::Request) -> Result<[u8; wire::RESPONSE_SIZE], u32> {
    if setup.network_request == 0 || setup.network_response == 0 {
        return Err(10013);
    }
    transfer(setup.network_request, &mut request.encode(), true)?;
    let mut response = [0; wire::RESPONSE_SIZE];
    transfer(setup.network_response, &mut response, false)?;
    let error = wire::response_error(&response, request.id)?;
    if error != 0 { Err(error) } else { Ok(response) }
}
fn request(operation: u32, id: u32, child: u64) -> wire::Request {
    wire::Request {
        operation,
        id,
        family: 0,
        kind: 0,
        protocol: 0,
        flags: 0,
        child,
    }
}
pub(super) fn register_child(process: Handle) -> Result<(Handle, Handle), u32> {
    let setup = SETUP.get().ok_or(10050u32)?;
    if setup.network_request == 0 {
        return Ok((0, 0));
    }
    let mut id = REQUEST.lock().map_err(|_| 10050u32)?;
    let current = *id;
    *id = id.checked_add(1).ok_or(10055u32)?;
    let response = exchange(setup, request(wire::REGISTER, current, process as u64))?;
    let tx = u64::from_le_bytes(response[16..24].try_into().unwrap()) as Handle;
    let rx = u64::from_le_bytes(response[24..32].try_into().unwrap()) as Handle;
    if [tx, rx].iter().any(|h| *h == 0 || *h == -1) {
        return Err(10022);
    }
    Ok((tx, rx))
}
fn broker_socket(af: i32, kind: i32, protocol: i32, flags: u32) -> Result<usize, u32> {
    let setup = SETUP.get().ok_or(10050u32)?;
    let mut id = REQUEST.lock().map_err(|_| 10050u32)?;
    let current = *id;
    let request = wire::Request {
        operation: wire::SOCKET,
        id: current,
        family: af,
        kind,
        protocol,
        flags,
        child: 0,
    };
    if !request.supported_socket() {
        return Err(10022);
    }
    *id = id.checked_add(1).ok_or(10055u32)?;
    let response = exchange(setup, request)?;
    let mut storage = [0u64; 79];
    // SAFETY: aligned storage is larger than the validated fixed native protocol information structure.
    unsafe {
        ptr::copy_nonoverlapping(
            response[32..660].as_ptr(),
            storage.as_mut_ptr().cast::<u8>(),
            wire::PROTOCOL_SIZE,
        )
    };
    let socket = unsafe { WSASocketW(-1, -1, -1, storage.as_ptr().cast(), 0, flags) };
    let error = unsafe { WSAGetLastError() };
    let ack = exchange(setup, self::request(wire::ACK, current, 0));
    if let Err(failure) = ack {
        if socket != usize::MAX {
            unsafe { closesocket(socket) };
        }
        return Err(failure);
    }
    if socket == usize::MAX {
        Err(error as u32)
    } else {
        Ok(socket)
    }
}
fn result(socket: Result<usize, u32>) -> usize {
    match socket {
        Ok(socket) => socket,
        Err(error) => {
            unsafe { WSASetLastError(error as i32) };
            usize::MAX
        }
    }
}
pub(super) unsafe extern "system" fn socket(af: i32, kind: i32, protocol: i32) -> usize {
    result(broker_socket(af, kind, protocol, 1))
}
pub(super) unsafe extern "system" fn socket_w(
    af: i32,
    kind: i32,
    protocol: i32,
    info: *const c_void,
    group: u32,
    flags: u32,
) -> usize {
    if !info.is_null() {
        return unsafe { WSASocketW(af, kind, protocol, info, group, flags) };
    }
    if group != 0 {
        return result(Err(10022));
    }
    result(broker_socket(af, kind, protocol, flags))
}
pub(super) unsafe extern "system" fn socket_a(
    af: i32,
    kind: i32,
    protocol: i32,
    info: *const c_void,
    group: u32,
    flags: u32,
) -> usize {
    if !info.is_null() {
        return unsafe { WSASocketA(af, kind, protocol, info, group, flags) };
    }
    if group != 0 {
        return result(Err(10022));
    }
    result(broker_socket(af, kind, protocol, flags))
}
