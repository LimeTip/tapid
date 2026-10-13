//! Embedded stock-Node compatibility. AppContainer and kernel handle rights enforce
//! authority independently of these hooks; bypassing them cannot grant host access.
#[cfg(not(test))]
mod imports;
#[cfg(not(test))]
mod native;
#[cfg(not(test))]
mod protocol;
#[cfg(test)]
use super::{import_tests as imports, native, protocol};
use native::*;
use protocol::*;
use std::{
    ffi::{CString, c_void},
    ptr,
    sync::{
        OnceLock,
        atomic::{AtomicIsize, AtomicUsize, Ordering},
    },
};

static SELF: AtomicIsize = AtomicIsize::new(0);
static SETUP: OnceLock<Setup> = OnceLock::new();
static PIPE: AtomicUsize = AtomicUsize::new(0);
static FILE: AtomicUsize = AtomicUsize::new(0);
static WFILE: AtomicUsize = AtomicUsize::new(0);
static PROCESS: AtomicUsize = AtomicUsize::new(0);
static FINAL: AtomicUsize = AtomicUsize::new(0);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ReOpenFile(file: Handle, access: u32, share: u32, flags: u32) -> Handle;
}

fn read_memory(base: usize, offset: usize, length: usize) -> Option<Vec<u8>> {
    let start = base.checked_add(offset)?;
    let end = start.checked_add(length)?;
    let mut at = start;
    while at < end {
        let mut information: MemoryInfo = unsafe { std::mem::zeroed() };
        if unsafe {
            VirtualQuery(
                at as *const c_void,
                &mut information,
                std::mem::size_of::<MemoryInfo>(),
            )
        } == 0
            || information.allocation as usize != base
            || information.state != 0x1000
            || information.protect & 0x101 != 0
            || information.protect & 0xff == 0x10
        {
            return None;
        }
        let limit = (information.base as usize).checked_add(information.size)?;
        if limit <= at {
            return None;
        }
        at = limit.min(end);
    }
    // SAFETY: all bytes are within committed readable regions of this loaded module.
    Some(unsafe { std::slice::from_raw_parts(start as *const u8, length) }.to_vec())
}

fn install_imports() -> Result<(), u32> {
    let base = unsafe { GetModuleHandleW(ptr::null()) } as usize;
    let entries =
        imports::imports(|offset, length| read_memory(base, offset, length)).ok_or(193u32)?;
    let kernel: Vec<_> = "kernel32.dll\0".encode_utf16().collect();
    let kernel = unsafe { GetModuleHandleW(kernel.as_ptr()) };
    let image = module_path(0)?;
    let name = image
        .rsplit(|unit| *unit == 92 || *unit == 47)
        .next()
        .ok_or(193u32)?;
    let node = same_path(name, &"node.exe".encode_utf16().collect::<Vec<_>>());
    let mut patches = Vec::new();
    let mut kinds = 0;
    for entry in entries {
        let (replacement, original, kind) = match entry.name.as_slice() {
            b"CreateProcessW" => (create_process as *const () as usize, &PROCESS, 1),
            b"CreateNamedPipeA" if node => (create_pipe as *const () as usize, &PIPE, 2),
            b"CreateFileA" if node => (create_file as *const () as usize, &FILE, 4),
            b"CreateFileW" if node => (create_wfile as *const () as usize, &WFILE, 8),
            b"GetFinalPathNameByHandleW" if node => (final_path as *const () as usize, &FINAL, 16),
            _ => continue,
        };
        let symbol = CString::new(entry.name).map_err(|_| 193u32)?;
        let expected = unsafe { GetProcAddress(kernel, symbol.as_ptr().cast()) } as usize;
        if expected == 0 || entry.address != expected {
            return Err(193);
        }
        original.store(expected, Ordering::Release);
        kinds |= kind;
        patches.push((base.checked_add(entry.offset).ok_or(193u32)?, replacement));
    }
    if node && kinds != 31 {
        return Err(193);
    }
    // Validate the complete plan before changing any IAT slot. Initialization failure
    // retains the DLL mapping and the launcher kills the suspended target.
    for (address, replacement) in patches {
        let mut old = 0;
        if unsafe { VirtualProtect(address as *mut c_void, 8, 4, &mut old) } == 0 {
            return Err(unsafe { GetLastError() });
        }
        unsafe { (address as *mut usize).write_unaligned(replacement) };
        let mut ignored = 0;
        if unsafe { VirtualProtect(address as *mut c_void, 8, old, &mut ignored) } == 0 {
            return Err(unsafe { GetLastError() });
        }
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn TapidInitialize(parameter: *mut c_void) -> u32 {
    if parameter.is_null() {
        return 0;
    }
    // SAFETY: the launcher supplies a fully written fixed-size allocation in this process.
    let setup = unsafe { &*parameter.cast::<Setup>() };
    if !setup.valid() {
        return 0;
    }
    // Verify the kernel capability boundary, independently of import hooks. A file
    // metadata handle must not duplicate into listing/data, execution, mutation,
    // deletion, ownership or DACL editing authority in the contained recipient.
    for entry in &setup.entries[..setup.count as usize] {
        for access in [1, 2, 4, 0x20, 0x10000, 0x40000, 0x80000] {
            let mut duplicate = 0;
            if unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    entry.handle,
                    GetCurrentProcess(),
                    &mut duplicate,
                    access,
                    0,
                    0,
                )
            } != 0
            {
                unsafe { CloseHandle(duplicate) };
                return 0;
            }
        }
        let path = ordinary_path(&entry.path[..entry.length as usize]);
        if path.len() == 3 && path[1..] == [58, 92] {
            for access in [1, 2, 4, 0x40000, 0x80000] {
                let reopened = unsafe { ReOpenFile(entry.handle, access, 7, 0x02000000) };
                if reopened != -1 {
                    unsafe { CloseHandle(reopened) };
                    return 0;
                }
            }
        }
    }
    if SETUP.set(setup.clone()).is_err() {
        return 0;
    }
    u32::from(install_imports().is_ok())
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(module: Handle, reason: u32, _reserved: *mut c_void) -> i32 {
    // No allocation, import patching, remote operations or lock acquisition under loader lock.
    if reason == 1 {
        SELF.store(module, Ordering::Release);
    }
    1
}

fn translated_pipe(bytes: &[u8]) -> Option<CString> {
    const OLD: &[u8] = b"\\\\?\\pipe\\uv\\";
    if bytes.len() > 1024 || !bytes.starts_with(OLD) {
        return None;
    }
    let mut name = b"\\\\?\\pipe\\LOCAL\\uv\\".to_vec();
    name.extend_from_slice(&bytes[OLD.len()..]);
    CString::new(name).ok()
}
unsafe fn pipe_name(name: *const u8) -> Option<CString> {
    if name.is_null() {
        return None;
    }
    // Native API callers provide a readable NUL-terminated string. Limit inspection to
    // libuv's bounded generated names rather than accepting arbitrary pipe namespaces.
    let mut bytes = Vec::new();
    for index in 0..1025 {
        let byte = unsafe { *name.add(index) };
        if byte == 0 {
            return translated_pipe(&bytes);
        }
        bytes.push(byte);
    }
    None
}
type PipeFn =
    unsafe extern "system" fn(*const u8, u32, u32, u32, u32, u32, u32, *const c_void) -> Handle;
type FileFn =
    unsafe extern "system" fn(*const u8, u32, u32, *const c_void, u32, u32, Handle) -> Handle;
type WFileFn =
    unsafe extern "system" fn(*const u16, u32, u32, *const c_void, u32, u32, Handle) -> Handle;
unsafe extern "system" fn create_pipe(
    name: *const u8,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    e: u32,
    f: u32,
    g: *const c_void,
) -> Handle {
    let original: PipeFn = unsafe { std::mem::transmute(PIPE.load(Ordering::Acquire)) };
    let translated = unsafe { pipe_name(name) };
    let result = unsafe {
        original(
            translated
                .as_ref()
                .map_or(name, |value| value.as_ptr().cast()),
            a,
            b,
            c,
            d,
            e,
            f,
            g,
        )
    };
    let error = unsafe { GetLastError() };
    if result == -1 && translated.is_some() {
        report_failure("create IPC pipe", error);
    }
    drop(translated);
    unsafe { SetLastError(error) };
    result
}
unsafe extern "system" fn create_file(
    name: *const u8,
    a: u32,
    b: u32,
    c: *const c_void,
    d: u32,
    e: u32,
    f: Handle,
) -> Handle {
    let original: FileFn = unsafe { std::mem::transmute(FILE.load(Ordering::Acquire)) };
    let translated = unsafe { pipe_name(name) };
    let result = unsafe {
        original(
            translated
                .as_ref()
                .map_or(name, |value| value.as_ptr().cast()),
            a,
            b,
            c,
            d,
            e,
            f,
        )
    };
    let error = unsafe { GetLastError() };
    if result == -1 && translated.is_some() {
        report_failure("open IPC pipe", error);
    }
    drop(translated);
    unsafe { SetLastError(error) };
    result
}
fn ordinary_path(path: &[u16]) -> &[u16] {
    let path = path.strip_prefix(&[92, 92, 63, 92]).unwrap_or(path);
    if path.len() > 3 && path.last() == Some(&92) {
        &path[..path.len() - 1]
    } else {
        path
    }
}
fn same_path(a: &[u16], b: &[u16]) -> bool {
    let a = ordinary_path(a);
    let b = ordinary_path(b);
    (unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) })
        == 2
}
unsafe extern "system" fn create_wfile(
    name: *const u16,
    access: u32,
    share: u32,
    attributes: *const c_void,
    creation: u32,
    flags: u32,
    template: Handle,
) -> Handle {
    let original: WFileFn = unsafe { std::mem::transmute(WFILE.load(Ordering::Acquire)) };
    if !name.is_null()
        && access == METADATA_ACCESS
        && share == 7
        && creation == 3
        && attributes.is_null()
        && template == 0
        && flags & !0x02200080 == 0
    {
        let mut path = Vec::new();
        for index in 0..MAX_PATH_UNITS {
            let unit = unsafe { *name.add(index) };
            if unit == 0 {
                break;
            }
            path.push(unit);
        }
        if path.len() < MAX_PATH_UNITS
            && let Some(setup) = SETUP.get()
            && let Some(entry) = setup.entries[..setup.count as usize]
                .iter()
                .find(|entry| same_path(&path, &entry.path[..entry.length as usize]))
        {
            let mut duplicate = 0;
            // SAME_ACCESS cannot convert this FILE_READ_ATTRIBUTES-only capability into
            // directory listing, file contents, image mapping, write or ACL-edit authority.
            let succeeded = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    entry.handle,
                    GetCurrentProcess(),
                    &mut duplicate,
                    0,
                    0,
                    2,
                )
            };
            let error = unsafe { GetLastError() };
            drop(path);
            unsafe { SetLastError(error) };
            return if succeeded != 0 { duplicate } else { -1 };
        }
    }
    let result = unsafe { original(name, access, share, attributes, creation, flags, template) };
    let error = unsafe { GetLastError() };
    if result == -1 && !name.is_null() {
        let mut path = Vec::new();
        for index in 0..4 {
            let unit = unsafe { *name.add(index) };
            if unit == 0 {
                break;
            }
            path.push(unit);
        }
        if same_path(&path, &[78, 85, 76]) {
            report_failure("open null stdio", error);
        }
    }
    unsafe { SetLastError(error) };
    result
}

type FinalFn = unsafe extern "system" fn(Handle, *mut u16, u32, u32) -> u32;
unsafe extern "system" fn final_path(
    handle: Handle,
    buffer: *mut u16,
    length: u32,
    flags: u32,
) -> u32 {
    let original: FinalFn = unsafe { std::mem::transmute(FINAL.load(Ordering::Acquire)) };
    let result = unsafe { original(handle, buffer, length, flags) };
    if result != 0 || flags != 0 {
        return result;
    }
    let error = unsafe { GetLastError() };
    if error != 5 {
        return 0;
    }
    let adapted = (|| {
        let setup = SETUP.get()?;
        let mut path = vec![0u16; 32768];
        let path_length =
            unsafe { original(handle, path.as_mut_ptr(), path.len() as u32, 2) } as usize;
        if path_length == 0 || path_length >= path.len() {
            return None;
        }
        for entry in &setup.entries[..setup.count as usize] {
            let drive = ordinary_path(&entry.path[..entry.length as usize]);
            if drive.len() != 3 || drive[1..] != [58, 92] {
                continue;
            }
            let mut root = [0u16; 1024];
            let root_length =
                unsafe { original(entry.handle, root.as_mut_ptr(), root.len() as u32, 2) } as usize;
            if root_length == 0
                || root_length >= root.len()
                || !path[..path_length].starts_with(&root[..root_length])
            {
                continue;
            }
            let mut translated = vec![92, 92, 63, 92, drive[0], 58, 92];
            translated.extend_from_slice(&path[root_length..path_length]);
            if buffer.is_null() || length as usize <= translated.len() {
                return Some((translated.len() + 1) as u32);
            }
            unsafe {
                ptr::copy_nonoverlapping(translated.as_ptr(), buffer, translated.len());
                *buffer.add(translated.len()) = 0;
            }
            return Some(translated.len() as u32);
        }
        None
    })();
    unsafe { SetLastError(error) };
    adapted.unwrap_or(0)
}

fn initialize_child(child: &ProcessInfo) -> Result<(), u32> {
    let module = SELF.load(Ordering::Acquire);
    let path = module_path(module)?;
    let (base, size) = load_component(child.process, &path)?;
    let offset = export_offset(module, c"TapidInitialize", size)?;
    let mut setup = SETUP.get().ok_or(87u32)?.clone();
    for entry in &mut setup.entries[..setup.count as usize] {
        let mut handle = 0;
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                entry.handle,
                child.process,
                &mut handle,
                METADATA_ACCESS,
                0,
                0,
            )
        } == 0
        {
            let error = unsafe { GetLastError() };
            report_failure("duplicate metadata", error);
            return Err(error);
        }
        entry.handle = handle;
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&setup as *const Setup).cast(),
            std::mem::size_of::<Setup>(),
        )
    };
    if remote_call(child.process, base + offset, bytes)? != 1 {
        report_failure("initialize component", 193);
        return Err(193);
    }
    Ok(())
}
type ProcessFn = unsafe extern "system" fn(
    *const u16,
    *mut u16,
    *const c_void,
    *const c_void,
    i32,
    u32,
    *const c_void,
    *const u16,
    *const c_void,
    *mut ProcessInfo,
) -> i32;
unsafe extern "system" fn create_process(
    application: *const u16,
    command: *mut u16,
    process_attributes: *const c_void,
    thread_attributes: *const c_void,
    inherit: i32,
    flags: u32,
    environment: *const c_void,
    cwd: *const u16,
    startup: *const c_void,
    info: *mut ProcessInfo,
) -> i32 {
    let original: ProcessFn = unsafe { std::mem::transmute(PROCESS.load(Ordering::Acquire)) };
    let result = unsafe {
        original(
            application,
            command,
            process_attributes,
            thread_attributes,
            inherit,
            flags | 4,
            environment,
            cwd,
            startup,
            info,
        )
    };
    let error = unsafe { GetLastError() };
    if result == 0 {
        report_failure("create child", error);
        unsafe { SetLastError(error) };
        return result;
    }
    let child = unsafe { &*info };
    if let Err(failure) = initialize_child(child).and_then(|()| {
        if flags & 4 == 0 && unsafe { ResumeThread(child.thread) } == u32::MAX {
            Err(unsafe { GetLastError() })
        } else {
            Ok(())
        }
    }) {
        report_failure("prepare child", failure);
        unsafe {
            TerminateProcess(child.process, 127);
            WaitForSingleObject(child.process, 5000);
            CloseHandle(child.thread);
            CloseHandle(child.process);
            *info = ProcessInfo {
                process: 0,
                thread: 0,
                pid: 0,
                tid: 0,
            };
            SetLastError(failure);
        }
        return 0;
    }
    unsafe { SetLastError(error) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_generated_uv_prefix_is_translated() {
        assert_eq!(
            translated_pipe(br"\\?\pipe\uv\123-abcd")
                .unwrap()
                .to_bytes(),
            br"\\?\pipe\LOCAL\uv\123-abcd"
        );
        for path in [
            br"\\?\pipe\LOCAL\uv\123".as_slice(),
            br"\\?\pipe\host-private",
            br"\\server\pipe\uv\123",
        ] {
            assert!(translated_pipe(path).is_none());
        }
        let mut too_long = br"\\?\pipe\uv\".to_vec();
        too_long.extend([b'a'; 1024]);
        assert!(translated_pipe(&too_long).is_none());
    }
}
