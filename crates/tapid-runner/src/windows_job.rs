#![cfg(windows)]

use crate::config::ExecutionLimits;
use crate::execution::{
    ExecutionError, ExecutionErrorCategory, FilesystemAccess, FilesystemGrantKind,
    ResolvedFilesystemGrant,
};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Security::{
    EqualSid, FreeSid, GetTokenInformation, IsValidSid, SECURITY_CAPABILITIES,
    TOKEN_APPCONTAINER_INFORMATION, TOKEN_QUERY, TokenAppContainerSid, TokenIsAppContainer,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// Owns a kernel Job Object configured before any child is assigned to it.
#[allow(dead_code)] // The Windows launch lifecycle will own this after suspended creation lands.
pub(super) struct WindowsJob {
    handle: HANDLE,
}

#[allow(dead_code)] // Used by the Windows launch lifecycle and the native configuration test.
impl WindowsJob {
    pub(super) fn new(
        limits: &ExecutionLimits,
        restrict_subprocesses: bool,
    ) -> Result<Self, ExecutionError> {
        // SAFETY: null attributes request a private unnamed object with default security.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle == 0 {
            return Err(unsupported_job("create Job Object"));
        }
        let job = Self { handle };

        // SAFETY: the structure is a plain Win32 POD output/input structure; zero is the documented
        // baseline for every limit field that is not explicitly enabled below.
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let process_limit = match (limits.max_processes(), restrict_subprocesses) {
            (Some(limit), true) => Some(limit.min(1)),
            (Some(limit), false) => Some(limit),
            (None, true) => Some(1),
            (None, false) => None,
        };
        if let Some(limit) = process_limit {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            information.BasicLimitInformation.ActiveProcessLimit = limit;
        }
        if let Some(limit) = limits.max_memory_bytes() {
            information.JobMemoryLimit = usize::try_from(limit).map_err(|_| {
                ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows Job Object cannot represent the requested job-wide memory limit",
                )
            })?;
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        }
        information.BasicLimitInformation.LimitFlags = flags;
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("Win32 Job Object information structure size fits u32");
        // SAFETY: `information` points to a fully initialized structure of the size supplied.
        let configured = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                size,
            )
        };
        if configured == 0 {
            return Err(unsupported_job("configure Job Object limits"));
        }
        Ok(job)
    }

    /// Assigns a child that the caller created suspended, then verifies membership before it may run.
    #[allow(dead_code)]
    pub(super) fn assign_suspended_process(&self, process: HANDLE) -> Result<(), ExecutionError> {
        if process == 0 {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "cannot assign an invalid process handle to the Windows Job Object",
            ));
        }
        // SAFETY: the caller must retain a valid process handle and keep the child suspended until
        // this call and the membership check complete.
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process) };
        if assigned == 0 {
            return Err(unsupported_job("assign suspended child to Job Object"));
        }

        let mut in_job = 0;
        // SAFETY: both handles are valid for this call; `in_job` is a writable BOOL destination.
        let verified = unsafe { IsProcessInJob(process, self.handle, &mut in_job) };
        if verified == 0 {
            return Err(unsupported_job(
                "verify suspended child Job Object membership",
            ));
        }
        if in_job == 0 {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows reported successful Job Object assignment but membership verification failed",
            ));
        }
        Ok(())
    }

    pub(super) fn terminate_all(&self) -> Result<(), ExecutionError> {
        // SAFETY: the owned Job Object handle remains valid for the duration of the call.
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            return Err(unsupported_job("terminate Windows Job Object members"));
        }
        Ok(())
    }

    pub(super) fn active_process_count(&self) -> Result<u32, ExecutionError> {
        // SAFETY: zeroed is a valid writable destination; the declared output length matches.
        let mut information: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        let size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
            .expect("Job Object accounting information size fits u32");
        // SAFETY: the Job Object handle and output buffer remain valid for the call.
        let queried = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                (&mut information as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast::<c_void>(),
                size,
                null_mut(),
            )
        };
        if queried == 0 {
            return Err(unsupported_job("query active Job Object process count"));
        }
        Ok(information.ActiveProcesses)
    }

    #[cfg(test)]
    fn query_extended_limits(
        &self,
    ) -> Result<JOBOBJECT_EXTENDED_LIMIT_INFORMATION, ExecutionError> {
        // SAFETY: zeroed is a valid writable destination and the API fills the declared structure.
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("Win32 Job Object information structure size fits u32");
        // SAFETY: the handle is owned by self and the output pointer/length match the structure.
        let queried = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                (&mut information as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                size,
                null_mut(),
            )
        };
        if queried == 0 {
            return Err(unsupported_job("query configured Job Object limits"));
        }
        Ok(information)
    }
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        if self.handle != 0 {
            // SAFETY: this wrapper uniquely owns the Job Object handle.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

static NEXT_APPCONTAINER_ID: AtomicU64 = AtomicU64::new(1);

/// Owns a per-execution AppContainer profile and its allocated SID.
#[allow(dead_code)]
pub(super) struct WindowsAppContainer {
    name: Vec<u16>,
    sid: windows_sys::Win32::Foundation::PSID,
    deleted: bool,
}

#[allow(dead_code)]
impl WindowsAppContainer {
    pub(super) fn create() -> Result<Self, ExecutionError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows AppContainer profile name could not be generated",
                )
            })?
            .as_nanos();
        let sequence = NEXT_APPCONTAINER_ID.fetch_add(1, Ordering::Relaxed);
        let profile = format!("td-{:x}-{nonce:x}-{sequence:x}", std::process::id());
        if profile.encode_utf16().count() > 64 {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows AppContainer profile name exceeds the platform limit",
            ));
        }
        let name = profile
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let display = format!("Tapid {profile}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let description = "Ephemeral Tapid root-script execution"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut sid = null_mut();
        // SAFETY: all strings are NUL-terminated; no capability SIDs are requested, so null/zero
        // are the documented capability list inputs. The returned SID is released with FreeSid.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if result < 0 {
            if !sid.is_null() {
                // SAFETY: the API returned this allocation despite reporting failure.
                unsafe { FreeSid(sid) };
            }
            return Err(unsupported_hresult("create AppContainer profile", result));
        }
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            if !sid.is_null() {
                // SAFETY: the profile API returned this allocation.
                unsafe { FreeSid(sid) };
            }
            // SAFETY: the name is valid and profile creation succeeded.
            unsafe { DeleteAppContainerProfile(name.as_ptr()) };
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows created an AppContainer profile without a valid SID",
            ));
        }
        Ok(Self {
            name,
            sid,
            deleted: false,
        })
    }

    pub(super) fn sid(&self) -> windows_sys::Win32::Foundation::PSID {
        self.sid
    }

    pub(super) fn profile_name(&self) -> &[u16] {
        &self.name
    }

    pub(super) fn cleanup(&mut self) -> Result<(), ExecutionError> {
        if !self.deleted {
            // SAFETY: profile name is the exact NUL-terminated name created by this value.
            let result = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
            if result < 0 {
                return Err(unsupported_hresult("delete AppContainer profile", result));
            }
            self.deleted = true;
        }
        if !self.sid.is_null() {
            // SAFETY: CreateAppContainerProfile allocated this SID for this wrapper.
            unsafe { FreeSid(self.sid) };
            self.sid = null_mut();
        }
        Ok(())
    }
}

impl Drop for WindowsAppContainer {
    fn drop(&mut self) {
        if !self.deleted {
            // SAFETY: best-effort profile cleanup; explicit lifecycle cleanup reports errors.
            if unsafe { DeleteAppContainerProfile(self.name.as_ptr()) } >= 0 {
                self.deleted = true;
            }
        }
        if !self.sid.is_null() {
            // SAFETY: CreateAppContainerProfile allocated this SID for this wrapper.
            unsafe { FreeSid(self.sid) };
            self.sid = null_mut();
        }
    }
}

#[path = "windows_filesystem.rs"]
mod filesystem;
pub(super) use filesystem::WindowsFilesystemGrants;
#[cfg(test)]
pub(super) use filesystem::WindowsPathAcl;

#[path = "windows_process.rs"]
mod process;
pub(super) use process::{
    WindowsChildTermination, WindowsOutputCapture, WindowsStdioPipes, WindowsSuspendedChild,
};

fn unsupported_hresult(operation: &str, hresult: i32) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!(
            "Windows {operation} failed with HRESULT 0x{:08X}",
            hresult as u32
        ),
    )
}

fn unsupported_job(operation: &str) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!(
            "Windows {operation} failed: {}",
            std::io::Error::last_os_error()
        ),
    )
}

#[cfg(test)]
#[path = "windows_job_tests.rs"]
mod tests;
