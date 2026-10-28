//! A deliberately tiny XPC transport experiment, not an execution backend.
use std::{
    ffi::{CStr, CString, c_char, c_void},
    io,
    path::Path,
};

// The research bundle owns this dylib and stays loaded until process exit.
fn symbol(name: &CStr) -> io::Result<*mut c_void> {
    let path = std::env::current_exe()?
        .parent()
        .ok_or_else(|| io::Error::other("missing executable parent"))?
        .join("libprobe.dylib");
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        return Err(io::Error::other(
            "research bridge unavailable; package the bundle first",
        ));
    }
    let pointer = unsafe { libc::dlsym(library, name.as_ptr()) };
    if pointer.is_null() {
        return Err(io::Error::other("research bridge symbol unavailable"));
    }
    Ok(pointer)
}

extern "C" fn control(outside: *const c_char) -> i64 {
    // Never unwind across the SDK callback. Bits describe observations only:
    // 1 acknowledgement, 2 generated-file control, 4 denied read, 8 denied write.
    std::panic::catch_unwind(|| {
        let path = unsafe { CStr::from_ptr(outside) };
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
        let mut bits = 1;
        if let Ok(root) = tapid_test_support::TempProject::new("xpc-generated") {
            let generated = root.path().join("generated");
            if std::fs::write(&generated, b"generated").is_ok()
                && std::fs::read(&generated).is_ok_and(|v| v == b"generated")
            {
                bits |= 2;
            }
        }
        if std::fs::read(path).is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied) {
            bits |= 4;
        }
        if std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied)
        {
            bits |= 8;
        }
        bits
    })
    .unwrap_or(-3)
}

pub fn service() -> io::Result<()> {
    let pointer = symbol(c"probe_service_main")?;
    let entry: unsafe extern "C" fn(extern "C" fn(*const c_char) -> i64) =
        unsafe { std::mem::transmute(pointer) };
    unsafe { entry(control) };
    Err(io::Error::other("xpc_main unexpectedly returned"))
}

pub fn client() -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let root = tapid_test_support::TempProject::new("xpc-outside")?;
    let outside = root.path().join("synthetic-only");
    std::fs::write(&outside, b"synthetic-secret")?;
    // Paired native positive control with the same existing regular file.
    assert_eq!(std::fs::read(&outside)?, b"synthetic-secret");
    std::fs::OpenOptions::new().write(true).open(&outside)?;
    let path = CString::new(outside.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let pointer = symbol(c"probe_client")?;
    let entry: unsafe extern "C" fn(*const c_char) -> i64 = unsafe { std::mem::transmute(pointer) };
    let result = unsafe { entry(path.as_ptr()) };
    println!(
        "candidate=app-sandbox-xpc control_bits={result} expected=15 authoritative_boundary=none lifecycle=unverified accounting=unverified"
    );
    if result != 15 {
        return Err(io::Error::other(
            "XPC setup or paired authority controls incomplete; no enforcement verdict",
        ));
    }
    Ok(())
}

pub fn transport_controls() -> io::Result<()> {
    let pointer = symbol(c"probe_transport_controls")?;
    let entry: unsafe extern "C" fn(*mut i64) = unsafe { std::mem::transmute(pointer) };
    let mut observed = [0i64; 4];
    unsafe { entry(observed.as_mut_ptr()) };
    println!(
        "xpc_transport_controls={observed:?} expected=[15,-1,-2,-2] candidate_enforcement=none"
    );
    if observed != [15, -1, -2, -2] {
        return Err(io::Error::other("XPC transport controls incomplete"));
    }
    Ok(())
}

pub fn missing_service() -> io::Result<()> {
    let pointer = symbol(c"probe_missing_service")?;
    let entry: unsafe extern "C" fn() -> i64 = unsafe { std::mem::transmute(pointer) };
    let result = unsafe { entry() };
    println!(
        "xpc_missing_service={result} authority_payload_dispatched=false candidate_enforcement=none"
    );
    if result != -1 {
        return Err(io::Error::other(
            "missing service did not return an explicit transport error",
        ));
    }
    Ok(())
}
