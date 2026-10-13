//! Native preparation primitives used by the host and by contained parent processes.
//! These operate only on held handles. They do not expose a PID/path request service.
use std::{ffi::c_void, mem, ptr};

pub type Handle = isize;
pub type RemoteEntry = unsafe extern "system" fn(*mut c_void) -> u32;

#[repr(C)]
pub struct ProcessInfo {
    pub process: Handle,
    pub thread: Handle,
    pub pid: u32,
    pub tid: u32,
}

#[repr(C)]
struct ModuleEntry {
    size: u32,
    module_id: u32,
    pid: u32,
    global_count: u32,
    process_count: u32,
    base: *mut u8,
    image_size: u32,
    module: Handle,
    name: [u16; 256],
    path: [u16; 260],
}

#[repr(C)]
pub struct MemoryInfo {
    pub base: *mut c_void,
    pub allocation: *mut c_void,
    pub allocation_protect: u32,
    pub partition: u16,
    pub size: usize,
    pub state: u32,
    pub protect: u32,
    pub kind: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetCurrentProcess() -> Handle;
    pub fn GetModuleHandleW(name: *const u16) -> Handle;
    pub fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut Handle) -> i32;
    pub fn GetModuleFileNameW(module: Handle, path: *mut u16, length: u32) -> u32;
    pub fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
    pub fn GetLastError() -> u32;
    pub fn SetLastError(error: u32);
    pub fn GetProcessId(process: Handle) -> u32;
    pub fn CloseHandle(handle: Handle) -> i32;
    pub fn DuplicateHandle(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    pub fn VirtualProtect(address: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
    fn VirtualAllocEx(
        process: Handle,
        address: *mut c_void,
        size: usize,
        kind: u32,
        protection: u32,
    ) -> *mut c_void;
    fn VirtualFreeEx(process: Handle, address: *mut c_void, size: usize, kind: u32) -> i32;
    fn WriteProcessMemory(
        process: Handle,
        address: *mut c_void,
        source: *const c_void,
        size: usize,
        written: *mut usize,
    ) -> i32;
    fn CreateRemoteThread(
        process: Handle,
        attributes: *const c_void,
        stack: usize,
        start: RemoteEntry,
        parameter: *mut c_void,
        flags: u32,
        id: *mut u32,
    ) -> Handle;
    pub fn WaitForSingleObject(handle: Handle, timeout: u32) -> u32;
    fn GetExitCodeThread(handle: Handle, code: *mut u32) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
    fn Module32FirstW(snapshot: Handle, entry: *mut ModuleEntry) -> i32;
    fn Module32NextW(snapshot: Handle, entry: *mut ModuleEntry) -> i32;
    pub fn ResumeThread(thread: Handle) -> u32;
    pub fn TerminateProcess(process: Handle, exit: u32) -> i32;
    #[allow(dead_code)] // Host-only export inspection in this shared ABI module.
    pub fn LoadLibraryExW(name: *const u16, file: Handle, flags: u32) -> Handle;
    #[allow(dead_code)] // Host-only export inspection in this shared ABI module.
    pub fn FreeLibrary(module: Handle) -> i32;
    fn IsWow64Process2(process: Handle, machine: *mut u16, native_machine: *mut u16) -> i32;
    pub fn VirtualQuery(
        address: *const c_void,
        information: *mut MemoryInfo,
        length: usize,
    ) -> usize;
    fn VirtualQueryEx(
        process: Handle,
        address: *const c_void,
        information: *mut MemoryInfo,
        length: usize,
    ) -> usize;
    fn K32GetMappedFileNameW(
        process: Handle,
        address: *const c_void,
        name: *mut u16,
        length: u32,
    ) -> u32;
    pub fn CompareStringOrdinal(
        a: *const u16,
        a_len: i32,
        b: *const u16,
        b_len: i32,
        ignore_case: i32,
    ) -> i32;
}

pub struct OwnedHandle(pub Handle);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != 0 && self.0 != -1 {
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub fn module_path(module: Handle) -> Result<Vec<u16>, u32> {
    let mut path = vec![0; 32768];
    let length =
        unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
    if length == 0 || length >= path.len() {
        return Err(unsafe { GetLastError() });
    }
    path.truncate(length);
    Ok(path)
}

pub fn module_in_process(process: Handle, name: &[u16]) -> Result<(usize, usize), u32> {
    let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(0x18, GetProcessId(process)) });
    if snapshot.0 == -1 {
        return Err(unsafe { GetLastError() });
    }
    let mut entry: ModuleEntry = unsafe { mem::zeroed() };
    entry.size = mem::size_of::<ModuleEntry>() as u32;
    let mut valid = unsafe { Module32FirstW(snapshot.0, &mut entry) };
    while valid != 0 {
        let full = name.contains(&92) || name.contains(&47);
        let candidate = if full {
            &entry.path[..]
        } else {
            &entry.name[..]
        };
        let length = candidate
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(candidate.len());
        let candidate = without_extended_prefix(&candidate[..length]);
        let expected = without_extended_prefix(name);
        if unsafe {
            CompareStringOrdinal(
                candidate.as_ptr(),
                candidate.len() as i32,
                expected.as_ptr(),
                expected.len() as i32,
                1,
            )
        } == 2
        {
            return Ok((entry.base as usize, entry.image_size as usize));
        }
        valid = unsafe { Module32NextW(snapshot.0, &mut entry) };
    }
    Err(126)
}
fn without_extended_prefix(path: &[u16]) -> &[u16] {
    path.strip_prefix(&[92, 92, 63, 92]).unwrap_or(path)
}

pub fn matching_architecture(process: Handle) -> Result<(), u32> {
    let mut current = 0;
    let mut current_native = 0;
    let mut target = 0;
    let mut target_native = 0;
    if unsafe { IsWow64Process2(GetCurrentProcess(), &mut current, &mut current_native) } == 0
        || unsafe { IsWow64Process2(process, &mut target, &mut target_native) } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    if current != 0
        || target != 0
        || current_native != target_native
        || ![0x8664, 0xaa64].contains(&current_native)
    {
        return Err(193);
    }
    Ok(())
}

/// Resolve forwarded system functions through their actual containing module, then use
/// that module's verified remote base. ASLR equality across processes is never assumed.
pub fn remote_system_function(process: Handle, name: &std::ffi::CStr) -> Result<usize, u32> {
    let kernel: Vec<_> = "kernel32.dll\0".encode_utf16().collect();
    let address =
        unsafe { GetProcAddress(GetModuleHandleW(kernel.as_ptr()), name.as_ptr().cast()) };
    let mut owner = 0;
    if address.is_null() || unsafe { GetModuleHandleExW(6, address.cast(), &mut owner) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    let path = module_path(owner)?;
    let (base, size) = module_in_process(process, &path)?;
    let offset = (address as usize)
        .checked_sub(owner as usize)
        .ok_or(193u32)?;
    if offset >= size {
        return Err(193);
    }
    base.checked_add(offset).ok_or(193)
}

/// A timeout leaves the allocation in the owned child: freeing it while the remote thread
/// may still use it is unsafe. The caller terminates/drains that child on every error.
pub fn remote_call(process: Handle, start: usize, payload: &[u8]) -> Result<u32, u32> {
    if payload.is_empty() || payload.len() > 256 * 1024 || start == 0 {
        return Err(87);
    }
    let memory = unsafe { VirtualAllocEx(process, ptr::null_mut(), payload.len(), 0x3000, 4) };
    if memory.is_null() {
        return Err(unsafe { GetLastError() });
    }
    let mut written = 0;
    if unsafe {
        WriteProcessMemory(
            process,
            memory,
            payload.as_ptr().cast(),
            payload.len(),
            &mut written,
        )
    } == 0
        || written != payload.len()
    {
        let error = unsafe { GetLastError() };
        unsafe { VirtualFreeEx(process, memory, 0, 0x8000) };
        return Err(error);
    }
    let result = remote_thread(process, start, memory);
    if result.is_ok() {
        unsafe { VirtualFreeEx(process, memory, 0, 0x8000) };
    }
    result
}

fn remote_thread(process: Handle, start: usize, parameter: *mut c_void) -> Result<u32, u32> {
    let start: RemoteEntry = unsafe { mem::transmute(start) };
    let thread = OwnedHandle(unsafe {
        CreateRemoteThread(
            process,
            ptr::null(),
            0,
            start,
            parameter,
            0,
            ptr::null_mut(),
        )
    });
    if thread.0 == 0 {
        let error = unsafe { GetLastError() };
        return Err(error);
    }
    let wait = unsafe { WaitForSingleObject(thread.0, 5000) };
    if wait != 0 {
        return Err(if wait == 258 {
            1460
        } else {
            unsafe { GetLastError() }
        });
    }
    let mut result = 0;
    let queried = unsafe { GetExitCodeThread(thread.0, &mut result) };
    let error = unsafe { GetLastError() };
    if queried == 0 { Err(error) } else { Ok(result) }
}

pub fn load_component(process: Handle, path: &[u16]) -> Result<(usize, usize), u32> {
    matching_architecture(process)?;
    // A freshly suspended process has not initialized its loader module list. Run a
    // trusted ntdll thread-exit entry first: loader initialization completes, while the
    // original application thread remains suspended. Resolve ntdll from actual mapped
    // file identity, so this bootstrap also does not depend on identical ASLR addresses.
    initialize_loader(process).inspect_err(|error| report_failure("initialize loader", *error))?;
    let mut terminated = path.to_vec();
    terminated.push(0);
    let bytes =
        unsafe { std::slice::from_raw_parts(terminated.as_ptr().cast(), terminated.len() * 2) };
    // LoadLibraryW's DWORD thread exit truncates a 64-bit HMODULE. Locate the actual
    // module through the snapshot instead of interpreting that value as an address.
    remote_call(
        process,
        remote_system_function(process, c"LoadLibraryW")?,
        bytes,
    )
    .inspect_err(|error| report_failure("load component", *error))?;
    module_in_process(process, path).inspect_err(|error| report_failure("locate component", *error))
}

pub(super) fn report_failure(stage: &'static str, error: u32) {
    use std::io::Write;
    // Fixed internal stage names and numeric errors disclose no paths or handles.
    // Ignore closed stderr; diagnostic failure must not change native API behavior.
    let _ = writeln!(
        std::io::stderr().lock(),
        "Tapid Node compatibility: {stage} failed (Win32 {error})"
    );
}

fn initialize_loader(process: Handle) -> Result<(), u32> {
    let name: Vec<_> = "ntdll.dll\0".encode_utf16().collect();
    let local = unsafe { GetModuleHandleW(name.as_ptr()) };
    let entry = unsafe { GetProcAddress(local, c"RtlExitUserThread".as_ptr().cast()) } as usize;
    let offset = entry.checked_sub(local as usize).ok_or(193u32)?;
    let mut expected = vec![0u16; 32768];
    let length = unsafe {
        K32GetMappedFileNameW(
            GetCurrentProcess(),
            local as *const c_void,
            expected.as_mut_ptr(),
            expected.len() as u32,
        )
    } as usize;
    if length == 0 || length >= expected.len() || entry == 0 {
        return Err(193);
    }
    expected.truncate(length);
    let mut address = 0usize;
    for _ in 0..8192 {
        let mut information: MemoryInfo = unsafe { mem::zeroed() };
        if unsafe {
            VirtualQueryEx(
                process,
                address as *const c_void,
                &mut information,
                mem::size_of::<MemoryInfo>(),
            )
        } == 0
        {
            break;
        }
        if information.kind == 0x1000000 && information.base == information.allocation {
            let mut path = vec![0u16; 32768];
            let length = unsafe {
                K32GetMappedFileNameW(
                    process,
                    information.base,
                    path.as_mut_ptr(),
                    path.len() as u32,
                )
            } as usize;
            if length == expected.len() && path[..length] == expected {
                let exit = remote_thread(
                    process,
                    (information.base as usize)
                        .checked_add(offset)
                        .ok_or(193u32)?,
                    ptr::null_mut(),
                )?;
                if exit != 0 {
                    return Err(193);
                }
                return Ok(());
            }
        }
        let next = (information.base as usize)
            .checked_add(information.size)
            .ok_or(193u32)?;
        if next <= address {
            break;
        }
        address = next;
    }
    Err(126)
}

pub fn export_offset(module: Handle, name: &std::ffi::CStr, size: usize) -> Result<usize, u32> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) } as usize;
    let offset = address.checked_sub(module as usize).ok_or(127u32)?;
    if address == 0 || offset >= size {
        Err(127)
    } else {
        Ok(offset)
    }
}
