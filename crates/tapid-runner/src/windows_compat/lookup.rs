//! Native GetAddrInfoW adaptation for stock libuv. Results contain only owned child memory.
use super::{SETUP, lookup_protocol as wire, network, network_protocol};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr,
    sync::Mutex,
};
#[repr(C)]
pub(super) struct Info {
    flags: i32,
    family: i32,
    kind: i32,
    protocol: i32,
    length: usize,
    canonical: *mut u16,
    address: *mut c_void,
    next: *mut Info,
}
static RESULTS: Mutex<BTreeMap<usize, Box<[u64]>>> = Mutex::new(BTreeMap::new());
#[link(name = "ws2_32")]
unsafe extern "system" {
    fn FreeAddrInfoW(info: *mut Info);
    fn WSASetLastError(error: i32);
}
unsafe fn string(pointer: *const u16, limit: usize) -> Result<Vec<u16>, u32> {
    if pointer.is_null() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for index in 0..limit {
        // SAFETY: GetAddrInfoW's caller supplies a valid terminated UTF-16 input.
        let unit = unsafe { *pointer.add(index) };
        result.push(unit);
        if unit == 0 {
            return Ok(result);
        }
    }
    Err(10022)
}
fn resolve(query: wire::Query) -> Result<*mut Info, u32> {
    let setup = SETUP.get().ok_or(10050u32)?;
    if setup.network_request == 0 || setup.network_response == 0 {
        return Err(10013);
    }
    let mut id = network::REQUEST.lock().map_err(|_| 10050u32)?;
    let current = *id;
    let mut request = query.encode(current).ok_or(10022u32)?;
    *id = id.checked_add(1).ok_or(10055u32)?;
    network::transfer(setup.network_request, &mut request, true)?;
    let mut response = [0; network_protocol::RESPONSE_SIZE];
    network::transfer(setup.network_response, &mut response, false)?;
    let error = network_protocol::response_error(&response, current)?;
    if error != 0 {
        return Err(error);
    }
    let answer = wire::Answer::decode(&response).ok_or(10022u32)?;
    drop(id);
    let mut results = RESULTS.lock().map_err(|_| 10050u32)?;
    if results.len() + answer.addresses.len() > 64 * wire::MAX_ADDRESSES {
        return Err(10055);
    }
    // Each node has independent ownership: FreeAddrInfoW permits callers to split a list.
    const WORDS: usize = (size_of::<Info>() + 32 + 512) / 8;
    let mut nodes: Vec<_> = answer
        .addresses
        .iter()
        .map(|_| vec![0u64; WORDS].into_boxed_slice())
        .collect();
    let pointers: Vec<_> = nodes
        .iter_mut()
        .map(|node| node.as_mut_ptr().cast::<Info>())
        .collect();
    for (index, address) in answer.addresses.iter().enumerate() {
        // SAFETY: each allocation has disjoint, aligned node, sockaddr and canonical-name storage.
        unsafe {
            let node = pointers[index];
            let socket = node.add(1).cast::<u8>();
            let canonical = socket.add(32).cast::<u16>();
            ptr::copy_nonoverlapping(address.bytes.as_ptr(), socket, address.bytes.len());
            if index == 0 {
                ptr::copy_nonoverlapping(
                    answer.canonical.as_ptr(),
                    canonical,
                    answer.canonical.len(),
                );
            }
            node.write(Info {
                flags: address.flags,
                family: address.family,
                kind: address.kind,
                protocol: address.protocol,
                length: address.bytes.len(),
                canonical: if index == 0 && answer.canonical[0] != 0 {
                    canonical
                } else {
                    ptr::null_mut()
                },
                address: socket.cast(),
                next: pointers.get(index + 1).copied().unwrap_or(ptr::null_mut()),
            });
        }
    }
    let head = pointers[0];
    for (pointer, node) in pointers.into_iter().zip(nodes) {
        results.insert(pointer as usize, node);
    }
    Ok(head)
}
pub(super) unsafe extern "system" fn lookup(
    name: *const u16,
    service: *const u16,
    hints: *const Info,
    result: *mut *mut Info,
) -> i32 {
    if result.is_null() {
        unsafe { WSASetLastError(10022) };
        return 10022;
    }
    unsafe { *result = ptr::null_mut() };
    let attempt = (|| {
        let empty: Info = unsafe { zeroed() };
        let hints = if hints.is_null() {
            &empty
        } else {
            unsafe { &*hints }
        };
        let query = wire::Query {
            name: unsafe { string(name, 256) }?,
            service: unsafe { string(service, 32) }?,
            family: hints.family,
            kind: hints.kind,
            protocol: hints.protocol,
            flags: hints.flags as u32,
        };
        resolve(query)
    })();
    match attempt {
        Ok(info) => {
            unsafe { *result = info };
            0
        }
        Err(error) => {
            unsafe { WSASetLastError(error as i32) };
            error as i32
        }
    }
}
pub(super) unsafe extern "system" fn free(mut info: *mut Info) {
    while !info.is_null() {
        let Ok(mut results) = RESULTS.lock() else {
            return;
        };
        if let Some(node) = results.remove(&(info as usize)) {
            // SAFETY: the map proves this is an owned node; preserve its caller-modified link before freeing it.
            info = unsafe { (*node.as_ptr().cast::<Info>()).next };
        } else {
            drop(results);
            // SAFETY: a list not allocated by this adapter keeps the original Windows allocator contract.
            unsafe { FreeAddrInfoW(info) };
            return;
        }
    }
}
