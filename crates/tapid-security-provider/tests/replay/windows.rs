use super::{ReplayStore, request};
use std::{os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
    },
};

fn broaden_acl(path: &Path) {
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let sddl = "D:P(A;;FA;;;WD)"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: descriptor is allocated by the conversion API; both buffers stay
    // live until SetFileSecurityW returns, then LocalFree releases it once.
    unsafe {
        let mut descriptor = null_mut();
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut()
            ),
            0
        );
        let result = SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        assert_ne!(result, 0);
    }
}

fn floor(root: &Path) -> std::path::PathBuf {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "floor"))
        .unwrap()
}

#[test]
fn windows_rejects_broadened_root_and_floor_acls_without_repair() {
    let home = tapid_test_support::TempHome::new("replay-windows-acl").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    store
        .record(
            "issuer",
            &request(),
            3,
            &format!("sha256-{}", "a".repeat(64)),
        )
        .unwrap();
    let path = floor(&root);
    let before = std::fs::read(&path).unwrap();
    broaden_acl(&path);
    assert!(store.load("issuer", &request()).is_err());
    assert!(
        store
            .record(
                "issuer",
                &request(),
                4,
                &format!("sha256-{}", "b".repeat(64))
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    broaden_acl(&root);
    assert!(store.load("issuer", &request()).is_err());
    assert!(ReplayStore::open(&root).is_err());
}

#[test]
fn windows_rejects_corruption_and_pins_root_against_replacement() {
    let home = tapid_test_support::TempHome::new("replay-windows-root").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    assert!(std::fs::rename(&root, home.path().join("saved")).is_err());
    store
        .record(
            "issuer",
            &request(),
            3,
            &format!("sha256-{}", "a".repeat(64)),
        )
        .unwrap();
    let path = floor(&root);
    std::fs::write(&path, b"corrupt").unwrap();
    assert!(store.load("issuer", &request()).is_err());
    assert!(
        store
            .record(
                "issuer",
                &request(),
                4,
                &format!("sha256-{}", "b".repeat(64))
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
}

#[test]
fn windows_rejects_junction_roots() {
    let home = tapid_test_support::TempHome::new("replay-windows-junction").unwrap();
    let target = home.path().join("target");
    drop(ReplayStore::open(&target).unwrap());
    let link = home.path().join("junction");
    assert!(
        std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(ReplayStore::open(&link).is_err());
    std::fs::remove_dir(&link).unwrap();
    assert!(ReplayStore::open(&target).is_ok());
}

#[test]
fn windows_rejects_broadened_lock_acl() {
    let home = tapid_test_support::TempHome::new("replay-windows-lock").unwrap();
    let root = home.path().join("floors");
    let store = ReplayStore::open(&root).unwrap();
    broaden_acl(&root.join(".lock"));
    assert!(store.load("issuer", &request()).is_err());
    assert!(ReplayStore::open(&root).is_err());
}
