//! Held-object authentication; pipe process IDs alone never authorize privileged work.
use super::{owned, raw};
use crate::execution::windows_job::token_information;
use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    os::windows::io::OwnedHandle,
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    Storage::FileSystem::{READ_CONTROL, WRITE_DAC},
    System::{
        Pipes::{
            GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, ImpersonateNamedPipeClient,
        },
        SystemServices::SECURITY_DESCRIPTOR_REVISION,
        Threading::*,
    },
};
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    pub user: Vec<u32>,
    session: u32,
    logon: (u32, i32),
    integrity: u32,
    elevation: u32,
    restricted: bool,
    container: bool,
}
fn token(process: isize) -> Result<OwnedHandle, u32> {
    let mut t = 0;
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut t) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    owned(t)
}
fn value<T: Copy>(token: isize, class: i32) -> Result<T, u32> {
    let b = token_information(token, class)?;
    if b.len() * size_of::<usize>() < size_of::<T>() {
        return Err(ERROR_INVALID_DATA);
    }
    Ok(unsafe { *b.as_ptr().cast::<T>() })
}
fn sid(sid: PSID) -> Result<Vec<u32>, u32> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    let n = unsafe { GetLengthSid(sid) };
    if !(8..=68).contains(&n) {
        return Err(ERROR_INVALID_DATA);
    }
    let mut b = vec![0u32; (n as usize).div_ceil(4)];
    if unsafe { CopySid(n, b.as_mut_ptr().cast(), sid) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    Ok(b)
}
fn identity(token: isize) -> Result<Identity, u32> {
    let user_buffer = token_information(token, TokenUser)?;
    let user = sid(unsafe { (*user_buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })?;
    let statistics = value::<TOKEN_STATISTICS>(token, TokenStatistics)?;
    let label_buffer = token_information(token, TokenIntegrityLevel)?;
    let label = unsafe { &*label_buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
    let pointer = label.Label.Sid;
    if pointer.is_null() || unsafe { IsValidSid(pointer) } == 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    let count = unsafe { *GetSidSubAuthorityCount(pointer) };
    if count == 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok(Identity {
        user,
        session: value(token, TokenSessionId)?,
        logon: (
            statistics.AuthenticationId.LowPart,
            statistics.AuthenticationId.HighPart,
        ),
        integrity: unsafe { *GetSidSubAuthority(pointer, u32::from(count) - 1) },
        elevation: value(token, TokenElevationType)?,
        restricted: unsafe { IsTokenRestricted(token) } != 0,
        container: value::<u32>(token, TokenIsAppContainer)? != 0,
    })
}
struct Revert;
impl Drop for Revert {
    fn drop(&mut self) {
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}
pub(super) fn caller(pipe: isize) -> Result<(OwnedHandle, Identity), u32> {
    let mut pid = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    let process = owned(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_DUP_HANDLE | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    })?;
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    let revert = Revert;
    let mut t = 0;
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut t) } == 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    let t = owned(t)?;
    let peer = identity(raw(&t))?;
    drop(t);
    drop(revert);
    let held = identity(raw(&token(raw(&process))?))?;
    if peer != held
        || peer.container
        || peer.integrity < 0x2000
        || unsafe { WaitForSingleObject(raw(&process), 0) } != WAIT_TIMEOUT
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok((process, peer))
}
pub(super) fn server(pipe: isize) -> Result<OwnedHandle, u32> {
    let expected = super::installation::verified_service_pid()?;
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 || pid != expected {
        return Err(ERROR_ACCESS_DENIED);
    }
    let process = owned(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    })?;
    let info = identity(raw(&token(raw(&process))?))?;
    // S-1-5-18 is LocalSystem, independent of localized account names.
    if info.user != [0x00000101, 0x05000000, 18]
        || info.container
        || unsafe { WaitForSingleObject(raw(&process), 0) } != WAIT_TIMEOUT
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    let mut check = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut check) } == 0
        || check != pid
        || super::installation::verified_service_pid()? != pid
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok(process)
}
/// Permit ordinary clients to hold the server and query its actual token identity.
pub(super) fn prepare_server_authentication() -> Result<(), u32> {
    let process = unsafe { GetCurrentProcess() };
    allow_interactive_inspection(
        process,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
    )?;
    let mut token = 0;
    if unsafe { OpenProcessToken(process, TOKEN_QUERY | READ_CONTROL | WRITE_DAC, &mut token) } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    let token = owned(token)?;
    allow_interactive_inspection(raw(&token), TOKEN_QUERY)
}

fn allow_interactive_inspection(object: isize, rights: u32) -> Result<(), u32> {
    use windows_sys::Win32::Security::Authorization::*;
    let mut needed = 0;
    unsafe {
        GetKernelObjectSecurity(
            object,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 || needed > 65536 {
        return Err(ERROR_INVALID_DATA);
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let descriptor = buffer.as_mut_ptr().cast();
    if unsafe {
        GetKernelObjectSecurity(
            object,
            DACL_SECURITY_INFORMATION,
            descriptor,
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    let mut present = 0;
    let mut defaulted = 0;
    let mut original = null_mut();
    if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut original, &mut defaulted) }
        == 0
        || present == 0
        || original.is_null()
    {
        return Err(ERROR_INVALID_DATA);
    }
    let mut sid = [0u32; 17];
    let mut length = size_of::<[u32; 17]>() as u32;
    if unsafe {
        CreateWellKnownSid(
            WinInteractiveSid,
            null_mut(),
            sid.as_mut_ptr().cast(),
            &mut length,
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: rights,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.as_mut_ptr().cast(),
        },
    };
    let mut merged = null_mut();
    let result = unsafe { SetEntriesInAclW(1, &entry, original, &mut merged) };
    if result != 0 {
        return Err(result);
    }
    // Preserve existing authority, including SYSTEM/administrator control and
    // explicit denials. Add no inheritance, token duplication or mutation rights.
    let mut update: SECURITY_DESCRIPTOR = unsafe { zeroed() };
    let initialized = unsafe {
        InitializeSecurityDescriptor(
            (&mut update as *mut SECURITY_DESCRIPTOR).cast(),
            SECURITY_DESCRIPTOR_REVISION,
        )
    };
    let attached = initialized != 0
        && unsafe {
            SetSecurityDescriptorDacl(
                (&mut update as *mut SECURITY_DESCRIPTOR).cast(),
                1,
                merged,
                0,
            )
        } != 0;
    let result = attached
        && unsafe {
            SetKernelObjectSecurity(
                object,
                DACL_SECURITY_INFORMATION,
                (&mut update as *mut SECURITY_DESCRIPTOR).cast(),
            )
        } != 0;
    let error = unsafe { GetLastError() };
    unsafe {
        LocalFree(merged.cast());
    }
    if result { Ok(()) } else { Err(error) }
}
fn clone_handle(caller: isize, handle: u64) -> Result<OwnedHandle, u32> {
    let mut output = 0;
    if unsafe {
        DuplicateHandle(
            caller,
            handle as isize,
            GetCurrentProcess(),
            &mut output,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    owned(output)
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryObject(
        object: isize,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}
fn granted_access(handle: isize) -> Result<u32, u32> {
    let mut b = [0u64; 7];
    let mut returned = 0;
    let status = unsafe {
        NtQueryObject(
            handle,
            0,
            b.as_mut_ptr().cast(),
            size_of::<[u64; 7]>() as u32,
            &mut returned,
        )
    };
    if status < 0 || returned < 16 {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok((b[0] >> 32) as u32)
}
pub(super) struct Target {
    pub _process: OwnedHandle,
    pub job: OwnedHandle,
    pub sid: Vec<u32>,
}
pub(super) fn target(
    caller: &OwnedHandle,
    owner: &Identity,
    process: u64,
    job: u64,
) -> Result<Target, u32> {
    use windows_sys::Win32::System::JobObjects::*;
    let process = clone_handle(raw(caller), process)?;
    let job = clone_handle(raw(caller), job)?;
    let needed = 0x0004 | 0x0008;
    if granted_access(raw(&job))? & needed != needed {
        return Err(ERROR_ACCESS_DENIED);
    }
    let t = token(raw(&process))?;
    let root = identity(raw(&t))?;
    let mut member = 0;
    if !root.container
        || root.user != owner.user
        || root.logon != owner.logon
        || root.session != owner.session
        || unsafe { IsProcessInJob(raw(&process), raw(&job), &mut member) } == 0
        || member == 0
        || unsafe { WaitForSingleObject(raw(&process), 0) } != WAIT_TIMEOUT
    {
        return Err(ERROR_ACCESS_DENIED);
    }
    crate::execution::windows_job::validate_runtime_capabilities(raw(&t))?;
    let package_buffer = token_information(raw(&t), TokenAppContainerSid)?;
    let package = unsafe {
        &*package_buffer
            .as_ptr()
            .cast::<TOKEN_APPCONTAINER_INFORMATION>()
    };
    Ok(Target {
        _process: process,
        job,
        sid: sid(package.TokenAppContainer)?,
    })
}
pub(super) fn job_empty(job: isize) -> Result<bool, u32> {
    use windows_sys::Win32::System::JobObjects::*;
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    Ok(info.ActiveProcesses == 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_server_inspection_grants_only_query_and_wait_to_interactive_users() {
        use std::{
            os::windows::io::AsRawHandle,
            process::{Command, Stdio},
        };
        // Use a disposable process: other kernel object types have different
        // valid access masks and cannot prove process-query permission.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution::windows_guard::identity::tests::guard_server_inspection_child",
            ])
            .env("TAPID_GUARD_INSPECTION_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let process = child.as_raw_handle() as isize;
        allow_interactive_inspection(
            process,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
        )
        .unwrap();
        let rights = interactive_rights(process);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            rights,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE
        );
    }

    #[test]
    fn guard_token_inspection_grants_query_without_duplication_or_mutation() {
        use windows_sys::Win32::Security::Authorization::*;
        let mut source = 0;
        assert_ne!(
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_DUPLICATE | TOKEN_QUERY,
                    &mut source,
                )
            },
            0
        );
        let source = owned(source).unwrap();
        let mut duplicate = 0;
        assert_ne!(
            unsafe {
                DuplicateTokenEx(
                    raw(&source),
                    TOKEN_QUERY | READ_CONTROL | WRITE_DAC,
                    std::ptr::null(),
                    SecurityIdentification,
                    TokenImpersonation,
                    &mut duplicate,
                )
            },
            0
        );
        let duplicate = owned(duplicate).unwrap();
        // The explicit deny must continue to override the matching allow;
        // READ_CONTROL is an existing allow which the merge must retain.
        let fixture =
            super::super::wide("D:P(D;;0x20;;;IU)(A;;0x20020;;;IU)(A;;GA;;;SY)(A;;GA;;;BA)");
        let mut descriptor = null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    fixture.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );
        let applied = unsafe {
            SetKernelObjectSecurity(raw(&duplicate), DACL_SECURITY_INFORMATION, descriptor)
        };
        unsafe {
            LocalFree(descriptor);
        }
        assert_ne!(applied, 0);
        assert_eq!(interactive_rights(raw(&duplicate)), READ_CONTROL);
        allow_interactive_inspection(raw(&duplicate), TOKEN_QUERY).unwrap();
        assert_eq!(
            interactive_rights(raw(&duplicate)),
            READ_CONTROL | TOKEN_QUERY
        );
    }

    fn interactive_rights(object: isize) -> u32 {
        use windows_sys::Win32::Security::Authorization::*;
        let mut acl = null_mut();
        let mut descriptor = null_mut();
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    object,
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    &mut acl,
                    null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        let mut sid = [0u32; 17];
        let mut length = size_of::<[u32; 17]>() as u32;
        assert_ne!(
            unsafe {
                CreateWellKnownSid(
                    WinInteractiveSid,
                    null_mut(),
                    sid.as_mut_ptr().cast(),
                    &mut length,
                )
            },
            0
        );
        let trustee = TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.as_mut_ptr().cast(),
        };
        let mut rights = 0;
        let result = unsafe { GetEffectiveRightsFromAclW(acl, &trustee, &mut rights) };
        unsafe {
            LocalFree(descriptor);
        }
        assert_eq!(result, 0);
        rights
    }
    #[test]
    fn guard_server_inspection_child() {
        if std::env::var("TAPID_GUARD_INSPECTION_CHILD").as_deref() == Ok("1") {
            let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
        }
    }
    #[test]
    fn guard_rejects_host_process_and_source_job_without_termination_rights() {
        use windows_sys::Win32::System::JobObjects::*;
        let caller = owned(unsafe {
            OpenProcess(
                PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                GetCurrentProcessId(),
            )
        })
        .unwrap();
        let owner = identity(raw(&token(raw(&caller)).unwrap())).unwrap();
        let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) }).unwrap();
        let mut limited = 0;
        assert_ne!(
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    raw(&job),
                    GetCurrentProcess(),
                    &mut limited,
                    0x0004,
                    0,
                    0,
                )
            },
            0
        );
        let limited = owned(limited).unwrap();
        assert_eq!(granted_access(raw(&limited)).unwrap() & 0x0008, 0);
        assert!(target(&caller, &owner, raw(&caller) as u64, raw(&limited) as u64).is_err());
        assert!(target(&caller, &owner, raw(&caller) as u64, raw(&job) as u64).is_err());
    }
}
