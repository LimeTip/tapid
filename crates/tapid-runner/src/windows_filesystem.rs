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
    parent_grants: Vec<WindowsPathAcl>,
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
        let mut parent_grants = Vec::new();
        let _transaction = lock_acl_mutations()?;
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| unsupported_acl("resolve SystemRoot before parent ACL changes", 2))?;
        let system_root = std::fs::canonicalize(system_root).map_err(|error| {
            unsupported_acl(
                "canonicalize SystemRoot before parent ACL changes",
                error.raw_os_error().unwrap_or(1) as u32,
            )
        })?;
        let mut parent = path.parent().map(std::path::Path::to_path_buf);
        while let Some(directory) = parent {
            // The volume root is not user-writable. Native AppContainer probes also show that
            // declared descendants remain accessible without parent ACEs, so never modify shared
            // SystemRoot DACLs such as the default SystemTemp directory.
            if directory.parent().is_none() {
                break;
            }
            let canonical_directory = std::fs::canonicalize(&directory).map_err(|error| {
                unsupported_acl(
                    "canonicalize parent directory before ACL changes",
                    error.raw_os_error().unwrap_or(1) as u32,
                )
            })?;
            if canonical_directory.starts_with(&system_root)
                || !Self::parent_dacl_is_writable(&directory)?
            {
                break;
            }
            trace_windows_stage("acl: parent traversal grant start");
            parent_grants.push(Self::grant_parent_traversal_unlocked(&directory, sid)?);
            trace_windows_stage("acl: parent traversal grant complete");
            parent = directory
                .parent()
                .filter(|ancestor| *ancestor != directory)
                .map(std::path::Path::to_path_buf);
        }
        trace_windows_stage("acl: target grant start");
        let mut grant = Self::grant_inner(path, sid, access, kind, false, allow_execute)?;
        trace_windows_stage("acl: target grant complete");
        grant.parent_grants = parent_grants;
        Ok(grant)
    }

    fn parent_dacl_is_writable(path: &std::path::Path) -> Result<bool, ExecutionError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        };

        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: wide is NUL-terminated, and backup semantics permits opening a directory.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(5) {
                return Ok(false);
            }
            return Err(unsupported_acl(
                "probe parent directory DACL writability",
                error.raw_os_error().unwrap_or(1) as u32,
            ));
        }
        // SAFETY: handle was returned successfully by CreateFileW.
        unsafe { CloseHandle(handle) };
        Ok(true)
    }

    fn grant_parent_traversal_unlocked(
        path: &std::path::Path,
        sid: windows_sys::Win32::Foundation::PSID,
    ) -> Result<Self, ExecutionError> {
        Self::grant_inner(
            path,
            sid,
            FilesystemAccess::ReadMetadata,
            FilesystemGrantKind::ExactDirectory,
            true,
            false,
        )
    }

    fn grant_inner(
        path: &std::path::Path,
        sid: windows_sys::Win32::Foundation::PSID,
        access: FilesystemAccess,
        kind: FilesystemGrantKind,
        directory_traversal_only: bool,
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
        if !kind_matches
            || (directory_traversal_only && kind != FilesystemGrantKind::ExactDirectory)
        {
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
                parent_grants: Vec::new(),
            });
        }

        let root_permissions = if directory_traversal_only {
            FILE_EXECUTE | FILE_READ_ATTRIBUTES
        } else {
            match access {
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
            }
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
            entries.push(make_entry(
                FILE_GENERIC_WRITE,
                inheritance | windows_sys::Win32::Security::INHERIT_ONLY_ACE,
            ));
        } else {
            entries.push(make_entry(root_permissions, inheritance));
        }
        if is_directory && !directory_traversal_only && !allow_execute {
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
            parent_grants: Vec::new(),
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
        for parent_grant in self.parent_grants.iter_mut().rev() {
            if let Err(error) = parent_grant.restore_unlocked()
                && restore_error.is_none()
            {
                restore_error = Some(error);
            }
        }
        if let Some(error) = restore_error {
            return Err(error);
        }
        self.restored = true;
        Ok(())
    }
}

/// Owns the policy-derived ACL changes for one AppContainer and restores them on every exit path.
pub struct WindowsFilesystemGrants {
    grants: Vec<WindowsPathAcl>,
}

impl WindowsFilesystemGrants {
    pub fn apply(
        sid: windows_sys::Win32::Foundation::PSID,
        grants: &[ResolvedFilesystemGrant],
    ) -> Result<Self, ExecutionError> {
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
mod tests {
    use super::lock_acl_mutations;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

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
