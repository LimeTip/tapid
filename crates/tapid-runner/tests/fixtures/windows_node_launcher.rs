//! Native test addon: resolve N-API from unchanged Node; import process creation normally.
use std::{
    ffi::{CStr, c_void},
    ptr,
};
type Value = *mut c_void;
type Callback = unsafe extern "C" fn(Value, Value) -> Value;
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
    fn WSAGetLastError() -> i32;
    fn WSASetLastError(error: i32);
    fn closesocket(socket: usize) -> i32;
}
#[repr(C)]
struct AddressInfo {
    flags: i32,
    family: i32,
    kind: i32,
    protocol: i32,
    length: usize,
    canonical: *mut u16,
    address: *mut c_void,
    next: *mut AddressInfo,
}
#[link(name = "ws2_32")]
unsafe extern "system" {
    fn GetAddrInfoW(
        name: *const u16,
        service: *const u16,
        hints: *const AddressInfo,
        result: *mut *mut AddressInfo,
    ) -> i32;
    fn FreeAddrInfoW(info: *mut AddressInfo);
}
unsafe extern "C" fn lookup_recovery(env: Value, _: Value) -> Value {
    let check = || -> i32 {
        let name: Vec<_> = "localhost\0".encode_utf16().collect();
        let service: Vec<_> = "443\0".encode_utf16().collect();
        let mut hints: AddressInfo = unsafe { std::mem::zeroed() };
        hints.flags = i32::MIN;
        hints.kind = 1;
        hints.protocol = 6;
        let mut result = ptr::null_mut();
        unsafe { WSASetLastError(12345) };
        let rejected =
            unsafe { GetAddrInfoW(name.as_ptr(), service.as_ptr(), &hints, &mut result) };
        if rejected != 10022 || unsafe { WSAGetLastError() } != 10022 || !result.is_null() {
            return 1;
        }
        hints.flags = 2;
        for _ in 0..128 {
            let error =
                unsafe { GetAddrInfoW(name.as_ptr(), service.as_ptr(), &hints, &mut result) };
            if error != 0 {
                return error;
            }
            if result.is_null() {
                return 2;
            }
            let info = unsafe { &*result };
            let valid = matches!((info.family, info.length), (2, 16) | (23, 28))
                && !info.address.is_null()
                && !info.canonical.is_null();
            unsafe { FreeAddrInfoW(result) };
            if !valid {
                return 3;
            }
        }
        for head_first in [false, true] {
            let error =
                unsafe { GetAddrInfoW(name.as_ptr(), service.as_ptr(), &hints, &mut result) };
            if error != 0 {
                return error;
            }
            let tail = unsafe { (*result).next };
            if tail.is_null() {
                unsafe { FreeAddrInfoW(result) };
                return 4;
            }
            unsafe {
                (*result).next = ptr::null_mut();
            }
            if head_first {
                unsafe {
                    FreeAddrInfoW(result);
                    FreeAddrInfoW(tail);
                }
            } else {
                unsafe {
                    FreeAddrInfoW(tail);
                    FreeAddrInfoW(result);
                }
            }
        }
        // Free a genuine system allocation through the adapted import as well.
        type Lookup = unsafe extern "system" fn(
            *const u16,
            *const u16,
            *const AddressInfo,
            *mut *mut AddressInfo,
        ) -> i32;
        let module: Vec<_> = "ws2_32.dll\0".encode_utf16().collect();
        let direct: Lookup = unsafe {
            std::mem::transmute(GetProcAddress(
                GetModuleHandleW(module.as_ptr()),
                c"GetAddrInfoW".as_ptr().cast(),
            ))
        };
        let numeric: Vec<_> = "127.0.0.1\0".encode_utf16().collect();
        hints.flags = 4;
        let error = unsafe { direct(numeric.as_ptr(), service.as_ptr(), &hints, &mut result) };
        if error != 0 {
            return error;
        }
        unsafe { FreeAddrInfoW(result) };
        0
    };
    type Integer = unsafe extern "C" fn(Value, i32, *mut Value) -> i32;
    let integer: Integer = unsafe { api(c"napi_create_int32") };
    let mut result = ptr::null_mut();
    unsafe { integer(env, check(), &mut result) };
    result
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFinalPathNameByHandleW(handle: isize, path: *mut u16, length: u32, flags: u32) -> u32;
    fn ReOpenFile(handle: isize, access: u32, share: u32, flags: u32) -> isize;
    fn CloseHandle(handle: isize) -> i32;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> *const c_void;
    fn LoadLibraryW(name: *const u16) -> isize;
    fn FreeLibrary(module: isize) -> i32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: isize,
        class: u32,
        data: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
}
unsafe fn api<T: Copy>(name: &CStr) -> T {
    let address = unsafe { GetProcAddress(GetModuleHandleW(ptr::null()), name.as_ptr().cast()) };
    assert!(!address.is_null());
    unsafe { std::mem::transmute_copy(&address) }
}
unsafe fn arguments(env: Value, info: Value) -> Option<[String; 2]> {
    type Info =
        unsafe extern "C" fn(Value, Value, *mut usize, *mut Value, *mut Value, *mut Value) -> i32;
    type Text = unsafe extern "C" fn(Value, Value, *mut u8, usize, *mut usize) -> i32;
    let get_info: Info = unsafe { api(c"napi_get_cb_info") };
    let text: Text = unsafe { api(c"napi_get_value_string_utf8") };
    let mut count = 2;
    let mut values = [ptr::null_mut(); 2];
    if unsafe {
        get_info(
            env,
            info,
            &mut count,
            values.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    } != 0
        || count != 2
    {
        return None;
    }
    let mut result = [String::new(), String::new()];
    for (value, target) in values.into_iter().zip(&mut result) {
        let mut length = 0;
        if unsafe { text(env, value, ptr::null_mut(), 0, &mut length) } != 0 || length > 4096 {
            return None;
        }
        let mut buffer = vec![0; length + 1];
        if unsafe { text(env, value, buffer.as_mut_ptr(), buffer.len(), &mut length) } != 0 {
            return None;
        }
        buffer.truncate(length);
        *target = String::from_utf8(buffer).ok()?;
    }
    Some(result)
}
unsafe extern "C" fn launch(env: Value, info: Value) -> Value {
    type Integer = unsafe extern "C" fn(Value, i32, *mut Value) -> i32;
    // Repeated concurrent loads exercise idempotent import scans and loader references.
    let workers: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                let name: Vec<_> = "kernel32.dll\0".encode_utf16().collect();
                for _ in 0..4 {
                    let module = unsafe { LoadLibraryW(name.as_ptr()) };
                    assert_ne!(module, 0);
                    assert_ne!(unsafe { FreeLibrary(module) }, 0);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let code = unsafe { arguments(env, info) }
        .and_then(|[program, script]| {
            std::process::Command::new(program)
                .args(["-e", &script])
                .status()
                .ok()?
                .code()
        })
        .unwrap_or(99);
    let integer: Integer = unsafe { api(c"napi_create_int32") };
    let mut result = ptr::null_mut();
    unsafe { integer(env, code, &mut result) };
    result
}
unsafe extern "C" fn reopen(env: Value, info: Value) -> Value {
    type Integer = unsafe extern "C" fn(Value, i32, *mut Value) -> i32;
    let mut code = 99;
    if let Some([suffix, _]) = unsafe { arguments(env, info) } {
        code = 2;
        let mut snapshot = vec![0usize; 131072];
        let status = unsafe {
            NtQueryInformationProcess(
                -1,
                51,
                snapshot.as_mut_ptr().cast(),
                (snapshot.len() * 8) as u32,
                ptr::null_mut(),
            )
        };
        if status < 0 {
            return ptr::null_mut();
        }
        let count = snapshot[0];
        for index in 0..count.min((snapshot.len() - 2) / 5) {
            // Only pinned metadata capabilities, not Node's transient runtime handles.
            if snapshot[2 + index * 5 + 3] as u32 != 0x80 {
                continue;
            }
            let handle = snapshot[2 + index * 5] as isize;
            let mut path = [0u16; 2048];
            let len = unsafe {
                GetFinalPathNameByHandleW(handle, path.as_mut_ptr(), path.len() as u32, 10)
            } as usize;
            if len == 0 || len >= path.len() {
                continue;
            }
            if !String::from_utf16_lossy(&path[..len]).ends_with(&suffix) {
                continue;
            }
            code = 0;
            let opened = unsafe { ReOpenFile(handle, 1, 7, 0x02200000) };
            if opened != -1 {
                unsafe { CloseHandle(opened) };
                code = 1;
                break;
            }
        }
    }
    let integer: Integer = unsafe { api(c"napi_create_int32") };
    let mut result = ptr::null_mut();
    unsafe { integer(env, code, &mut result) };
    result
}
unsafe extern "C" fn socket_recovery(env: Value, _: Value) -> Value {
    type Integer = unsafe extern "C" fn(Value, i32, *mut Value) -> i32;
    // Probe a rejected raw socket, then require an ordinary TCP socket to remain usable.
    let rejected = unsafe { WSASocketW(2, 3, 0, ptr::null(), 0, 1) };
    let mut code = if rejected == usize::MAX {
        0
    } else {
        unsafe { closesocket(rejected) };
        1
    };
    let socket = unsafe { WSASocketW(2, 1, 6, ptr::null(), 0, 1) };
    if socket == usize::MAX {
        code = unsafe { WSAGetLastError() };
    } else {
        unsafe { closesocket(socket) };
    }
    let integer: Integer = unsafe { api(c"napi_create_int32") };
    let mut result = ptr::null_mut();
    unsafe { integer(env, code, &mut result) };
    result
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_register_module_v1(env: Value, exports: Value) -> Value {
    type Create =
        unsafe extern "C" fn(Value, *const u8, usize, Option<Callback>, Value, *mut Value) -> i32;
    type Set = unsafe extern "C" fn(Value, Value, *const u8, Value) -> i32;
    let create: Create = unsafe { api(c"napi_create_function") };
    let set: Set = unsafe { api(c"napi_set_named_property") };
    for (name, callback) in [
        (c"launch", launch as Callback),
        (c"reopen", reopen as Callback),
        (c"socketRecovery", socket_recovery as Callback),
        (c"lookupRecovery", lookup_recovery as Callback),
    ] {
        let mut function = ptr::null_mut();
        if unsafe {
            create(
                env,
                name.as_ptr().cast(),
                usize::MAX,
                Some(callback),
                ptr::null_mut(),
                &mut function,
            )
        } != 0
            || unsafe { set(env, exports, name.as_ptr().cast(), function) } != 0
        {
            return ptr::null_mut();
        }
    }
    exports
}
