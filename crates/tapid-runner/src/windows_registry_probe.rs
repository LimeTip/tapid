//! Disposable host-hive input characterization, not a registry closure proof.
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{Authorization::ConvertSidToStringSidW, TOKEN_QUERY, TOKEN_USER, TokenUser},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

const USERS: isize = -2147483645;
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(
        key: isize,
        path: *const u16,
        reserved: u32,
        class: *mut u16,
        options: u32,
        access: u32,
        security: *const std::ffi::c_void,
        result: *mut isize,
        disposition: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        key: isize,
        name: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        size: u32,
    ) -> i32;
    fn RegOpenKeyExW(
        key: isize,
        path: *const u16,
        options: u32,
        access: u32,
        result: *mut isize,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        size: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: isize) -> i32;
    fn RegDeleteKeyW(key: isize, path: *const u16) -> i32;
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

pub struct RegistryInput {
    pub path: String,
    active: bool,
}
impl RegistryInput {
    pub fn new(unique: &str) -> Self {
        let mut token = 0;
        // SAFETY: output handle is closed before interpreting the owned token buffer.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
            0
        );
        let information = super::super::token_information(token, TokenUser);
        unsafe { CloseHandle(token) };
        let information = information.unwrap();
        let mut sid_text = std::ptr::null_mut();
        // SAFETY: aligned token buffer holds a live SID; conversion allocates a terminated string.
        assert_ne!(
            unsafe {
                ConvertSidToStringSidW(
                    (*information.as_ptr().cast::<TOKEN_USER>()).User.Sid,
                    &mut sid_text,
                )
            },
            0
        );
        let mut length = 0;
        unsafe {
            while *sid_text.add(length) != 0 {
                length += 1;
            }
        }
        let sid =
            unsafe { String::from_utf16(std::slice::from_raw_parts(sid_text, length)).unwrap() };
        unsafe { LocalFree(sid_text as _) };
        assert!(!unique.contains(['\\', '/']));
        let path = format!("{sid}\\Software\\TapidAuthority-{unique}");
        let mut key = 0;
        let mut disposition = 0;
        // SAFETY: exact disposable key in the current user's loaded hive; no inherited handles.
        assert_eq!(
            unsafe {
                RegCreateKeyExW(
                    USERS,
                    wide(&path).as_ptr(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    3,
                    std::ptr::null(),
                    &mut key,
                    &mut disposition,
                )
            },
            0
        );
        let fixture = Self {
            path,
            active: disposition == 1,
        };
        if disposition != 1 {
            unsafe { RegCloseKey(key) };
            panic!("never modify a preexisting key");
        }
        let result = unsafe {
            RegSetValueExW(
                key,
                wide("Input").as_ptr(),
                0,
                4,
                0x273u32.to_le_bytes().as_ptr(),
                4,
            )
        };
        unsafe { RegCloseKey(key) };
        assert_eq!(result, 0);
        assert_eq!(read(&fixture.path), Ok(0x273));
        fixture
    }
    pub fn cleanup(&mut self) {
        assert_eq!(
            unsafe { RegDeleteKeyW(USERS, wide(&self.path).as_ptr()) },
            0
        );
        self.active = false;
        assert_eq!(read(&self.path), Err(2), "exact disposable key removed");
    }
}
impl Drop for RegistryInput {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: only remove the exact key this fixture created, never a parent/tree.
            unsafe { RegDeleteKeyW(USERS, wide(&self.path).as_ptr()) };
        }
    }
}
fn read(path: &str) -> Result<u32, i32> {
    let mut key = 0;
    // SAFETY: terminated path, query-only access, output handle closed on every query result.
    let status = unsafe { RegOpenKeyExW(USERS, wide(path).as_ptr(), 0, 1, &mut key) };
    if status != 0 {
        return Err(status);
    }
    let mut value = [0u8; 4];
    let mut size = 4;
    let mut kind = 0;
    let status = unsafe {
        RegQueryValueExW(
            key,
            wide("Input").as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            value.as_mut_ptr(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    if status != 0 {
        return Err(status);
    }
    assert_eq!((kind, size), (4, 4));
    Ok(u32::from_le_bytes(value))
}
pub fn probe() {
    let Some(path) = std::env::var_os("TAPID_TEST_REGISTRY") else {
        return;
    };
    match read(&path.to_string_lossy()) {
        Ok(value) => {
            assert_eq!(value, 0x273);
            println!("registry:host-input:readable");
        }
        Err(5) => println!("registry:host-input:access-denied"),
        Err(2 | 3) => println!("registry:host-input:unavailable"),
        Err(error) => panic!("unexpected registry input failure: {error}"),
    }
}
