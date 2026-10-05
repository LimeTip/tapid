#![cfg(windows)]
use super::*;
use crate::execution::windows_cancellation::WindowsCancellation;

struct ProcThreadAttributeList {
    storage: Vec<usize>,
    list: windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST,
    initialized: bool,
    // UpdateProcThreadAttribute retains these pointers until the list is deleted.
    security_capabilities: Box<SECURITY_CAPABILITIES>,
    inherited_handles: Option<Box<[HANDLE]>>,
}

impl ProcThreadAttributeList {
    fn for_appcontainer(
        container: &WindowsAppContainer,
        inherited_handles: &[HANDLE],
    ) -> Result<Self, ExecutionError> {
        let inherited_handles =
            (!inherited_handles.is_empty()).then(|| inherited_handles.to_vec().into_boxed_slice());
        let attribute_count = if inherited_handles.is_some() { 2 } else { 1 };
        let mut required = 0usize;
        // SAFETY: the null probe is the documented first call used to obtain the allocation size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), attribute_count, 0, &mut required) };
        if required == 0 {
            return Err(unsupported_job("size process attribute list"));
        }
        let slots = required
            .checked_add(size_of::<usize>() - 1)
            .ok_or_else(|| unsupported_job("allocate process attribute list"))?
            / size_of::<usize>();
        let mut storage = vec![0usize; slots];
        let list = storage.as_mut_ptr().cast();
        let mut actual = required;
        // SAFETY: storage is usize-aligned and large enough for the size returned by the probe.
        if unsafe { InitializeProcThreadAttributeList(list, attribute_count, 0, &mut actual) } == 0
        {
            return Err(unsupported_job("initialize process attribute list"));
        }
        let security_capabilities = Box::new(SECURITY_CAPABILITIES {
            AppContainerSid: container.sid(),
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        });
        let attributes = Self {
            storage,
            list,
            initialized: true,
            security_capabilities,
            inherited_handles,
        };
        // SAFETY: the boxed capabilities and attribute-list storage remain alive until the list is
        // deleted after CreateProcessW; moving the wrapper cannot relocate the boxed value.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (&*attributes.security_capabilities as *const SECURITY_CAPABILITIES)
                    .cast::<c_void>(),
                size_of::<SECURITY_CAPABILITIES>(),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            return Err(unsupported_job(
                "set AppContainer process security capabilities",
            ));
        }
        if let Some(handles) = &attributes.inherited_handles {
            // SAFETY: the boxed handle array remains alive through CreateProcessW; every listed
            // handle is inheritable and the child is restricted to exactly this standard-handle set.
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    attributes.list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast_mut().cast::<c_void>(),
                    std::mem::size_of_val(&**handles),
                    null_mut(),
                    null(),
                )
            };
            if updated == 0 {
                return Err(unsupported_job("set explicit child handle allowlist"));
            }
        }
        Ok(attributes)
    }
}

impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: list was initialized and remains backed by storage for its full lifetime.
            unsafe { DeleteProcThreadAttributeList(self.list) };
            self.initialized = false;
        }
        let _ = self.storage.len();
    }
}

/// Owns child standard handles and parent pipe readers until process creation completes.
pub struct WindowsStdioPipes {
    stdin_read: HANDLE,
    stdin_write: HANDLE,
    stdout_read: HANDLE,
    stdout_write: HANDLE,
    stderr_read: HANDLE,
    stderr_write: HANDLE,
}

impl WindowsStdioPipes {
    pub fn new() -> Result<Self, ExecutionError> {
        let mut pipes = Self {
            stdin_read: 0,
            stdin_write: 0,
            stdout_read: 0,
            stdout_write: 0,
            stderr_read: 0,
            stderr_write: 0,
        };
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                .expect("SECURITY_ATTRIBUTES size fits u32"),
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        // SAFETY: CreatePipe initializes the supplied handles; security attributes request
        // inheritable handles so CreateProcessW can pass only the explicit handle list.
        for (read, write) in [
            (&mut pipes.stdin_read, &mut pipes.stdin_write),
            (&mut pipes.stdout_read, &mut pipes.stdout_write),
            (&mut pipes.stderr_read, &mut pipes.stderr_write),
        ] {
            if unsafe { CreatePipe(read, write, &mut attributes, 0) } == 0 {
                return Err(unsupported_job("create child standard I/O pipe"));
            }
        }
        for handle in [pipes.stdout_read, pipes.stderr_read, pipes.stdin_write] {
            // SAFETY: each handle was created above and only its inheritance flag is changed.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(unsupported_job("restrict standard pipe handle inheritance"));
            }
        }
        // The runner supplies closed stdin; closing this writer makes the inherited read end report EOF.
        unsafe { CloseHandle(pipes.stdin_write) };
        pipes.stdin_write = 0;
        Ok(pipes)
    }

    fn child_handles(&self) -> [HANDLE; 3] {
        [self.stdin_read, self.stdout_write, self.stderr_write]
    }

    fn close_child_ends(&mut self) {
        for handle in [
            &mut self.stdin_read,
            &mut self.stdout_write,
            &mut self.stderr_write,
        ] {
            if *handle != 0 {
                // SAFETY: these are the parent copies of handles inherited by the child.
                unsafe { CloseHandle(*handle) };
                *handle = 0;
            }
        }
    }

    pub fn into_parent_readers(mut self) -> Result<(std::fs::File, std::fs::File), ExecutionError> {
        use std::os::windows::io::FromRawHandle;
        self.close_child_ends();
        let stdout = std::mem::replace(&mut self.stdout_read, 0);
        let stderr = std::mem::replace(&mut self.stderr_read, 0);
        if stdout == 0 || stderr == 0 {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows standard output pipe handles are incomplete",
            ));
        }
        // SAFETY: each handle is a valid uniquely owned pipe read handle transferred to File.
        Ok(unsafe {
            (
                std::fs::File::from_raw_handle(stdout as usize as *mut std::ffi::c_void),
                std::fs::File::from_raw_handle(stderr as usize as *mut std::ffi::c_void),
            )
        })
    }
}

impl Drop for WindowsStdioPipes {
    fn drop(&mut self) {
        for handle in [
            &mut self.stdin_read,
            &mut self.stdin_write,
            &mut self.stdout_read,
            &mut self.stdout_write,
            &mut self.stderr_read,
            &mut self.stderr_write,
        ] {
            if *handle != 0 {
                // SAFETY: this wrapper uniquely owns each nonzero handle.
                unsafe { CloseHandle(*handle) };
                *handle = 0;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsChildTermination {
    Exited(u32),
    TimedOut,
    OutputLimitExceeded,
    Cancelled,
}

/// Concurrently drains both output pipes while retaining at most the configured aggregate limit.
pub struct WindowsOutputCapture {
    stdout: JoinHandle<Result<Vec<u8>, ExecutionError>>,
    stderr: JoinHandle<Result<Vec<u8>, ExecutionError>>,
    output_limit_exceeded: Arc<AtomicBool>,
}

impl WindowsOutputCapture {
    pub fn start((stdout, stderr): (std::fs::File, std::fs::File), max_bytes: Option<u64>) -> Self {
        let total = Arc::new(AtomicU64::new(0));
        let output_limit_exceeded = Arc::new(AtomicBool::new(false));
        let stdout = spawn_capture_reader(
            stdout,
            Arc::clone(&total),
            Arc::clone(&output_limit_exceeded),
            max_bytes,
        );
        let stderr =
            spawn_capture_reader(stderr, total, Arc::clone(&output_limit_exceeded), max_bytes);
        Self {
            stdout,
            stderr,
            output_limit_exceeded,
        }
    }

    pub fn output_limit_exceeded(&self) -> &AtomicBool {
        &self.output_limit_exceeded
    }

    pub fn finish(self) -> Result<(Vec<u8>, Vec<u8>), ExecutionError> {
        let stdout = join_capture_reader(self.stdout)?;
        let stderr = join_capture_reader(self.stderr)?;
        Ok((stdout, stderr))
    }
}

fn spawn_capture_reader(
    mut reader: std::fs::File,
    total: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
    max_bytes: Option<u64>,
) -> JoinHandle<Result<Vec<u8>, ExecutionError>> {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut captured = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = reader.read(&mut buffer).map_err(|error| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    format!("failed reading Windows child output: {error}"),
                )
            })?;
            if count == 0 {
                break;
            }
            let accepted = match max_bytes {
                Some(limit) => {
                    let requested = u64::try_from(count).unwrap_or(u64::MAX);
                    let mut current = total.load(Ordering::Relaxed);
                    loop {
                        let accepted = requested.min(limit.saturating_sub(current));
                        match total.compare_exchange_weak(
                            current,
                            current.saturating_add(accepted),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => {
                                if accepted < requested {
                                    exceeded.store(true, Ordering::Release);
                                }
                                break usize::try_from(accepted).unwrap_or(usize::MAX);
                            }
                            Err(actual) => current = actual,
                        }
                    }
                }
                None => {
                    total.fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
                    count
                }
            };
            captured.extend_from_slice(&buffer[..accepted.min(count)]);
        }
        Ok(captured)
    })
}

fn join_capture_reader(
    reader: JoinHandle<Result<Vec<u8>, ExecutionError>>,
) -> Result<Vec<u8>, ExecutionError> {
    reader.join().map_err(|_| {
        ExecutionError::new(
            ExecutionErrorCategory::Internal,
            "Windows output reader thread panicked",
        )
    })?
}

/// Owns a process and thread created with CREATE_SUSPENDED and without inherited handles.
#[allow(dead_code)] // Used by the Windows launch lifecycle and native suspended-launch test.
pub struct WindowsSuspendedChild {
    process: HANDLE,
    thread: HANDLE,
    exited: bool,
}

#[allow(dead_code)]
impl WindowsSuspendedChild {
    pub fn create(
        container: &WindowsAppContainer,
        application: &[u16],
        command_line: &[u16],
        environment: &[u16],
        current_directory: &[u16],
    ) -> Result<Self, ExecutionError> {
        Self::create_inner(
            container,
            application,
            command_line,
            environment,
            current_directory,
            None,
        )
    }

    pub fn create_with_stdio(
        container: &WindowsAppContainer,
        application: &[u16],
        command_line: &[u16],
        environment: &[u16],
        current_directory: &[u16],
        stdio: &mut WindowsStdioPipes,
    ) -> Result<Self, ExecutionError> {
        Self::create_inner(
            container,
            application,
            command_line,
            environment,
            current_directory,
            Some(stdio),
        )
    }

    fn create_inner(
        container: &WindowsAppContainer,
        application: &[u16],
        command_line: &[u16],
        environment: &[u16],
        current_directory: &[u16],
        mut stdio: Option<&mut WindowsStdioPipes>,
    ) -> Result<Self, ExecutionError> {
        if !is_single_nul_terminated(application)
            || !is_single_nul_terminated(command_line)
            || !is_double_nul_terminated(environment)
            || !is_single_nul_terminated(current_directory)
        {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows suspended launch received a malformed UTF-16 buffer",
            ));
        }

        let inherited_handles = stdio
            .as_deref()
            .map(WindowsStdioPipes::child_handles)
            .unwrap_or([0; 3]);
        let handles_to_inherit = if stdio.is_some() {
            &inherited_handles[..]
        } else {
            &[]
        };
        let attributes = ProcThreadAttributeList::for_appcontainer(container, handles_to_inherit)?;
        let mut mutable_command_line = command_line.to_vec();
        // SAFETY: zero is the documented default for optional STARTUPINFOEXW fields.
        let mut startup = unsafe { zeroed::<STARTUPINFOEXW>() };
        startup.StartupInfo.cb =
            u32::try_from(size_of::<STARTUPINFOEXW>()).expect("STARTUPINFOEXW size fits u32");
        startup.lpAttributeList = attributes.list;
        if let Some(pipes) = stdio.as_deref() {
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = pipes.stdin_read;
            startup.StartupInfo.hStdOutput = pipes.stdout_write;
            startup.StartupInfo.hStdError = pipes.stderr_write;
        }
        // SAFETY: all pointers reference NUL-terminated owned buffers for the duration of the call;
        // the command line is mutable as required by CreateProcessW. The process is created with
        // the requested AppContainer token and suspended before user code. When pipes are supplied,
        // only the explicit standard-handle list is inherited.
        let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                mutable_command_line.as_mut_ptr(),
                null(),
                null(),
                if stdio.is_some() { 1 } else { 0 },
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_ptr().cast::<c_void>(),
                current_directory.as_ptr(),
                &startup.StartupInfo,
                &mut information,
            )
        };
        if created == 0 {
            return Err(unsupported_job("create suspended child"));
        }
        if let Some(pipes) = stdio.as_deref_mut() {
            pipes.close_child_ends();
        }
        if information.hProcess == 0 || information.hThread == 0 {
            if information.hProcess != 0 {
                // SAFETY: this handle was returned by CreateProcessW and the child is suspended.
                unsafe { TerminateProcess(information.hProcess, 1) };
                // SAFETY: this is the valid process handle returned by CreateProcessW.
                unsafe { CloseHandle(information.hProcess) };
            }
            if information.hThread != 0 {
                // SAFETY: this handle was returned by CreateProcessW.
                unsafe { CloseHandle(information.hThread) };
            }
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows returned incomplete handles for a suspended child",
            ));
        }
        let child = Self {
            process: information.hProcess,
            thread: information.hThread,
            exited: false,
        };
        child.verify_appcontainer(container.sid())?;
        Ok(child)
    }

    fn verify_appcontainer(
        &self,
        expected_sid: windows_sys::Win32::Foundation::PSID,
    ) -> Result<(), ExecutionError> {
        let mut token = 0;
        // SAFETY: process is a valid suspended child handle; token receives the queried handle.
        if unsafe { OpenProcessToken(self.process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(unsupported_job(
                "open child token for AppContainer verification",
            ));
        }
        let verification = (|| {
            let mut is_appcontainer = 0i32;
            let mut returned = 0u32;
            // SAFETY: the output buffer is a writable BOOL-sized value.
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenIsAppContainer,
                    (&mut is_appcontainer as *mut i32).cast::<c_void>(),
                    size_of::<i32>() as u32,
                    &mut returned,
                )
            } == 0
            {
                return Err(unsupported_job("query child AppContainer token state"));
            }
            if is_appcontainer == 0 {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows child token is not an AppContainer; refusing to resume it",
                ));
            }

            let mut required = 0u32;
            // SAFETY: null/zero is the documented size probe for GetTokenInformation.
            unsafe {
                GetTokenInformation(token, TokenAppContainerSid, null_mut(), 0, &mut required)
            };
            if required < size_of::<TOKEN_APPCONTAINER_INFORMATION>() as u32 {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows child token did not return an AppContainer SID",
                ));
            }
            let slots = (required as usize).div_ceil(size_of::<usize>());
            let mut buffer = vec![0usize; slots];
            // SAFETY: the usize-aligned buffer is at least the size reported by the probe.
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenAppContainerSid,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    required,
                    &mut returned,
                )
            } == 0
            {
                return Err(unsupported_job("query child AppContainer SID"));
            }
            // SAFETY: the output buffer begins with TOKEN_APPCONTAINER_INFORMATION.
            let info = unsafe { &*buffer.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>() };
            if info.TokenAppContainer.is_null()
                || unsafe { EqualSid(info.TokenAppContainer, expected_sid) } == 0
            {
                return Err(ExecutionError::new(
                    ExecutionErrorCategory::UnsupportedContainment,
                    "Windows child AppContainer SID differs from the requested profile",
                ));
            }
            Ok(())
        })();
        // SAFETY: token is the handle returned by OpenProcessToken above.
        unsafe { CloseHandle(token) };
        verification
    }

    pub fn process_handle(&self) -> HANDLE {
        self.process
    }

    #[cfg(test)]
    pub(super) fn wait_for_signal(&self, timeout_ms: u32) -> Result<bool, ExecutionError> {
        // SAFETY: the process handle is valid and timeout_ms is bounded by the test.
        match unsafe { WaitForSingleObject(self.process, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(unsupported_job("wait for child process termination")),
        }
    }

    pub fn resume_and_wait_for_exit(
        &mut self,
        job: &WindowsJob,
        timeout_ms: u32,
    ) -> Result<u32, ExecutionError> {
        let no_output_limit = AtomicBool::new(false);
        match self.resume_and_wait_for_status(job, timeout_ms, &no_output_limit, None)? {
            WindowsChildTermination::Exited(code) => Ok(code),
            WindowsChildTermination::TimedOut => Err(ExecutionError::new(
                ExecutionErrorCategory::Timeout,
                "Windows child exceeded its execution timeout; the complete Job Object was terminated",
            )),
            WindowsChildTermination::OutputLimitExceeded => Err(ExecutionError::new(
                ExecutionErrorCategory::OutputLimit,
                "Windows child exceeded its output limit; the complete Job Object was terminated",
            )),
            WindowsChildTermination::Cancelled => Err(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "Windows child cancellation requires an active cancellation scope",
            )),
        }
    }

    pub fn resume_and_wait_for_status(
        &mut self,
        job: &WindowsJob,
        timeout_ms: u32,
        output_limit_exceeded: &AtomicBool,
        cancellation: Option<&WindowsCancellation>,
    ) -> Result<WindowsChildTermination, ExecutionError> {
        // SAFETY: this thread handle belongs to the child created suspended by this wrapper.
        let previous_suspend_count = unsafe { ResumeThread(self.thread) };
        if previous_suspend_count != 1 {
            // The child must not be allowed to run if the expected suspended state is ambiguous.
            self.terminate_job_and_reap(job)?;
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                "Windows child did not have exactly one initial suspension count",
            ));
        }

        let started = Instant::now();
        loop {
            if cancellation.is_some_and(WindowsCancellation::is_cancelled) {
                self.terminate_job_and_reap(job)?;
                return Ok(WindowsChildTermination::Cancelled);
            }
            if output_limit_exceeded.load(Ordering::Acquire) {
                self.terminate_job_and_reap(job)?;
                return Ok(WindowsChildTermination::OutputLimitExceeded);
            }
            let elapsed_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
            if elapsed_ms >= timeout_ms {
                self.terminate_job_and_reap(job)?;
                return Ok(WindowsChildTermination::TimedOut);
            }
            let remaining_ms = timeout_ms - elapsed_ms;
            // SAFETY: the process handle is valid and the wait duration is a short bounded poll.
            match unsafe { WaitForSingleObject(self.process, remaining_ms.min(20).max(1)) } {
                WAIT_OBJECT_0 => {
                    self.exited = true;
                    let mut exit_code = 0;
                    // SAFETY: the root process is signaled and exit_code is a writable DWORD destination.
                    if unsafe { GetExitCodeProcess(self.process, &mut exit_code) } == 0 {
                        return Err(unsupported_job("read suspended child exit code"));
                    }
                    loop {
                        if cancellation.is_some_and(WindowsCancellation::is_cancelled) {
                            self.terminate_job_and_reap(job)?;
                            return Ok(WindowsChildTermination::Cancelled);
                        }
                        if output_limit_exceeded.load(Ordering::Acquire) {
                            self.terminate_job_and_reap(job)?;
                            return Ok(WindowsChildTermination::OutputLimitExceeded);
                        }
                        let elapsed_ms =
                            u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
                        if elapsed_ms >= timeout_ms {
                            self.terminate_job_and_reap(job)?;
                            return Ok(WindowsChildTermination::TimedOut);
                        }
                        match job.wait_until_empty(10) {
                            Ok(()) => return Ok(WindowsChildTermination::Exited(exit_code)),
                            Err(error) if error.category() == ExecutionErrorCategory::Timeout => {}
                            Err(error) => return Err(error),
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                WAIT_TIMEOUT => {}
                _ => {
                    self.terminate_job_and_reap(job)?;
                    return Err(ExecutionError::new(
                        ExecutionErrorCategory::UnsupportedContainment,
                        "Windows failed while waiting for the root process; the complete Job Object was terminated",
                    ));
                }
            }
        }
    }

    fn terminate_job_and_reap(&mut self, job: &WindowsJob) -> Result<(), ExecutionError> {
        let termination = job.terminate_all();
        if termination.is_err() {
            // SAFETY: fallback root termination keeps the owned process from escaping if Job Object
            // termination fails; the failure is still surfaced after the root process is reaped.
            unsafe { TerminateProcess(self.process, 1) };
        }
        // SAFETY: wait for the root process to signal, then poll the kernel-maintained active
        // process count. Job object handles are not signaled merely because members exit.
        let root_reaped = unsafe { WaitForSingleObject(self.process, 5_000) } == WAIT_OBJECT_0;
        self.exited = root_reaped;
        let tree_reaped = job.wait_until_empty(5_000).is_ok();
        if !root_reaped || !tree_reaped {
            return Err(unsupported_job(
                "reap child process tree after Job Object termination",
            ));
        }
        termination
    }
}

impl Drop for WindowsSuspendedChild {
    fn drop(&mut self) {
        if self.process != 0 && !self.exited {
            // SAFETY: this wrapper uniquely owns the process handle; terminate before closing it.
            unsafe { TerminateProcess(self.process, 1) };
            // SAFETY: bounded best-effort reap after forced termination.
            unsafe { WaitForSingleObject(self.process, 5_000) };
        }
        if self.thread != 0 {
            // SAFETY: this wrapper uniquely owns the thread handle.
            unsafe { CloseHandle(self.thread) };
        }
        if self.process != 0 {
            // SAFETY: this wrapper uniquely owns the process handle.
            unsafe { CloseHandle(self.process) };
        }
    }
}

fn is_single_nul_terminated(buffer: &[u16]) -> bool {
    buffer.len() >= 2 && buffer.last() == Some(&0) && !buffer[..buffer.len() - 1].contains(&0)
}

fn is_double_nul_terminated(buffer: &[u16]) -> bool {
    if buffer.len() < 2 || buffer[buffer.len() - 2..] != [0, 0] {
        return false;
    }
    let entries = &buffer[..buffer.len() - 2];
    entries.is_empty()
        || entries.split(|unit| *unit == 0).all(|entry| {
            !entry.is_empty()
                && entry
                    .iter()
                    .position(|unit| *unit == u16::from(b'='))
                    .is_some_and(|equals| equals > 0)
        })
}
