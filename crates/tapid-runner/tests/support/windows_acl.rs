// Test-only initialization of newly created, disposable Windows ACL fixtures.
// Never call this on shared ancestors or production paths.

pub fn read_acl(path: &std::path::Path) -> (u16, Vec<u8>) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl};
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: path is NUL-terminated and all output pointers are writable.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(status, 0, "query exact DACL");
    assert!(!dacl.is_null(), "fixture must have a real DACL");
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: GetNamedSecurityInfoW owns a valid descriptor and ACL until LocalFree.
    let (control_status, bytes) = unsafe {
        let control_status = GetSecurityDescriptorControl(descriptor, &mut control, &mut revision);
        let bytes =
            std::slice::from_raw_parts(dacl.cast::<u8>(), (*dacl).AclSize as usize).to_vec();
        LocalFree(descriptor);
        (control_status, bytes)
    };
    assert_ne!(control_status, 0, "query DACL control bits");
    (control, bytes)
}

pub fn initialize_inheritance(root: &std::path::Path) {
    let initial = read_acl(root);
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        };
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let wide: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: root is a newly created fixture; the output pointers are writable.
        let queried = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(queried, 0, "query fixture inheritance DACL");
        assert!(!dacl.is_null(), "fixture must have a real DACL");
        // SAFETY: reapply the existing DACL without changing its ACEs or protection;
        // Windows propagates its current inheritance model within this fixture only.
        let applied = unsafe {
            let applied = SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            );
            LocalFree(descriptor);
            applied
        };
        assert_eq!(applied, 0, "initialize fixture inheritance model");
    }
    let initialized = read_acl(root);
    assert!(
        initialized.0 == initial.0
            || initialized.0 == initial.0 | windows_sys::Win32::Security::SE_DACL_AUTO_INHERITED,
        "fixture setup may only set the auto-inherited control bit"
    );
    // Windows can convert matching explicit ACEs to inherited ACEs on the root.
    // Permit only that one-way bookkeeping transition during SETUP, not cleanup.
    assert_eq!(
        initialized.1.len(),
        initial.1.len(),
        "fixture ACL size must not change"
    );
    let mut expected = initial.1.clone();
    let mut offset = std::mem::size_of::<windows_sys::Win32::Security::ACL>();
    while offset < expected.len() {
        assert!(offset + 4 <= expected.len(), "valid ACE header");
        let size = u16::from_le_bytes([expected[offset + 2], expected[offset + 3]]) as usize;
        assert!(
            size >= 4 && offset + size <= expected.len(),
            "valid ACE size"
        );
        let inherited = windows_sys::Win32::Security::INHERITED_ACE as u8;
        let old_flags = expected[offset + 1];
        let new_flags = initialized.1[offset + 1];
        assert!(
            new_flags == old_flags || new_flags == old_flags | inherited,
            "fixture setup may only mark an existing ACE inherited"
        );
        expected[offset + 1] = new_flags;
        offset += size;
    }
    assert_eq!(
        initialized.1, expected,
        "fixture setup must preserve every other ACL byte"
    );
}
