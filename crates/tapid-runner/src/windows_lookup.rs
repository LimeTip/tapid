//! Cancellable native DNS work; no caller pointers or namespace providers cross the channel.
use super::super::native::OwnedHandle;
use super::super::{lookup_protocol as wire, network_protocol};
use std::{mem::zeroed, ptr::null};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    Networking::WinSock::*,
    System::{
        IO::OVERLAPPED,
        Threading::{CreateEventW, WaitForSingleObject},
    },
};
struct State {
    hints: ADDRINFOEXW,
    result: *mut ADDRINFOEXW,
    overlapped: OVERLAPPED,
    cancel: isize,
    timeout: TIMEVAL,
}
pub(super) struct Lookup {
    // Every address retained by the asynchronous API points into these stable allocations.
    state: Box<State>,
    _query: wire::Query,
    event: OwnedHandle,
    pending: bool,
    error: i32,
}
impl Lookup {
    pub(super) fn start(query: wire::Query) -> Result<Self, u32> {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event == 0 {
            return Err(WSAENOBUFS as u32);
        }
        let mut state: Box<State> = Box::new(unsafe { zeroed() });
        state.hints.ai_family = query.family;
        state.hints.ai_socktype = query.kind;
        state.hints.ai_protocol = query.protocol;
        state.hints.ai_flags = query.flags as i32;
        state.overlapped.hEvent = event;
        state.timeout.tv_sec = 5;
        let error = unsafe {
            GetAddrInfoExW(
                if query.name.is_empty() {
                    null()
                } else {
                    query.name.as_ptr()
                },
                if query.service.is_empty() {
                    null()
                } else {
                    query.service.as_ptr()
                },
                NS_DNS,
                null(),
                &state.hints,
                &mut state.result,
                &state.timeout,
                &state.overlapped,
                None,
                &mut state.cancel,
            )
        };
        Ok(Self {
            state,
            _query: query,
            event: OwnedHandle(event),
            pending: error == WSA_IO_PENDING,
            error,
        })
    }
    pub(super) fn poll(&mut self, id: u32) -> Option<[u8; network_protocol::RESPONSE_SIZE]> {
        if self.pending {
            match unsafe { WaitForSingleObject(self.event.0, 0) } {
                WAIT_TIMEOUT => return None,
                WAIT_OBJECT_0 => {}
                _ => return Some(network_protocol::response(id, WSAEINVAL as u32, 0, 0)),
            }
            self.error = unsafe { GetAddrInfoExOverlappedResult(&self.state.overlapped) };
            self.pending = false;
        }
        let answer = if self.error == 0 {
            self.answer()
                .and_then(|a| a.encode(id).ok_or(WSAENOBUFS as u32))
        } else {
            Err(self.error as u32)
        };
        Some(answer.unwrap_or_else(|e| network_protocol::response(id, e, 0, 0)))
    }
    fn answer(&self) -> Result<wire::Answer, u32> {
        let mut answer = wire::Answer {
            canonical: vec![0],
            addresses: Vec::new(),
        };
        let mut at = self.state.result;
        while !at.is_null() {
            if answer.addresses.len() == wire::MAX_ADDRESSES {
                return Err(WSAENOBUFS as u32);
            }
            // SAFETY: the completed system resolver owns this list until FreeAddrInfoExW.
            let info = unsafe { &*at };
            if !matches!((info.ai_family, info.ai_addrlen), (2, 16) | (23, 28))
                || info.ai_addr.is_null()
            {
                return Err(WSAEINVAL as u32);
            }
            if !info.ai_canonname.is_null() {
                let mut name = Vec::new();
                for index in 0..256 {
                    let unit = unsafe { *info.ai_canonname.add(index) };
                    name.push(unit);
                    if unit == 0 {
                        break;
                    }
                }
                if name.last() != Some(&0) || (answer.canonical != [0] && answer.canonical != name)
                {
                    return Err(WSAENOBUFS as u32);
                }
                answer.canonical = name;
            }
            answer.addresses.push(wire::Address {
                flags: info.ai_flags,
                family: info.ai_family,
                kind: info.ai_socktype,
                protocol: info.ai_protocol,
                bytes: unsafe {
                    std::slice::from_raw_parts(info.ai_addr.cast::<u8>(), info.ai_addrlen)
                }
                .to_vec(),
            });
            at = info.ai_next;
        }
        Ok(answer)
    }
}
impl Drop for Lookup {
    fn drop(&mut self) {
        if self.pending {
            // Windows guarantees cancellation signals the completion event immediately, even if
            // legacy provider work continues. All retained buffers outlive that signal; NS_DNS only.
            unsafe {
                GetAddrInfoExCancel(&self.state.cancel);
                WaitForSingleObject(self.event.0, u32::MAX);
            }
        }
        if !self.state.result.is_null() {
            unsafe { FreeAddrInfoExW(self.state.result) };
        }
        let _ = size_of::<State>();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_lookup_preserves_null_and_empty_host_semantics() {
        let _winsock = super::super::Winsock::start().unwrap();
        for name in [Vec::new(), vec![0]] {
            let service: Vec<_> = "443\0".encode_utf16().collect();
            let mut hints: ADDRINFOW = unsafe { zeroed() };
            hints.ai_family = 2;
            hints.ai_socktype = 1;
            hints.ai_protocol = 6;
            hints.ai_flags = AI_PASSIVE as i32;
            let mut baseline = std::ptr::null_mut();
            assert_eq!(
                unsafe {
                    GetAddrInfoW(
                        if name.is_empty() {
                            null()
                        } else {
                            name.as_ptr()
                        },
                        service.as_ptr(),
                        &hints,
                        &mut baseline,
                    )
                },
                0
            );
            let mut expected = std::collections::BTreeSet::new();
            let mut at = baseline;
            while !at.is_null() {
                let info = unsafe { &*at };
                expected.insert(
                    unsafe {
                        std::slice::from_raw_parts(info.ai_addr.cast::<u8>(), info.ai_addrlen)
                    }
                    .to_vec(),
                );
                at = info.ai_next;
            }
            unsafe { FreeAddrInfoW(baseline) };
            let mut lookup = Lookup::start(wire::Query {
                name,
                service,
                family: 2,
                kind: 1,
                protocol: 6,
                flags: AI_PASSIVE,
            })
            .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
            let bytes = loop {
                if let Some(bytes) = lookup.poll(1) {
                    break bytes;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(2));
            };
            assert_eq!(network_protocol::response_error(&bytes, 1), Ok(0));
            let actual = wire::Answer::decode(&bytes)
                .unwrap()
                .addresses
                .into_iter()
                .map(|a| a.bytes)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, expected);
        }
    }
    #[test]
    fn native_lookup_results_and_cancellation_keep_owned_storage_alive() {
        let _winsock = super::super::Winsock::start().unwrap();
        for name in ["localhost\0", "127.0.0.1\0", "::1\0"] {
            let query = wire::Query {
                name: name.encode_utf16().collect(),
                service: "443\0".encode_utf16().collect(),
                family: 0,
                kind: 1,
                protocol: 6,
                flags: 0,
            };
            let mut lookup = Lookup::start(query).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
            let bytes = loop {
                if let Some(bytes) = lookup.poll(1) {
                    break bytes;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(2));
            };
            assert_eq!(network_protocol::response_error(&bytes, 1), Ok(0));
            assert!(!wire::Answer::decode(&bytes).unwrap().addresses.is_empty());
        }
        let query = wire::Query {
            name: "localhost\0".encode_utf16().collect(),
            service: vec![0],
            family: 0,
            kind: 1,
            protocol: 6,
            flags: 0,
        };
        drop(Lookup::start(query).unwrap());
    }
}
