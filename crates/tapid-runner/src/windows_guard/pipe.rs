//! Local-only precise-access named pipes with fixed, bounded, nonblocking frames.
use super::{PIPE, owned, protocol::SIZE, raw, wide};
use std::{
    os::windows::io::OwnedHandle,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Pipes::*,
};
// FILE_APPEND_DATA is FILE_CREATE_PIPE_INSTANCE for pipes: never delegate it to clients.
const CLIENT_ACCESS: u32 = FILE_READ_DATA | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;
pub(super) fn listener(first: bool) -> Result<OwnedHandle, u32> {
    listener_at(PIPE, first)
}
fn listener_at(endpoint: &str, first: bool) -> Result<OwnedHandle, u32> {
    let sddl = wide("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0012019b;;;IU)");
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let result = owned(unsafe {
        CreateNamedPipeW(
            wide(endpoint).as_ptr(),
            PIPE_ACCESS_DUPLEX
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            255,
            SIZE as u32,
            SIZE as u32,
            0,
            &security,
        )
    });
    unsafe {
        LocalFree(descriptor);
    }
    result
}
pub(super) fn connected(pipe: isize) -> Result<bool, u32> {
    if unsafe { ConnectNamedPipe(pipe, null_mut()) } != 0 {
        return Ok(true);
    }
    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => Ok(true),
        ERROR_PIPE_LISTENING => Ok(false),
        ERROR_NO_DATA => {
            if unsafe { DisconnectNamedPipe(pipe) } == 0 {
                return Err(unsafe { GetLastError() });
            }
            Ok(false)
        }
        e => Err(e),
    }
}
pub(super) fn connect() -> Result<OwnedHandle, u32> {
    // Native WFP work is serialized by the service. Concurrent launches may
    // legitimately wait longer than one exchange while earlier clients register.
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let pipe = owned(unsafe {
            CreateFileW(
                wide(PIPE).as_ptr(),
                CLIENT_ACCESS,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                0,
            )
        });
        match pipe {
            Ok(pipe) => {
                let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
                if unsafe { SetNamedPipeHandleState(raw(&pipe), &mode, null(), null()) } == 0 {
                    return Err(unsafe { GetLastError() });
                }
                return Ok(pipe);
            }
            Err(ERROR_PIPE_BUSY) if Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(e),
        }
    }
}
pub(super) fn read(pipe: isize, b: &mut [u8]) -> Result<usize, u32> {
    let mut n = 0;
    if unsafe { ReadFile(pipe, b.as_mut_ptr(), b.len() as u32, &mut n, null_mut()) } != 0 {
        return Ok(n as usize);
    }
    match unsafe { GetLastError() } {
        ERROR_NO_DATA | ERROR_PIPE_LISTENING => Ok(0),
        e => Err(e),
    }
}
pub(super) fn write(pipe: isize, b: &[u8]) -> Result<usize, u32> {
    let mut n = 0;
    if unsafe { WriteFile(pipe, b.as_ptr(), b.len() as u32, &mut n, null_mut()) } != 0 {
        return Ok(n as usize);
    }
    match unsafe { GetLastError() } {
        ERROR_NO_DATA => Ok(0),
        e => Err(e),
    }
}
pub(super) fn exchange(pipe: isize, message: &[u8; SIZE]) -> Result<[u8; SIZE], u32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut sent = 0;
    let mut got = 0;
    let mut response = [0; SIZE];
    while got < SIZE {
        if Instant::now() >= deadline {
            return Err(ERROR_TIMEOUT);
        }
        if sent < SIZE {
            sent += write(pipe, &message[sent..])?;
        } else {
            got += read(pipe, &mut response[got..])?;
        }
        if got < SIZE {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_listener_recovers_when_client_closes_before_acceptance() {
        let owner = tapid_test_support::TempProject::new("guard-pipe-recovery").unwrap();
        let endpoint = format!(
            r"\\.\pipe\Tapid.guard-test.{}",
            owner.path().file_name().unwrap().to_string_lossy()
        );
        let server = listener_at(&endpoint, true).unwrap();
        let open = || {
            owned(unsafe {
                CreateFileW(
                    wide(&endpoint).as_ptr(),
                    CLIENT_ACCESS,
                    0,
                    null(),
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    0,
                )
            })
            .unwrap()
        };
        assert!(!connected(raw(&server)).unwrap());
        drop(open());
        assert!(!connected(raw(&server)).unwrap());
        assert!(!connected(raw(&server)).unwrap());
        let client = open();
        assert!(connected(raw(&server)).unwrap());
        let request = [0x5a; SIZE];
        assert_eq!(write(raw(&client), &request).unwrap(), SIZE);
        let mut received = [0; SIZE];
        assert_eq!(read(raw(&server), &mut received).unwrap(), SIZE);
        assert_eq!(received, request);
        // Exercise the production identification-level peer/token binding before elevation.
        let (_caller, _identity) = super::super::identity::caller(raw(&server)).unwrap();
        let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
        assert_ne!(
            unsafe { SetNamedPipeHandleState(raw(&client), &mode, null(), null()) },
            0
        );
    }
    #[test]
    fn guard_client_cannot_create_pipe_instances() {
        assert_eq!(CLIENT_ACCESS & FILE_CREATE_PIPE_INSTANCE, 0);
        assert_eq!(CLIENT_ACCESS & FILE_WRITE_DATA, FILE_WRITE_DATA);
    }
}
