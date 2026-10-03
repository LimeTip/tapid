#![cfg(windows)]
use super::*;
use std::sync::{Mutex, MutexGuard};

// Windows exposes no compare-and-swap DACL update; serialize Tapid's ACL transactions within this process.
static WINDOWS_ACL_MUTATION_LOCK: Mutex<()> = Mutex::new(());

fn lock_acl_mutations() -> Result<MutexGuard<'static, ()>, ExecutionError> {
    WINDOWS_ACL_MUTATION_LOCK
        .lock()
        .map_err(|_| unsupported_acl("serialize filesystem ACL updates", 6))
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
        let _transaction = lock_acl_mutations()?;
        let mut parent_grants = Vec::new();
        if matches!(
            kind,
            FilesystemGrantKind::ExactFile | FilesystemGrantKind::ExactDirectory
        ) {
            let parent = path.parent().ok_or_else(|| {
                unsupported_acl("filesystem grant target has no parent directory", 87)
            })?;
            parent_grants.push(Self::grant_directory_listing_unlocked(parent, sid)?);
        }
        let mut grant = Self::grant_inner(path, sid, access, kind, false, allow_execute)?;
        grant.parent_grants = parent_grants;
        Ok(grant)
    }

    fn grant_directory_listing_unlocked(
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
        directory_listing_only: bool,
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
        if !kind_matches || (directory_listing_only && kind != FilesystemGrantKind::ExactDirectory)
        {
            return Err(unsupported_acl(
                "filesystem grant target kind is unsupported or changed",
                87,
            ));
        }
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
                "open filesystem grant for DACL update",
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

        let root_permissions = if directory_listing_only {
            FILE_LIST_DIRECTORY
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
        let entries = if access == FilesystemAccess::Write && is_directory && subtree {
            [
                make_entry(root_permissions, 0),
                make_entry(
                    FILE_GENERIC_WRITE,
                    inheritance | windows_sys::Win32::Security::INHERIT_ONLY_ACE,
                ),
            ]
        } else {
            [make_entry(root_permissions, inheritance), make_entry(0, 0)]
        };
        let entry_count = if access == FilesystemAccess::Write && is_directory && subtree {
            2
        } else {
            1
        };
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
                    let mut entry = EXPLICIT_ACCESS_W {
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
                        unsafe { SetEntriesInAclW(1, &mut entry, current_dacl, &mut updated_dacl) };
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
            if let Err(error) = parent_grant.restore_unlocked() {
                if restore_error.is_none() {
                    restore_error = Some(error);
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
            // Windows system files already grant read/execute access to application packages.
            // Do not attempt to rewrite TrustedInstaller-owned DACLs; if the OS ACL has been
            // hardened beyond that baseline, CreateProcess/read access fails closed at use time.
            if grant.access != FilesystemAccess::Write
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
            if let Err(error) = grant.restore() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
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
