#![cfg(windows)]
use super::*;
use crate::execution::platform_backend::trace_windows_stage;
use std::sync::{Mutex, MutexGuard};

// Windows exposes no compare-and-swap DACL update; serialize Tapid's ACL transactions within this process.
static WINDOWS_ACL_MUTATION_LOCK: Mutex<()> = Mutex::new(());

struct AclMutationGuard {
    _process_guard: MutexGuard<'static, ()>,
    named_mutex: HANDLE,
}

impl Drop for AclMutationGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;

        unsafe {
            ReleaseMutex(self.named_mutex);
            CloseHandle(self.named_mutex);
        }
    }
}

fn lock_acl_mutations() -> Result<AclMutationGuard, ExecutionError> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

    let process_guard = WINDOWS_ACL_MUTATION_LOCK
        .lock()
        .map_err(|_| unsupported_acl("serialize filesystem ACL updates", 6))?;
    let name: Vec<u16> = "Global\\TapidWindowsAclMutation-v1\0"
        .encode_utf16()
        .collect();
    let named_mutex = unsafe { CreateMutexW(null_mut(), 0, name.as_ptr()) };
    if named_mutex == 0 {
        return Err(unsupported_acl(
            "create cross-process filesystem ACL lock",
            unsafe { GetLastError() },
        ));
    }
    let wait = unsafe { WaitForSingleObject(named_mutex, 30_000) };
    match wait {
        WAIT_OBJECT_0 => Ok(AclMutationGuard {
            _process_guard: process_guard,
            named_mutex,
        }),
        WAIT_ABANDONED => {
            unsafe {
                windows_sys::Win32::System::Threading::ReleaseMutex(named_mutex);
                CloseHandle(named_mutex);
            }
            Err(unsupported_acl("recover abandoned filesystem ACL lock", 6))
        }
        WAIT_TIMEOUT => {
            unsafe { CloseHandle(named_mutex) };
            Err(unsupported_acl("wait for filesystem ACL lock", 1460))
        }
        _ => {
            let error = unsafe { GetLastError() };
            unsafe { CloseHandle(named_mutex) };
            Err(unsupported_acl("wait for filesystem ACL lock", error))
        }
    }
}

/// Temporarily grants an AppContainer SID access to a directory subtree and restores the
/// original DACL before releasing the held directory handle.
pub struct WindowsPathAcl {
    handle: HANDLE,
    security_descriptor: *mut c_void,
    restored: bool,
    changed: bool,
    appcontainer_sid: Box<[u32]>,
}

impl WindowsPathAcl {
    pub fn grant(
        path: &std::path::Path,
        sid: windows_sys::Win32::Foundation::PSID,
        access: FilesystemAccess,
        kind: FilesystemGrantKind,
    ) -> Result<Self, ExecutionError> {
        Self::grant_with_execute(path, sid, access, kind, false)
    }

    fn grant_executable(
        path: &std::path::Path,
        sid: windows_sys::Win32::Foundation::PSID,
        access: FilesystemAccess,
        kind: FilesystemGrantKind,
    ) -> Result<Self, ExecutionError> {
        Self::grant_with_execute(path, sid, access, kind, true)
    }

    fn grant_with_execute(
        path: &std::path::Path,
        sid: windows_sys::Win32::Foundation::PSID,
        access: FilesystemAccess,
        kind: FilesystemGrantKind,
        allow_execute: bool,
    ) -> Result<Self, ExecutionError> {
        let _transaction = lock_acl_mutations()?;
        // Existing AppContainer traversal semantics suffice for declared targets; inaccessible
        // targets remain denied. Do not add implicit ancestor traversal ACEs: SetSecurityInfo
        // reapplies all existing inheritable ACEs throughout the descendant tree even when our
        // new ACE is non-inheriting. On shared runtime/project ancestors this can traverse the
        // entire runner workspace on grant and cleanup. Restrict every DACL mutation to the
        // explicitly declared target instead.
        // https://learn.microsoft.com/en-us/windows/win32/secauthz/automatic-propagation-of-inheritable-aces
        trace_windows_stage("acl: target grant start");
        let grant = Self::grant_inner(path, sid, access, kind, allow_execute)?;
        trace_windows_stage("acl: target grant complete");
        Ok(grant)
    }

    fn grant_inner(
        path: &std::path::Path,
        sid: windows_sys::Win32::Foundation::PSID,
        access: FilesystemAccess,
        kind: FilesystemGrantKind,
        allow_execute: bool,
    ) -> Result<Self, ExecutionError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
            SetEntriesInAclW, SetSecurityInfo, TRUSTEE_FORM, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
            TRUSTEE_W,
        };
        use windows_sys::Win32::Security::{
            ACE_FLAGS, ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
            UNPROTECTED_DACL_SECURITY_INFORMATION,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_EXECUTE,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY,
            FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_READ_EA, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_EA, OPEN_EXISTING, READ_CONTROL,
            WRITE_DAC,
        };

        if sid.is_null() {
            return Err(unsupported_acl(
                "grant filesystem access to an invalid AppContainer SID",
                87,
            ));
        }
        let appcontainer_sid = own_sid(sid)?;
        let metadata = std::fs::metadata(path).map_err(|error| {
            unsupported_acl(
                "inspect filesystem grant target",
                error.raw_os_error().unwrap_or(1) as u32,
            )
        })?;
        let is_directory = metadata.is_dir();
        let subtree = kind == FilesystemGrantKind::DirectorySubtree;
        let kind_matches = match kind {
            FilesystemGrantKind::ExactFile => metadata.is_file(),
            FilesystemGrantKind::ExactDirectory | FilesystemGrantKind::DirectorySubtree => {
                is_directory
            }
            FilesystemGrantKind::CharacterDevice => false,
        };
        if !kind_matches {
            return Err(unsupported_acl(
                "filesystem grant target kind is unsupported or changed",
                87,
            ));
        }
        let display_path = path.display().to_string();
        let path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: path is NUL-terminated. The held handle anchors the object while its DACL is
        // changed and restored; backup semantics permits opening a directory.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsupported_acl(
                &format!("open filesystem grant for DACL update at {display_path}"),
                std::io::Error::last_os_error().raw_os_error().unwrap_or(1) as u32,
            ));
        }

        let mut original_dacl: *mut ACL = null_mut();
        let mut security_descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: handle is valid and all output pointers are writable.
        let queried = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut original_dacl,
                null_mut(),
                &mut security_descriptor,
            )
        };
        if queried != 0 || security_descriptor.is_null() {
            if !security_descriptor.is_null() {
                // SAFETY: GetSecurityInfo allocates this descriptor with LocalAlloc.
                unsafe { LocalFree(security_descriptor.cast()) };
            }
            // SAFETY: this wrapper has not yet been constructed and owns the handle.
            unsafe { CloseHandle(handle) };
            return Err(unsupported_acl(
                "read filesystem grant DACL",
                queried.max(1),
            ));
        }

        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: descriptor came from GetSecurityInfo and the output fields are writable.
        let control_read = unsafe {
            GetSecurityDescriptorControl(security_descriptor, &mut control, &mut revision)
        };
        if control_read == 0 {
            // SAFETY: release both allocations acquired above.
            unsafe {
                LocalFree(security_descriptor.cast());
                CloseHandle(handle);
            }
            return Err(unsupported_acl("inspect filesystem DACL inheritance", 87));
        }
        let original_protected = control & SE_DACL_PROTECTED != 0;
        if subtree && original_protected {
            // A protected DACL cannot propagate the requested subtree ACE. Refuse instead of
            // silently granting only the directory itself.
            unsafe {
                LocalFree(security_descriptor.cast());
                CloseHandle(handle);
            }
            return Err(unsupported_acl(
                "filesystem subtree has a protected DACL",
                5,
            ));
        }

        // A NULL DACL already grants access to everyone. Preserve it rather than replacing it
        // with a restrictive DACL merely to add an AppContainer ACE.
        if original_dacl.is_null() {
            return Ok(Self {
                handle,
                security_descriptor: security_descriptor.cast(),
                restored: false,
                changed: false,
                appcontainer_sid,
            });
        }

        let root_permissions = match access {
            FilesystemAccess::ReadData => {
                if is_directory {
                    FILE_LIST_DIRECTORY
                } else {
                    FILE_READ_DATA
                }
            }
            FilesystemAccess::ReadMetadata => FILE_READ_ATTRIBUTES | FILE_READ_EA,
            FilesystemAccess::Read => FILE_GENERIC_READ,
            FilesystemAccess::Write if is_directory && subtree => {
                FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_WRITE_ATTRIBUTES | FILE_WRITE_EA
            }
            FilesystemAccess::Write if is_directory => FILE_WRITE_ATTRIBUTES | FILE_WRITE_EA,
            FilesystemAccess::Write => FILE_GENERIC_WRITE,
        };
        let root_permissions = if allow_execute {
            root_permissions | FILE_EXECUTE
        } else {
            root_permissions
        };
        let inheritance = if subtree {
            (windows_sys::Win32::Security::OBJECT_INHERIT_ACE
                | windows_sys::Win32::Security::CONTAINER_INHERIT_ACE) as ACE_FLAGS
        } else {
            0
        };
        let make_entry = |permissions, inheritance| EXPLICIT_ACCESS_W {
            grfAccessPermissions: permissions,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: inheritance,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID as TRUSTEE_FORM,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.cast(),
            },
        };
        let mut entries = Vec::with_capacity(4);
        if access == FilesystemAccess::Write && is_directory && subtree {
            entries.push(make_entry(root_permissions, 0));
            // Internal rename/unlink requires DELETE on descendants, never on the declared
            // root. Do not grant FILE_DELETE_CHILD (which could override child denial),
            // WRITE_DAC, ownership, or parent-entry replacement for ExactFile grants.
            entries.push(make_entry(
                FILE_GENERIC_WRITE | windows_sys::Win32::Storage::FileSystem::DELETE,
                inheritance | windows_sys::Win32::Security::INHERIT_ONLY_ACE,
            ));
        } else {
            entries.push(make_entry(root_permissions, inheritance));
        }
        if is_directory && !allow_execute {
            // AppContainer tokens may not have SeChangeNotifyPrivilege. Grant traversal on the
            // granted directory itself and descendant directories, without granting execute on
            // descendant files.
            entries.push(make_entry(FILE_EXECUTE, 0));
            if subtree {
                entries.push(make_entry(
                    FILE_EXECUTE,
                    windows_sys::Win32::Security::CONTAINER_INHERIT_ACE
                        | windows_sys::Win32::Security::INHERIT_ONLY_ACE,
                ));
            }
        }
        let entry_count = entries.len() as u32;
        let mut new_dacl: *mut ACL = null_mut();
        // SAFETY: entries reference the live AppContainer SID; original_dacl remains owned by the
        // security descriptor until the merged ACL has been attached to the object.
        let merged = unsafe {
            SetEntriesInAclW(entry_count, entries.as_ptr(), original_dacl, &mut new_dacl)
        };
        if merged != 0 || new_dacl.is_null() {
            unsafe {
                if !new_dacl.is_null() {
                    LocalFree(new_dacl.cast());
                }
                LocalFree(security_descriptor.cast());
                CloseHandle(handle);
            }
            return Err(unsupported_acl(
                "add AppContainer filesystem ACE",
                merged.max(1),
            ));
        }

        let protection = if original_protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        // SAFETY: the target handle is held, the merged ACL is valid, and DACL protection state
        // is explicitly preserved; inheritable ACEs propagate only for directory-subtree grants.
        let applied = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | protection,
                null_mut(),
                null_mut(),
                new_dacl,
                null_mut(),
            )
        };
        unsafe { LocalFree(new_dacl.cast()) };
        if applied != 0 {
            unsafe {
                LocalFree(security_descriptor.cast());
                CloseHandle(handle);
            }
            return Err(unsupported_acl(
                "apply AppContainer filesystem ACE",
                applied,
            ));
        }

        Ok(Self {
            handle,
            security_descriptor: security_descriptor.cast(),
            restored: false,
            changed: true,
            appcontainer_sid,
        })
    }

    pub fn restore(&mut self) -> Result<(), ExecutionError> {
        let _transaction = lock_acl_mutations()?;
        self.restore_unlocked()
    }

    fn restore_unlocked(&mut self) -> Result<(), ExecutionError> {
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            EXPLICIT_ACCESS_W, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT,
            SetEntriesInAclW, SetSecurityInfo, TRUSTEE_FORM, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
            TRUSTEE_W,
        };
        use windows_sys::Win32::Security::{
            ACE_FLAGS, ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
            UNPROTECTED_DACL_SECURITY_INFORMATION,
        };

        let mut restore_error = None;
        if self.changed {
            let mut current_dacl: *mut ACL = null_mut();
            let mut current_descriptor: PSECURITY_DESCRIPTOR = null_mut();
            // SAFETY: the held handle anchors the object and both outputs are writable.
            let queried = unsafe {
                GetSecurityInfo(
                    self.handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    &mut current_dacl,
                    null_mut(),
                    &mut current_descriptor,
                )
            };
            if queried != 0 || current_descriptor.is_null() {
                restore_error = Some(unsupported_acl(
                    "read current filesystem DACL for grant removal",
                    queried.max(1),
                ));
            } else if current_dacl.is_null() {
                // Another actor replaced the DACL with a NULL DACL (allow all). Preserve that
                // concurrent change rather than restoring an obsolete, more restrictive snapshot.
                self.changed = false;
            } else {
                let mut control = 0u16;
                let mut revision = 0u32;
                // SAFETY: current_descriptor came from GetSecurityInfo.
                let control_read = unsafe {
                    GetSecurityDescriptorControl(current_descriptor, &mut control, &mut revision)
                };
                if control_read == 0 {
                    restore_error = Some(unsupported_acl(
                        "inspect current filesystem DACL inheritance",
                        87,
                    ));
                } else {
                    let entry = EXPLICIT_ACCESS_W {
                        grfAccessPermissions: 0,
                        grfAccessMode: REVOKE_ACCESS,
                        grfInheritance: 0 as ACE_FLAGS,
                        Trustee: TRUSTEE_W {
                            pMultipleTrustee: null_mut(),
                            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                            TrusteeForm: TRUSTEE_IS_SID as TRUSTEE_FORM,
                            TrusteeType: TRUSTEE_IS_UNKNOWN,
                            ptstrName: self.appcontainer_sid.as_ptr().cast_mut().cast(),
                        },
                    };
                    let mut updated_dacl: *mut ACL = null_mut();
                    // REVOKE_ACCESS removes only this execution's unique AppContainer SID and
                    // carries every current ACE for other trustees forward.
                    // SAFETY: entry points to the owned SID; current_dacl remains valid until the
                    // returned ACL is attached or freed.
                    let removed =
                        unsafe { SetEntriesInAclW(1, &entry, current_dacl, &mut updated_dacl) };
                    if removed != 0 || updated_dacl.is_null() {
                        restore_error = Some(unsupported_acl(
                            "remove AppContainer filesystem ACE",
                            removed.max(1),
                        ));
                    } else {
                        let protection = if control & SE_DACL_PROTECTED != 0 {
                            PROTECTED_DACL_SECURITY_INFORMATION
                        } else {
                            UNPROTECTED_DACL_SECURITY_INFORMATION
                        };
                        // SAFETY: updated_dacl is valid and the current inheritance mode is kept.
                        trace_windows_stage("acl: DACL restoration start");
                        let restored = unsafe {
                            SetSecurityInfo(
                                self.handle,
                                SE_FILE_OBJECT,
                                DACL_SECURITY_INFORMATION | protection,
                                null_mut(),
                                null_mut(),
                                updated_dacl,
                                null_mut(),
                            )
                        };
                        trace_windows_stage("acl: DACL restoration complete");
                        if restored != 0 {
                            restore_error = Some(unsupported_acl(
                                "restore filesystem DACL without Tapid ACE",
                                restored,
                            ));
                        } else {
                            self.changed = false;
                        }
                        // SAFETY: SetSecurityInfo copies the ACL; the local allocation is no
                        // longer needed.
                        unsafe { LocalFree(updated_dacl.cast()) };
                    }
                }
            }
            if !current_descriptor.is_null() {
                // SAFETY: GetSecurityInfo allocates this descriptor with LocalAlloc.
                unsafe { LocalFree(current_descriptor.cast()) };
            }
        }
        if !self.changed {
            unsafe {
                if !self.security_descriptor.is_null() {
                    LocalFree(self.security_descriptor);
                    self.security_descriptor = null_mut();
                }
                if self.handle != 0 {
                    CloseHandle(self.handle);
                    self.handle = 0;
                }
            }
        }
        if let Some(error) = restore_error {
            return Err(error);
        }
        self.restored = true;
        Ok(())
    }
}

fn runtime_path_wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn runtime_dacl_is_writable(path: &std::path::Path) -> Result<bool, ExecutionError> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::*;
    let wide = runtime_path_wide(path);
    // SAFETY: owned NUL-terminated path; no DACL mutation occurs here.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(handle) };
        return Ok(true);
    }
    let error = unsafe { GetLastError() };
    if error == windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED {
        Ok(false)
    } else {
        Err(unsupported_acl("inspect runtime DACL authority", error))
    }
}

fn verify_existing_runtime_access(
    container: &WindowsAppContainer,
    grant: &ResolvedFilesystemGrant,
) -> Result<(), ExecutionError> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_TOKEN, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, OpenProcessToken, OpenThreadToken,
    };

    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    struct Impersonation;
    impl Drop for Impersonation {
        fn drop(&mut self) {
            // A failed revert would leave the worker in a different security context. Microsoft
            // requires terminating the process rather than continuing after this failure.
            if unsafe { RevertToSelf() } == 0 {
                std::process::abort();
            }
        }
    }

    // Do not overwrite a caller's existing impersonation context.
    let mut prior = 0;
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut prior) } != 0 {
        unsafe { CloseHandle(prior) };
        return Err(unsupported_acl(
            "runtime access check on an impersonating thread",
            5,
        ));
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_NO_TOKEN {
        return Err(unsupported_acl(
            "inspect runtime access-check thread",
            error,
        ));
    }
    let system_root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| unsupported_acl("locate runtime access-check executable", 2))?;
    let application = runtime_path_wide(&system_root.join("System32/cmd.exe"));
    // This verified AppContainer process is never resumed. It supplies the same no-capability
    // token construction as the real launch, including traditional and restricted principals.
    // Its owner terminates/reaps it on every exit. No root or SystemRoot DACL is modified.
    let environment =
        crate::execution::windows_environment_block_units(&std::collections::BTreeMap::new())?;
    let child = WindowsSuspendedChild::create(
        container,
        &application,
        &application,
        &environment,
        &runtime_path_wide(&system_root),
    )?;
    let mut primary = 0;
    if unsafe {
        OpenProcessToken(
            child.process_handle(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut primary,
        )
    } == 0
    {
        return Err(unsupported_acl(
            "open verified runtime access-check token",
            unsafe { GetLastError() },
        ));
    }
    let primary = Token(primary);
    let mut duplicate = 0;
    if unsafe {
        DuplicateTokenEx(
            primary.0,
            TOKEN_QUERY | TOKEN_IMPERSONATE,
            null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut duplicate,
        )
    } == 0
    {
        return Err(unsupported_acl(
            "duplicate runtime access-check token",
            unsafe { GetLastError() },
        ));
    }
    let duplicate = Token(duplicate);
    if unsafe { ImpersonateLoggedOnUser(duplicate.0) } == 0 {
        return Err(unsupported_acl(
            "impersonate runtime access-check token",
            unsafe { GetLastError() },
        ));
    }
    let _impersonation = Impersonation;
    let mut pending = vec![grant.path.clone()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            unsupported_acl(
                "inspect immutable runtime target",
                error.raw_os_error().unwrap_or(1) as u32,
            )
        })?;
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(unsupported_acl(
                "immutable runtime contains a reparse point",
                5,
            ));
        }
        if path == grant.path
            && !match grant.kind() {
                FilesystemGrantKind::ExactFile => metadata.is_file(),
                FilesystemGrantKind::ExactDirectory | FilesystemGrantKind::DirectorySubtree => {
                    metadata.is_dir()
                }
                FilesystemGrantKind::CharacterDevice => false,
            }
        {
            return Err(unsupported_acl("immutable runtime target kind changed", 87));
        }
        let desired = match grant.access() {
            FilesystemAccess::Read => FILE_GENERIC_READ | FILE_EXECUTE,
            FilesystemAccess::ReadData => FILE_READ_DATA,
            FilesystemAccess::ReadMetadata => FILE_READ_ATTRIBUTES | FILE_READ_EA,
            FilesystemAccess::Write => {
                return Err(unsupported_acl("immutable runtime write check", 5));
            }
        } | if metadata.is_dir() { FILE_EXECUTE } else { 0 };
        let wide = runtime_path_wide(&path);
        // A real kernel open includes deny ACE ordering, both AppContainer access checks,
        // traversal, and mandatory integrity policy. ACE-presence/host-token checks do not.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                desired,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsupported_acl(
                "verify existing AppContainer runtime access",
                unsafe { GetLastError() },
            ));
        }
        unsafe { CloseHandle(handle) };
        if metadata.is_dir() && grant.kind() == FilesystemGrantKind::DirectorySubtree {
            for entry in std::fs::read_dir(&path).map_err(|error| {
                unsupported_acl(
                    "enumerate immutable runtime subtree",
                    error.raw_os_error().unwrap_or(1) as u32,
                )
            })? {
                pending.push(
                    entry
                        .map_err(|error| {
                            unsupported_acl(
                                "inspect immutable runtime descendant",
                                error.raw_os_error().unwrap_or(1) as u32,
                            )
                        })?
                        .path(),
                );
            }
        }
    }
    Ok(())
}

/// Owns the policy-derived ACL changes for one AppContainer and restores them on every exit path.
pub struct WindowsFilesystemGrants {
    grants: Vec<WindowsPathAcl>,
}

impl WindowsFilesystemGrants {
    pub fn apply(
        container: &WindowsAppContainer,
        grants: &[ResolvedFilesystemGrant],
    ) -> Result<Self, ExecutionError> {
        let sid = container.sid();
        let system_root =
            std::env::var_os("SystemRoot").and_then(|path| std::fs::canonicalize(path).ok());
        let mut applied = Vec::with_capacity(grants.len());
        for grant in grants {
            if grant.kind == FilesystemGrantKind::CharacterDevice {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows AppContainer filesystem policy cannot grant character devices",
                ));
            }
            // Backend system files already grant read/execute access to application packages.
            // Do not rewrite TrustedInstaller-owned DACLs for backend-runtime paths. Project grants
            // are still applied when the project happens to live below SystemRoot (for example,
            // Windows' default SystemTemp directory).
            if grant.access != FilesystemAccess::Write
                && grant.source() == crate::execution::FilesystemGrantSource::BackendRuntime
                && system_root
                    .as_ref()
                    .is_some_and(|root| grant.path.starts_with(root))
            {
                continue;
            }
            // An immutable installation may permit the actual AppContainer to use the runtime
            // while denying the host WRITE_DAC. Only that specific pre-mutation denial permits
            // an alternative: a kernel open under a verified token with this execution's SID.
            // Never swallow a failed ACL update, or infer effective access from an allow ACE.
            if grant.source() == crate::execution::FilesystemGrantSource::BackendRuntime
                && grant.access() != FilesystemAccess::Write
                && !runtime_dacl_is_writable(&grant.path)?
            {
                verify_existing_runtime_access(container, grant)?;
                continue;
            }
            let acl = if grant.source() == crate::execution::FilesystemGrantSource::BackendRuntime
                && grant.access() == FilesystemAccess::Read
            {
                WindowsPathAcl::grant_executable(&grant.path, sid, grant.access(), grant.kind())
            } else {
                WindowsPathAcl::grant(&grant.path, sid, grant.access(), grant.kind())
            }?;
            applied.push(acl);
        }
        Ok(Self { grants: applied })
    }

    pub fn restore(&mut self) -> Result<(), ExecutionError> {
        let mut first_error = None;
        for grant in self.grants.iter_mut().rev() {
            if let Err(error) = grant.restore()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for WindowsPathAcl {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn own_sid(sid: windows_sys::Win32::Foundation::PSID) -> Result<Box<[u32]>, ExecutionError> {
    use windows_sys::Win32::Security::GetLengthSid;

    // SAFETY: callers reject null SIDs before querying their native length.
    let length = unsafe { GetLengthSid(sid) } as usize;
    if length == 0 {
        return Err(unsupported_acl("copy AppContainer SID", 87));
    }
    let words = length.div_ceil(std::mem::size_of::<u32>());
    let mut owned = vec![0u32; words].into_boxed_slice();
    // SAFETY: `owned` has enough aligned storage for the SID's exact byte length.
    unsafe {
        std::ptr::copy_nonoverlapping(sid.cast::<u8>(), owned.as_mut_ptr().cast::<u8>(), length);
    }
    Ok(owned)
}

fn unsupported_acl(operation: &str, code: u32) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!(
            "Windows {operation} failed: {}",
            std::io::Error::from_raw_os_error(code as i32)
        ),
    )
}

#[cfg(test)]
#[path = "../tests/support/windows_acl.rs"]
pub(super) mod windows_acl;

#[cfg(test)]
mod tests {
    use super::lock_acl_mutations;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "native Windows large-project measurement; creates 25,000 disposable files"]
    fn representative_node_modules_acl_measurement() {
        use super::windows_acl::{initialize_inheritance, read_acl};
        let root = std::env::temp_dir().join(format!("tapid-large-acl-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let project = root.join("project");
        let modules = project.join("node_modules");
        let sibling = root.join("sibling.txt");
        std::fs::write(&sibling, b"outside").unwrap();
        let mut paths = vec![root.clone(), sibling, project.clone(), modules.clone()];
        for package in 0..500 {
            let package = modules.join(format!("package-{package}"));
            let lib = package.join("lib");
            std::fs::create_dir_all(&lib).unwrap();
            paths.push(package);
            paths.push(lib.clone());
            for file in 0..50 {
                let file = lib.join(format!("module-{file}.js"));
                std::fs::write(&file, b"module.exports = 42;\n").unwrap();
                paths.push(file);
            }
        }
        initialize_inheritance(&root);
        let before: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        for iteration in 0..3 {
            let start = Instant::now();
            let transaction = lock_acl_mutations().unwrap();
            let grant_wait = start.elapsed();
            let held = Instant::now();
            let mut grant = super::WindowsPathAcl::grant_inner(
                &project,
                container.sid(),
                crate::execution::FilesystemAccess::Read,
                crate::execution::FilesystemGrantKind::DirectorySubtree,
                false,
            )
            .unwrap();
            let grant_hold = held.elapsed();
            drop(transaction);
            let preparation = start.elapsed();
            assert_eq!(read_acl(&paths[0]), before[0]);
            assert_eq!(read_acl(&paths[1]), before[1]);
            let start = Instant::now();
            let transaction = lock_acl_mutations().unwrap();
            let restore_wait = start.elapsed();
            let held = Instant::now();
            grant.restore_unlocked().unwrap();
            let restore_hold = held.elapsed();
            drop(transaction);
            let restoration = start.elapsed();
            for (path, expected) in paths.iter().zip(&before) {
                assert_eq!(
                    &read_acl(path),
                    expected,
                    "exact ACL restoration at {}",
                    path.display()
                );
            }
            eprintln!(
                "LARGE_ACL iteration={iteration} files=25000 packages=500 audited_objects={} grant_ms={:.3} restore_ms={:.3} grant_wait_ms={:.3} grant_hold_ms={:.3} restore_wait_ms={:.3} restore_hold_ms={:.3}",
                paths.len(),
                preparation.as_secs_f64() * 1000.0,
                restoration.as_secs_f64() * 1000.0,
                grant_wait.as_secs_f64() * 1000.0,
                grant_hold.as_secs_f64() * 1000.0,
                restore_wait.as_secs_f64() * 1000.0,
                restore_hold.as_secs_f64() * 1000.0
            );
        }
        container.cleanup().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_preexisting_access_does_not_require_write_dac() {
        immutable_runtime_access_case(false, true);
    }

    #[test]
    fn runtime_preexisting_allow_ace_does_not_override_execute_deny() {
        immutable_runtime_access_case(true, false);
    }

    fn immutable_runtime_access_case(deny_execute: bool, expected_acceptance: bool) {
        use super::windows_acl::read_acl;
        use crate::execution::{
            FilesystemAccess, FilesystemBindingMode, FilesystemGrantKind, FilesystemGrantSource,
            ResolvedFilesystemGrant,
        };
        let root = std::env::temp_dir().join(format!(
            "tapid-runtime-access-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&root).unwrap();
        let runtime = root.join("runtime.exe");
        let cmd = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/cmd.exe");
        std::fs::copy(&cmd, &runtime).unwrap();
        let icacls = cmd.parent().unwrap().join("icacls.exe");
        let mut acl_command = Command::new(&icacls);
        acl_command.arg(&runtime).args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-1-0:RX",
            "*S-1-3-4:R",
            "*S-1-15-2-1:RX",
            "/deny",
        ]);
        if deny_execute {
            acl_command.arg("*S-1-15-2-1:(X)");
        }
        let output = acl_command.arg("*S-1-1-0:(WDAC)").output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let boundaries = [
            root.clone(),
            cmd.parent().unwrap().parent().unwrap().to_path_buf(),
            cmd.parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf(),
        ];
        let boundary_acls: Vec<_> = boundaries.iter().map(|path| read_acl(path)).collect();
        let before = read_acl(&runtime);
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        assert!(
            super::WindowsPathAcl::grant_executable(
                &runtime,
                container.sid(),
                FilesystemAccess::Read,
                FilesystemGrantKind::ExactFile
            )
            .is_err(),
            "fixture must deny WRITE_DAC"
        );
        let resolved = [ResolvedFilesystemGrant {
            path: std::fs::canonicalize(&runtime).unwrap(),
            access: FilesystemAccess::Read,
            kind: FilesystemGrantKind::ExactFile,
            source: FilesystemGrantSource::BackendRuntime,
            binding: FilesystemBindingMode::CanonicalPath,
        }];
        let result = super::WindowsFilesystemGrants::apply(&container, &resolved);
        // Save the outcome before cleanup so RED does not leave the protected disposable file.
        let accepted = result.is_ok();
        if let Err(error) = &result {
            eprintln!("runtime check error: {error:?}");
            if deny_execute {
                let message = error.to_string();
                assert!(
                    message.contains("verify existing AppContainer runtime access failed")
                        && message.contains("os error 5"),
                    "negative must reach the kernel access-check rejection: {error}"
                );
            }
        }
        if accepted {
            let environment = crate::execution::windows_environment_block_units(
                &std::collections::BTreeMap::new(),
            )
            .unwrap();
            let mut command: Vec<u16> = format!("\"{}\" /d /c exit /b 23", runtime.display())
                .encode_utf16()
                .collect();
            command.push(0);
            let job = super::WindowsJob::new(
                &crate::execution::ExecutionLimits::new(Some(5), None, None, None).unwrap(),
                false,
            )
            .unwrap();
            let mut child = super::WindowsSuspendedChild::create(
                &container,
                &super::runtime_path_wide(&runtime),
                &command,
                &environment,
                &super::runtime_path_wide(cmd.parent().unwrap().parent().unwrap()),
            )
            .unwrap();
            job.assign_suspended_process(child.process_handle())
                .unwrap();
            assert_eq!(child.resume_and_wait_for_exit(&job, 5000).unwrap(), 23);
        }
        let outside = root.join("outside.txt");
        std::fs::write(&outside, b"unchanged").unwrap();
        let mut outside_grant = resolved[0].clone();
        outside_grant.path = outside.clone();
        assert!(
            super::verify_existing_runtime_access(&container, &outside_grant).is_err(),
            "token must not gain unrelated read access"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
        drop(result);
        assert_eq!(
            read_acl(&runtime),
            before,
            "existing runtime DACL must stay unchanged"
        );
        for (path, before) in boundaries.iter().zip(&boundary_acls) {
            assert_eq!(
                &read_acl(path),
                before,
                "boundary DACL must remain unchanged"
            );
        }
        let mut project_grant = resolved[0].clone();
        project_grant.source = FilesystemGrantSource::ProjectPolicy;
        assert!(
            super::WindowsFilesystemGrants::apply(&container, &[project_grant]).is_err(),
            "project grant failures must not use runtime fallback"
        );
        container.cleanup().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            accepted, expected_acceptance,
            "actual AppContainer read/execute access result"
        );
    }

    #[test]
    fn declared_grant_leaves_ancestor_and_sibling_dacls_unchanged() {
        let root = std::env::temp_dir().join(format!(
            "tapid-no-ancestor-acl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("project");
        let sibling = root.join("unrelated");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let child = target.join("existing.txt");
        std::fs::write(&child, b"declared data").unwrap();
        use super::windows_acl::{initialize_inheritance, read_acl};
        let paths = [&root, &sibling, &target, &child];
        let initial: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        // Fresh directories can use the legacy inheritance model on hosted Windows: their
        // ACEs are inherited but SE_DACL_AUTO_INHERITED is unset. SetSecurityInfo converts
        // descendants to the current model, even when cleanup restores identical ACE bytes.
        // Establish that model on ONLY this disposable fixture before taking the baseline;
        // never normalize a shared host ancestor or mask control bits in the comparisons.
        // https://learn.microsoft.com/en-us/windows/win32/secauthz/automatic-propagation-of-inheritable-aces
        initialize_inheritance(&root);
        let before: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        for (normalized, original) in before.iter().zip(&initial) {
            assert_eq!(
                normalized.1, original.1,
                "fixture setup must preserve ACE bytes"
            );
            assert!(
                normalized.0 == original.0
                    || normalized.0
                        == original.0 | windows_sys::Win32::Security::SE_DACL_AUTO_INHERITED,
                "fixture setup may only set the auto-inherited bit, not change protection"
            );
        }
        for (control, _) in &before[1..] {
            assert_ne!(
                control & windows_sys::Win32::Security::SE_DACL_AUTO_INHERITED,
                0,
                "fixture descendants must use the current inheritance model"
            );
            assert_eq!(
                control & windows_sys::Win32::Security::SE_DACL_PROTECTED,
                0,
                "fixture descendants must remain unprotected"
            );
        }
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let started = Instant::now();
        let mut grant = super::WindowsPathAcl::grant(
            &target,
            container.sid(),
            crate::execution::FilesystemAccess::Read,
            crate::execution::FilesystemGrantKind::DirectorySubtree,
        )
        .unwrap();
        let preparation = started.elapsed();
        let active_root = read_acl(&root);
        let active_sibling = read_acl(&sibling);
        let started = Instant::now();
        grant.restore().unwrap();
        let cleanup = started.elapsed();
        let after: Vec<_> = paths.iter().map(|path| read_acl(path)).collect();
        container.cleanup().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        eprintln!("declared grant preparation={preparation:?} cleanup={cleanup:?}");
        assert_eq!(
            active_root, before[0],
            "ancestor DACL must never be rewritten"
        );
        assert_eq!(
            active_sibling, before[1],
            "sibling DACL must never be rewritten"
        );
        assert_eq!(
            after, before,
            "cleanup must restore exact target and descendant ACLs"
        );
    }

    #[test]
    fn project_grant_does_not_modify_systemtemp_ancestor() {
        let system_root = std::fs::canonicalize(
            std::env::var_os("SystemRoot").expect("Windows SystemRoot is required"),
        )
        .unwrap();
        let system_temp = std::env::temp_dir();
        let canonical_system_temp = std::fs::canonicalize(&system_temp).unwrap();
        if !canonical_system_temp.starts_with(&system_root) {
            eprintln!(
                "SystemTemp-specific ACL check skipped: temp directory is outside SystemRoot"
            );
            return;
        }
        let root = system_temp.join(format!(
            "tapid-systemtemp-parent-acl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writable = root.join("writable");
        if let Err(error) = std::fs::create_dir_all(&writable) {
            let _ = std::fs::remove_dir_all(&root);
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("SystemTemp-specific ACL check skipped: cannot create a probe directory");
                return;
            }
            panic!("create SystemTemp ACL probe directory: {error}");
        }
        let icacls = system_root.join("System32/icacls.exe");
        let read_acl = |path: &std::path::Path| {
            let output = Command::new(&icacls).arg(path).output().unwrap();
            assert!(
                output.status.success(),
                "icacls failed: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            output.stdout
        };
        let baseline_acl = read_acl(&system_temp);
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let mut grant = super::WindowsPathAcl::grant(
            &writable,
            container.sid(),
            crate::execution::FilesystemAccess::Write,
            crate::execution::FilesystemGrantKind::DirectorySubtree,
        )
        .unwrap();
        let active_acl = read_acl(&system_temp);
        let restore = grant.restore();
        let restored_acl = read_acl(&system_temp);
        let cleanup = container.cleanup();
        std::fs::remove_dir_all(root).unwrap();
        restore.unwrap();
        cleanup.unwrap();
        assert_eq!(
            restored_acl, baseline_acl,
            "normal cleanup must restore SystemTemp DACL"
        );
        assert_eq!(
            active_acl, baseline_acl,
            "project grants must not add temporary ACEs to shared SystemTemp"
        );
    }

    #[test]
    fn directory_write_grant_propagates_to_existing_files() {
        let root = std::env::temp_dir().join(format!(
            "tapid-existing-child-acl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writable = root.join("writable");
        std::fs::create_dir_all(&writable).unwrap();
        let file = writable.join("existing.txt");
        std::fs::write(&file, b"before").unwrap();
        let mut container = crate::execution::windows_job::WindowsAppContainer::create().unwrap();
        let mut grant = super::WindowsPathAcl::grant(
            &writable,
            container.sid(),
            crate::execution::FilesystemAccess::Write,
            crate::execution::FilesystemGrantKind::DirectorySubtree,
        )
        .unwrap();
        let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
        let icacls = std::fs::canonicalize(
            std::path::PathBuf::from(system_root).join("System32/icacls.exe"),
        )
        .unwrap();
        let output = Command::new(icacls).arg(&file).output().unwrap();
        let listing = String::from_utf8_lossy(&output.stdout).into_owned();
        let restore = grant.restore();
        container.cleanup().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        restore.unwrap();
        assert!(output.status.success(), "icacls failed: {listing}");
        assert!(
            listing.contains("(I)(W"),
            "existing child did not inherit the write ACE: {listing}"
        );
    }

    #[test]
    fn acl_mutation_lock_serializes_separate_processes() {
        let unique = format!(
            "tapid-acl-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        std::fs::create_dir(&directory).unwrap();
        let ready = directory.join("ready");
        let executable = std::env::current_exe().unwrap();
        let held = lock_acl_mutations().expect("parent must acquire ACL lock");
        let spawn_result = Command::new(executable)
            .args([
                "--exact",
                "execution::windows_job::filesystem::tests::acl_mutation_lock_child",
                "--nocapture",
            ])
            .env("TAPID_ACL_LOCK_CHILD", "1")
            .env("TAPID_ACL_LOCK_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(error) => {
                drop(held);
                panic!("spawn ACL lock child: {error}");
            }
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let child_started = ready.exists();
        std::thread::sleep(Duration::from_millis(200));
        let child_waiting_for_lock = child.try_wait().map(|status| status.is_none());
        drop(held);
        if !child_started {
            let _ = child.kill();
        }
        let child_status = child.wait();
        let _ = std::fs::remove_dir_all(directory);
        assert!(child_started, "child did not reach the lock attempt");
        assert!(
            matches!(child_waiting_for_lock, Ok(true)),
            "separate process bypassed the held ACL mutation lock"
        );
        assert!(child_status.unwrap().success());
    }

    #[test]
    fn acl_mutation_lock_child() {
        if std::env::var_os("TAPID_ACL_LOCK_CHILD").is_none() {
            return;
        }
        let ready = std::env::var_os("TAPID_ACL_LOCK_READY").unwrap();
        std::fs::write(ready, b"ready").unwrap();
        let _lock =
            lock_acl_mutations().expect("child must acquire ACL lock after parent releases");
    }
}
