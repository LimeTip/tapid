//! Native access controls for the suspended child's ambient package authority.
//! TokenIsLessPrivilegedAppContainer is not a usable user-mode query on all hosts.
//! The controls establish the sampled permission difference, not a complete input closure.

use super::{ExecutionError, ExecutionErrorCategory, unsupported_job};
use std::{
    ffi::CStr,
    mem::size_of,
    ptr::{NonNull, null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree, PSID},
    Security::{
        AccessCheck,
        Authorization::{
            ConvertSidToStringSidA, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        DuplicateTokenEx, GENERIC_MAPPING, PRIVILEGE_SET, SecurityImpersonation, TOKEN_QUERY,
        TokenImpersonation,
    },
    Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_READ_DATA,
    },
};

/// The sole runtime capability: Windows socket-provider initialization reads the registry.
/// This grants no network capability and is separate from filesystem package access.
pub(super) struct RegistryReadCapability {
    groups: *mut PSID,
    group_count: u32,
    capabilities: *mut PSID,
    capability_count: u32,
    entry: Box<windows_sys::Win32::Security::SID_AND_ATTRIBUTES>,
}

impl RegistryReadCapability {
    pub(super) fn new() -> Result<Self, ExecutionError> {
        use windows_sys::Win32::Security::{
            DeriveCapabilitySidsFromName, IsValidSid, SID_AND_ATTRIBUTES,
        };
        let mut capability = Self {
            groups: null_mut(),
            group_count: 0,
            capabilities: null_mut(),
            capability_count: 0,
            entry: Box::new(SID_AND_ATTRIBUTES {
                Sid: null_mut(),
                Attributes: windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED as u32,
            }),
        };
        let name: Vec<_> = "registryRead".encode_utf16().chain(Some(0)).collect();
        // SAFETY: terminated fixed capability name; the API allocates both output SID arrays.
        if unsafe {
            DeriveCapabilitySidsFromName(
                name.as_ptr(),
                &mut capability.groups,
                &mut capability.group_count,
                &mut capability.capabilities,
                &mut capability.capability_count,
            )
        } == 0
        {
            return Err(unsupported_job("derive registry-read runtime capability"));
        }
        let sid_array = NonNull::new(capability.capabilities)
            .ok_or_else(|| unsupported_job("receive registry-read runtime capability"))?;
        if capability.capability_count != 1 {
            return Err(unsupported_job(
                "receive one registry-read runtime capability",
            ));
        }
        // SAFETY: the native output array contains exactly one SID, owned by this value.
        let sid = unsafe { *sid_array.as_ptr() };
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(unsupported_job("validate registry-read runtime capability"));
        }
        capability.entry.Sid = sid;
        Ok(capability)
    }

    pub(super) fn entry(&self) -> *mut windows_sys::Win32::Security::SID_AND_ATTRIBUTES {
        (&*self.entry as *const windows_sys::Win32::Security::SID_AND_ATTRIBUTES).cast_mut()
    }
}

impl Drop for RegistryReadCapability {
    fn drop(&mut self) {
        for (array, count) in [
            (self.groups, self.group_count),
            (self.capabilities, self.capability_count),
        ] {
            if let Some(array) = NonNull::new(array) {
                // SAFETY: DeriveCapabilitySidsFromName allocates count SID pointers and each
                // individual SID with LocalAlloc. Free the SIDs before the containing array.
                unsafe {
                    for index in 0..count as usize {
                        LocalFree(*array.as_ptr().add(index));
                    }
                    LocalFree(array.as_ptr().cast());
                }
            }
        }
    }
}

struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: this owner retains the handle returned by DuplicateTokenEx.
        unsafe { CloseHandle(self.0) };
    }
}

struct Allocation(NonNull<std::ffi::c_void>);
impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: SID/descriptor conversion APIs allocate these values with LocalAlloc.
        unsafe { LocalFree(self.0.as_ptr()) };
    }
}

pub(super) fn verify_lpac_access(
    primary: HANDLE,
    expected_sid: PSID,
) -> Result<(), ExecutionError> {
    let mut duplicate = 0;
    // SAFETY: primary is the actual suspended child's token, opened with TOKEN_DUPLICATE.
    if unsafe {
        DuplicateTokenEx(
            primary,
            TOKEN_QUERY,
            null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut duplicate,
        )
    } == 0
    {
        return Err(unsupported_job(
            "duplicate child token for package access controls",
        ));
    }
    let duplicate = Token(duplicate);
    let mut sid_text = null_mut();
    // SAFETY: expected_sid is the validated per-run profile SID retained by the caller.
    if unsafe { ConvertSidToStringSidA(expected_sid, &mut sid_text) } == 0 {
        return Err(unsupported_job(
            "encode expected package SID for access control",
        ));
    }
    let sid_text = Allocation(
        NonNull::new(sid_text.cast())
            .ok_or_else(|| unsupported_job("receive expected package SID"))?,
    );
    // SAFETY: ConvertSidToStringSidA returns a terminated ASCII SID owned by sid_text.
    let sid = unsafe { CStr::from_ptr(sid_text.0.as_ptr().cast()) }
        .to_str()
        .map_err(|_| unsupported_job("decode expected package SID"))?;
    let positive = format!("O:SYG:SYD:P(A;;0x1;;;WD)(A;;0x1;;;{sid})");
    // Both controls permit the user's Everyone principal. Only the package-side
    // trustee differs. Never interpret an API error as successful access denial.
    if !permits_read(duplicate.0, &positive)?
        || permits_read(duplicate.0, "O:SYG:SYD:P(A;;0x1;;;WD)(A;;0x1;;;AC)")?
    {
        return Err(ExecutionError::new(
            ExecutionErrorCategory::UnsupportedContainment,
            "Windows child token failed explicit-package/ambient-package access controls; refusing to resume it",
        ));
    }
    Ok(())
}

fn permits_read(token: HANDLE, sddl: &str) -> Result<bool, ExecutionError> {
    let wide: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = null_mut();
    // SAFETY: the controlled SDDL is terminated; the API allocates the complete descriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(unsupported_job(
            "construct package access control descriptor",
        ));
    }
    let descriptor = Allocation(
        NonNull::new(descriptor)
            .ok_or_else(|| unsupported_job("receive package access control descriptor"))?,
    );
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    // SAFETY: PRIVILEGE_SET is plain Win32 output data; one entry suffices for this
    // FILE_READ_DATA check, which requests no privilege-dependent access rights.
    let mut privileges: PRIVILEGE_SET = unsafe { std::mem::zeroed() };
    let mut length = size_of::<PRIVILEGE_SET>() as u32;
    let mut granted = 0;
    let mut allowed = 0;
    // SAFETY: token is a queried impersonation token. The owned descriptor includes
    // owner/group SIDs, and every output has valid writable storage for the call.
    if unsafe {
        AccessCheck(
            descriptor.0.as_ptr(),
            token,
            FILE_READ_DATA,
            &mapping,
            &mut privileges,
            &mut length,
            &mut granted,
            &mut allowed,
        )
    } == 0
    {
        return Err(unsupported_job("check suspended child package authority"));
    }
    Ok(allowed != 0 && granted & FILE_READ_DATA != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        Security::TOKEN_DUPLICATE,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    #[test]
    fn package_access_controls_reject_an_ordinary_host_token() {
        let container = super::super::WindowsAppContainer::create().unwrap();
        let mut token = 0;
        // SAFETY: current process is valid; output receives a TOKEN_QUERY/DUPLICATE handle.
        assert_ne!(
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    &mut token,
                )
            },
            0
        );
        let token = Token(token);
        assert!(verify_lpac_access(token.0, container.sid()).is_err());
    }
}
