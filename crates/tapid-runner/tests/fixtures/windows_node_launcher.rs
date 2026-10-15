//! Native test addon: resolve N-API from unchanged Node; import process creation normally.
use std::{
    ffi::{CStr, c_void},
    ptr,
};
type Value = *mut c_void;
type Callback = unsafe extern "C" fn(Value, Value) -> Value;
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_register_module_v1(env: Value, exports: Value) -> Value {
    type Create =
        unsafe extern "C" fn(Value, *const u8, usize, Option<Callback>, Value, *mut Value) -> i32;
    type Set = unsafe extern "C" fn(Value, Value, *const u8, Value) -> i32;
    let create: Create = unsafe { api(c"napi_create_function") };
    let set: Set = unsafe { api(c"napi_set_named_property") };
    let mut function = ptr::null_mut();
    if unsafe {
        create(
            env,
            c"launch".as_ptr().cast(),
            usize::MAX,
            Some(launch),
            ptr::null_mut(),
            &mut function,
        )
    } != 0
        || unsafe { set(env, exports, c"launch".as_ptr().cast(), function) } != 0
    {
        return ptr::null_mut();
    }
    if unsafe {
        create(
            env,
            c"reopen".as_ptr().cast(),
            usize::MAX,
            Some(reopen),
            ptr::null_mut(),
            &mut function,
        )
    } != 0
        || unsafe { set(env, exports, c"reopen".as_ptr().cast(), function) } != 0
    {
        return ptr::null_mut();
    }
    exports
}
