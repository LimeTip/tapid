use super::*;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Security::TokenCapabilities;

// File-backed capture avoids waiting for pipe EOF if a COM server retains a
// standard handle. This host control launches only the isolated Rust helper;
// it is not an execution boundary for untrusted code.
fn bounded_host_output(
    command: &mut std::process::Command,
    timeout_ms: u32,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::os::windows::io::AsRawHandle;
    let capture = tapid_test_support::TempProject::new("host-authority-capture")?;
    let stdout_path = capture.path().join("stdout");
    let stderr_path = capture.path().join("stderr");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path)?)
        .stderr(std::fs::File::create(&stderr_path)?);
    let mut child = command.spawn()?;
    // SAFETY: Child owns the live process handle throughout both waits.
    let waited = unsafe { WaitForSingleObject(child.as_raw_handle() as _, timeout_ms) };
    if waited != WAIT_OBJECT_0 {
        child.kill()?;
        let reaped = unsafe { WaitForSingleObject(child.as_raw_handle() as _, 5_000) };
        if reaped != WAIT_OBJECT_0 {
            return Err(std::io::Error::other(
                "host probe termination was not observed",
            ));
        }
        child.wait()?;
        return Err(std::io::Error::new(
            if waited == WAIT_TIMEOUT {
                std::io::ErrorKind::TimedOut
            } else {
                std::io::ErrorKind::Other
            },
            "host authority probe did not complete within its bounded wait",
        ));
    }
    let status = child.wait()?;
    let read = |path: &std::path::Path| -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(16_385)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16_384 {
            return Err(std::io::Error::other(
                "host authority probe output exceeded limit",
            ));
        }
        Ok(bytes)
    };
    Ok(std::process::Output {
        status,
        stdout: read(&stdout_path)?,
        stderr: read(&stderr_path)?,
    })
}

#[test]
fn host_authority_probe_timeout_terminates_and_reaps_helper() {
    let helper = format!(
        "{}::provider_helper",
        module_path!().split_once("::").unwrap().1
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &helper])
        .env("TAPID_TEST_MARKER", "provider-stalled");
    let started = std::time::Instant::now();
    let error = bounded_host_output(&mut command, 50).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn provider_helper() {
    if std::env::var("TAPID_TEST_MARKER").as_deref() == Ok("provider-stalled") {
        std::thread::sleep(std::time::Duration::from_secs(30));
        panic!("stalled helper must be terminated");
    }
    super::registry_probe::probe();
    super::provider_probe::probe(&std::env::var("TAPID_TEST_MARKER").unwrap_or_default());
    if let Ok(marker) = std::env::var("TAPID_TEST_MARKER") {
        probe_com_activation(&marker);
    }
}

#[test]
fn lpac_provider_initialization_compares_registry_read_and_capability_free_tokens() {
    let project = tapid_test_support::TempProject::new("lpac-provider-comparison").unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    let program = root.join("provider.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    let helper = format!(
        "{}::provider_helper",
        module_path!().split_once("::").unwrap().1
    );
    let arguments = ["--exact", helper.as_str(), "--nocapture"];
    let mut registry =
        super::registry_probe::RegistryInput::new(root.file_name().unwrap().to_str().unwrap());
    let system_root = std::env::var_os("SystemRoot").unwrap();
    let mut host_command = std::process::Command::new(&program);
    host_command
        .args(arguments)
        .env_clear()
        .env("SystemRoot", &system_root)
        .env("TAPID_TEST_REGISTRY", &registry.path)
        .env("TAPID_TEST_MARKER", "provider-host");
    let host = bounded_host_output(&mut host_command, 10_000).unwrap();
    assert!(
        host.status.success(),
        "{}",
        String::from_utf8_lossy(&host.stderr)
    );
    assert!(String::from_utf8_lossy(&host.stdout).contains("provider:host:socket-created"));
    assert!(String::from_utf8_lossy(&host.stdout).contains("registry:host-input:readable"));
    assert!(String::from_utf8_lossy(&host.stdout).contains("com:shell-link:activated"));
    let program_units: Vec<_> = program.as_os_str().encode_wide().collect();
    let argument_units: Vec<Vec<u16>> = arguments
        .iter()
        .map(|arg| arg.encode_utf16().collect())
        .collect();
    let command = super::super::super::serialize_windows_command_line_units(
        &program_units,
        &argument_units,
        false,
    )
    .unwrap();
    let application: Vec<_> = program_units.into_iter().chain(Some(0)).collect();
    let cwd: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    for registry_read in [true, false] {
        let environment = super::super::super::windows_environment_block_units(
            &std::collections::BTreeMap::from([
                (std::ffi::OsString::from("SystemRoot"), system_root.clone()),
                (
                    std::ffi::OsString::from("TAPID_TEST_REGISTRY"),
                    std::ffi::OsString::from(&registry.path),
                ),
                (
                    std::ffi::OsString::from("TAPID_TEST_MARKER"),
                    std::ffi::OsString::from(if registry_read {
                        "provider-contained"
                    } else {
                        "provider-capability-free"
                    }),
                ),
            ]),
        )
        .unwrap();

        let mut container = WindowsAppContainer::create().unwrap();
        let job = WindowsJob::new(&ExecutionLimits::default(), false).unwrap();
        let mut grants = super::super::WindowsFilesystemGrants::apply(
            &container,
            &[crate::execution::ResolvedFilesystemGrant {
                path: root.clone(),
                access: FilesystemAccess::Read,
                kind: FilesystemGrantKind::DirectorySubtree,
                source: crate::execution::FilesystemGrantSource::ProjectPolicy,
                binding: crate::execution::FilesystemBindingMode::CanonicalPath,
            }],
        )
        .unwrap();
        let mut pipes = WindowsStdioPipes::new().unwrap();
        let capability = registry_read
            .then(super::super::token::RegistryReadCapability::new)
            .transpose()
            .unwrap();
        let mut child = WindowsSuspendedChild::create_inner(
            &container,
            &application,
            &command,
            &environment,
            &cwd,
            Some(&mut pipes),
            capability,
        )
        .expect("LPAC creation and actual-token verification");
        job.assign_suspended_process(child.process_handle())
            .unwrap();
        let mut token = 0;
        // SAFETY: child owns a live suspended process; output receives a query handle.
        assert_ne!(
            unsafe { OpenProcessToken(child.process_handle(), TOKEN_QUERY, &mut token) },
            0
        );
        let information = super::super::token_information(token, TokenCapabilities);
        let capabilities_valid = super::super::token::validate_runtime_capabilities(token);
        // SAFETY: close the query handle before asserting on the captured result.
        unsafe { CloseHandle(token) };
        capabilities_valid.unwrap();
        let information = information.unwrap();
        assert!(!information.is_empty());
        // SAFETY: token_information returns aligned storage containing the native count.
        let count = unsafe { *information.as_ptr().cast::<u32>() };
        assert_eq!(count, u32::from(registry_read));
        println!("LPAC:registryRead={registry_read}:capability-count={count}");
        let capture =
            WindowsOutputCapture::start(pipes.into_parent_readers().unwrap(), Some(16384));
        let termination = child
            .resume_and_wait_for_status(&job, 10_000, capture.output_limit_exceeded(), None)
            .unwrap();
        let (stdout, stderr) = capture.finish().unwrap();
        assert_eq!(
            termination,
            WindowsChildTermination::Exited(0),
            "registryRead={registry_read}: stdout={}; stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        let stdout = String::from_utf8_lossy(&stdout);
        assert!(stdout.contains("provider:startup:ok"), "{stdout}");
        assert!(stdout.contains("provider:contained:socket-"), "{stdout}");
        assert!(stdout.contains("registry:host-input:"), "{stdout}");
        assert!(stdout.contains("com:shell-link:"), "{stdout}");
        println!("registryRead={registry_read}: {stdout}");
        drop(child);
        job.wait_until_empty(5_000).unwrap();
        grants.restore().unwrap();
        container.cleanup().unwrap();
    }
    registry.cleanup();
}

// Runs only in the disposable helper, after the existing provider probes. No shell
// method is invoked: this samples activation, not broker response authority.
fn probe_com_activation(marker: &str) {
    use std::ffi::c_void;
    use windows_sys::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            UI::Shell::ShellLink,
        },
        core::GUID,
    };
    if !matches!(
        marker,
        "provider-host" | "provider-contained" | "provider-capability-free"
    ) {
        return;
    }
    // SAFETY: this helper's test thread has no prior COM use; balance each
    // successful initialization (including S_FALSE) on the same thread.
    let initialized = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    assert!(initialized >= 0, "COM initialization: {initialized:#010x}");
    let mut object = std::ptr::null_mut();
    let unknown = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
    // SAFETY: valid CLSID/IID and writable output; only in-process activation
    // is requested, never remote activation or an interactive shell operation.
    let result = unsafe {
        CoCreateInstance(
            &ShellLink,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &unknown,
            &mut object,
        )
    };
    if result >= 0 {
        assert!(!object.is_null());
        // IUnknown has QueryInterface, AddRef, Release in that order. The
        // unused slots preserve layout; only the typed Release entry is called.
        #[repr(C)]
        struct UnknownVtable {
            query_interface: *const c_void,
            add_ref: *const c_void,
            release: unsafe extern "system" fn(*mut c_void) -> u32,
        }
        // SAFETY: successful IID_IUnknown activation owns one reference. Release
        // it before uninitializing COM; never access the object afterward.
        unsafe {
            let vtable = *object.cast::<*const UnknownVtable>();
            ((*vtable).release)(object);
        }
    }
    // SAFETY: initialization succeeded on this thread and object was released.
    unsafe { CoUninitialize() };
    if marker == "provider-host" {
        assert_eq!(result, 0, "host ShellLink fixture: {result:#010x}");
    }
    match result as u32 {
        0 => println!("com:shell-link:activated"),
        0x80070005 => println!("com:shell-link:access-denied"),
        0x80040154 => println!("com:shell-link:class-unavailable"),
        _ => panic!("unexpected ShellLink activation result: {result:#010x}"),
    }
}
