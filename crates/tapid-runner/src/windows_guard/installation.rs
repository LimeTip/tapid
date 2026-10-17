//! Administrator-only protected image installation; ordinary status queries never mutate SCM.
use super::{
    PRIVATE_ARG, SERVICE, WindowsNetworkGuardAction as Action, WindowsNetworkGuardStatus as Status,
    wide,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Com::CoTaskMemFree, Services::*},
    UI::Shell::*,
};
const ACL: &str = "O:BAG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";
struct Sc(isize);
impl Drop for Sc {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
fn sc(value: isize) -> Result<Sc, u32> {
    if value == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(Sc(value))
    }
}
struct Descriptor(*mut std::ffi::c_void);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn descriptor() -> Result<Descriptor, u32> {
    let mut p = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(ACL).as_ptr(),
            SDDL_REVISION_1,
            &mut p,
            null_mut(),
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    Ok(Descriptor(p))
}
fn executable() -> Result<PathBuf, u32> {
    let mut p = null_mut();
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT as u32, 0, &mut p) };
    if result < 0 {
        return Err(ERROR_PATH_NOT_FOUND);
    }
    let mut n = 0;
    while n < 32768 && unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    let value = if n == 32768 {
        Err(ERROR_INVALID_DATA)
    } else {
        Ok(PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(p, n)
        }))
        .join("Tapid Network Guard")
        .join("guard-v1.exe"))
    };
    unsafe {
        CoTaskMemFree(p.cast());
    }
    value
}
fn protected(path: &Path) -> Result<(), u32> {
    let attrs = unsafe { GetFileAttributesW(wide(path).as_ptr()) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return Err(unsafe { GetLastError() });
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    let mut needed = 0;
    unsafe {
        GetFileSecurityW(
            wide(path).as_ptr(),
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 || needed > 65536 {
        return Err(ERROR_INVALID_DATA);
    }
    let mut b = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetFileSecurityW(
            wide(path).as_ptr(),
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            b.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    let actual = b.as_mut_ptr().cast();
    let expected = descriptor()?;
    let mut owner = null_mut();
    let mut defaulted = 0;
    if unsafe { GetSecurityDescriptorOwner(actual, &mut owner, &mut defaulted) } == 0
        || owner.is_null()
        || unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid) } == 0
            && unsafe { IsWellKnownSid(owner, WinLocalSystemSid) } == 0
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    let acl = |sd| -> Result<Vec<u8>, u32> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = null_mut();
        if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) } == 0
            || present == 0
            || acl.is_null()
        {
            return Err(ERROR_ACCESS_DENIED);
        }
        Ok(
            unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize) }
                .to_vec(),
        )
    };
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(actual, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || acl(actual)? != acl(expected.0)?
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok(())
}
fn manager(access: u32) -> Result<Sc, u32> {
    sc(unsafe { OpenSCManagerW(null(), null(), access) })
}
fn service(manager: &Sc, access: u32) -> Result<Sc, u32> {
    sc(unsafe { OpenServiceW(manager.0, wide(SERVICE).as_ptr(), access) })
}
fn query(service: &Sc) -> Result<SERVICE_STATUS_PROCESS, u32> {
    let mut info = unsafe { zeroed() };
    let mut needed = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut info as *mut SERVICE_STATUS_PROCESS).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    Ok(info)
}
fn command(path: &Path) -> OsString {
    let mut command = OsString::from("\"");
    command.push(path);
    command.push(format!("\" {PRIVATE_ARG}"));
    command
}
fn verified(service: &Sc, path: &Path) -> Result<(), u32> {
    protected(path.parent().ok_or(ERROR_INVALID_DATA)?)?;
    protected(path)?;
    let mut needed = 0;
    unsafe {
        QueryServiceConfigW(service.0, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 65536 {
        return Err(ERROR_INVALID_DATA);
    }
    let mut b = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe { QueryServiceConfigW(service.0, b.as_mut_ptr().cast(), needed, &mut needed) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    let config = unsafe { &*b.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let mut n = 0;
    while n < 32768 && unsafe { *config.lpBinaryPathName.add(n) } != 0 {
        n += 1;
    }
    if n == 32768
        || OsString::from_wide(unsafe { std::slice::from_raw_parts(config.lpBinaryPathName, n) })
            != command(path)
        || config.dwServiceType != SERVICE_WIN32_OWN_PROCESS
        || config.dwStartType != SERVICE_AUTO_START
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok(())
}
pub(super) fn verified_service_pid() -> Result<u32, u32> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = service(&manager, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
    verified(&service, &executable()?)?;
    let info = query(&service)?;
    if info.dwCurrentState != SERVICE_RUNNING || info.dwProcessId == 0 {
        return Err(ERROR_SERVICE_NOT_ACTIVE);
    }
    Ok(info.dwProcessId)
}
fn wait(service: &Sc, state: u32) -> Result<(), u32> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if query(service)?.dwCurrentState == state {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ERROR_TIMEOUT);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn install(path: &Path) -> Result<(), u32> {
    if unsafe { IsUserAnAdmin() } == 0 {
        return Err(ERROR_ELEVATION_REQUIRED);
    }
    let mut source = source_image(&std::env::current_exe().map_err(|_| ERROR_FILE_NOT_FOUND)?)
        .map_err(|_| ERROR_ACCESS_DENIED)?;
    let mut bytes = Vec::new();
    (&mut source)
        .take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ERROR_READ_FAULT)?;
    if bytes.len() > 512 * 1024 * 1024 {
        return Err(ERROR_FILE_TOO_LARGE);
    }
    let manager = manager(SC_MANAGER_CREATE_SERVICE | SC_MANAGER_CONNECT)?;
    match service(
        &manager,
        SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | SERVICE_START,
    ) {
        Ok(existing) => {
            verified(&existing, path)?;
            if Sha256::digest(fs::read(path).map_err(|_| ERROR_READ_FAULT)?)
                != Sha256::digest(&bytes)
            {
                return Err(ERROR_REVISION_MISMATCH);
            }
            if query(&existing)?.dwCurrentState != SERVICE_RUNNING {
                if unsafe { StartServiceW(existing.0, 0, null()) } == 0 {
                    return Err(unsafe { GetLastError() });
                }
                wait(&existing, SERVICE_RUNNING)?;
            }
            return Ok(());
        }
        Err(ERROR_SERVICE_DOES_NOT_EXIST) => {}
        Err(e) => return Err(e),
    }
    let directory = path.parent().ok_or(ERROR_INVALID_DATA)?;
    let descriptor = descriptor()?;
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(wide(directory).as_ptr(), &security) } == 0
        && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
    {
        return Err(unsafe { GetLastError() });
    }
    protected(directory)?;
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_WRITE | GENERIC_READ,
            FILE_SHARE_READ,
            &security,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE && unsafe { GetLastError() } != ERROR_FILE_EXISTS {
        return Err(unsafe { GetLastError() });
    }
    if handle != INVALID_HANDLE_VALUE {
        let mut image = unsafe { File::from_raw_handle(handle as *mut std::ffi::c_void) };
        image.write_all(&bytes).map_err(|_| ERROR_WRITE_FAULT)?;
        image.sync_all().map_err(|_| ERROR_WRITE_FAULT)?;
        drop(image);
    }
    protected(path)?;
    if Sha256::digest(fs::read(path).map_err(|_| ERROR_READ_FAULT)?) != Sha256::digest(&bytes) {
        return Err(ERROR_CRC);
    }
    let service = sc(unsafe {
        CreateServiceW(
            manager.0,
            wide(SERVICE).as_ptr(),
            wide("Tapid Network Guard").as_ptr(),
            SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            wide(command(path)).as_ptr(),
            null(),
            null_mut(),
            wide("BFE\0").as_ptr(),
            null(),
            null(),
        )
    })?;
    if unsafe { StartServiceW(service.0, 0, null()) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    wait(&service, SERVICE_RUNNING)
}
fn uninstall(path: &Path) -> Result<(), u32> {
    if unsafe { IsUserAnAdmin() } == 0 {
        return Err(ERROR_ELEVATION_REQUIRED);
    }
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = service(
        &manager,
        SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | SERVICE_STOP | DELETE,
    )?;
    verified(&service, path)?;
    if query(&service)?.dwCurrentState == SERVICE_RUNNING {
        let (active, retained) = super::client::Guard::status()?;
        if active != 0 || retained != 0 {
            return Err(ERROR_BUSY);
        }
        let mut state = unsafe { zeroed() };
        if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut state) } == 0 {
            return Err(unsafe { GetLastError() });
        }
        wait(&service, SERVICE_STOPPED)?;
    } else if query(&service)?.dwCurrentState != SERVICE_STOPPED {
        return Err(ERROR_BUSY);
    }
    if super::filtering::Engine::connect()?.count()? != 0 {
        return Err(ERROR_BUSY);
    }
    if unsafe { DeleteService(service.0) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    drop(service);
    fs::remove_file(path).map_err(|_| ERROR_ACCESS_DENIED)?;
    fs::remove_dir(path.parent().unwrap()).map_err(|_| ERROR_ACCESS_DENIED)?;
    Ok(())
}
fn observe(path: PathBuf) -> Result<Status, u32> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = match service(&manager, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS) {
        Ok(s) => s,
        Err(ERROR_SERVICE_DOES_NOT_EXIST) => {
            return Ok(Status {
                installed: false,
                running: false,
                active_executions: 0,
                retained_filters: 0,
                executable: path,
            });
        }
        Err(e) => return Err(e),
    };
    verified(&service, &path)?;
    let running = query(&service)?.dwCurrentState == SERVICE_RUNNING;
    let (active_executions, retained_filters) = if running {
        super::client::Guard::status()?
    } else {
        (0, super::filtering::Engine::connect()?.count()?)
    };
    Ok(Status {
        installed: true,
        running,
        active_executions,
        retained_filters,
        executable: path,
    })
}
pub(super) fn manage(action: Action) -> Result<Status, String> {
    let operation = || -> Result<Status, u32> {
        let path = executable()?;
        match action {
            Action::Install => install(&path)?,
            Action::Uninstall => uninstall(&path)?,
            Action::Status => {}
        }
        observe(path)
    };
    operation().map_err(|e|format!("Windows network guard management failed ({e}): {}",if e==ERROR_ELEVATION_REQUIRED{"run 'tapid sandbox install' or 'tapid sandbox uninstall' once from an administrator terminal"}else if e==ERROR_REVISION_MISMATCH{"installed guard differs from this build; uninstall the idle guard before installing the new build"}else if e==ERROR_BUSY{"active executions or retained denial filters prevent removal"}else{"inspect the protected service configuration and Windows Base Filtering Engine"}))
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: isize,
        class: u32,
        input: *mut std::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}
// ProcessImageFileMapping is an input HANDLE comparison against the mapped executable.
// Unsupported native queries reject setup, never fall back to trusting a pathname.
fn source_image(path: &Path) -> Result<File, u32> {
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_READ | FILE_EXECUTE | SYNCHRONIZE,
            FILE_SHARE_READ,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(unsafe { GetLastError() });
    }
    let file = unsafe { File::from_raw_handle(handle as *mut std::ffi::c_void) };
    let mut handle = file.as_raw_handle() as isize;
    if unsafe {
        NtQueryInformationProcess(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            44,
            (&mut handle as *mut isize).cast(),
            size_of::<isize>() as u32,
            null_mut(),
        )
    } != 0
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok(file)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_installer_binds_source_to_running_image_and_rejects_copy() {
        let source = source_image(&std::env::current_exe().unwrap()).unwrap();
        assert!(source.metadata().unwrap().len() > 0);
        let project = tapid_test_support::TempProject::new("guard-image-control").unwrap();
        let copy = project.path().join("copy.exe");
        fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        assert!(
            source_image(&copy).is_err(),
            "equal bytes on a different file object must be rejected"
        );
    }
}
