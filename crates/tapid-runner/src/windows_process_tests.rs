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
    let capture = tapid_test_support::TempProject::new("host-authority-capture")?;
    let stdout_path = capture.path().join("stdout");
    let stderr_path = capture.path().join("stderr");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path)?)
        .stderr(std::fs::File::create(&stderr_path)?);
    let mut child = command.spawn()?;
    let status = bounded_child_status(&mut child, timeout_ms)?;
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
fn scm_com_server_activates_and_removes_its_exact_registration() {
    for registry_read in [true, false] {
        scm_com_server_comparison(registry_read);
    }
}

fn scm_com_server_comparison(registry_read: bool) {
    let project = tapid_test_support::TempProject::new("scm-com-server").unwrap();
    let class = super::com_probe::unique_class();
    let executable = std::env::current_exe().unwrap();
    let program: Vec<_> = executable.as_os_str().encode_wide().collect();
    let arguments = [
        std::ffi::OsString::from("--tapid-test-com-server"),
        std::ffi::OsString::from(&class),
        project.path().as_os_str().to_owned(),
    ];
    let arguments: Vec<Vec<u16>> = arguments
        .iter()
        .map(|arg| arg.encode_wide().collect())
        .collect();
    let command =
        super::super::super::serialize_windows_command_line_units(&program, &arguments, false)
            .unwrap();
    let command = String::from_utf16(&command[..command.len() - 1]).unwrap();
    let mut registration = super::registry_probe::LocalServerRegistration::new(&class, &command);
    // A collision must not replace the command or take ownership of its key.
    assert!(
        std::panic::catch_unwind(|| {
            super::registry_probe::LocalServerRegistration::new(&class, "invalid-collision-command")
        })
        .is_err()
    );
    let mut client = std::process::Command::new(&executable);
    client
        .args([
            "--exact",
            &format!(
                "{}::provider_helper",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
        ])
        .env("TAPID_TEST_MARKER", "provider-host")
        .env("TAPID_TEST_COM_DESCENDANT", "1")
        .env("TAPID_TEST_COM_CLASS", &class);
    let root = std::fs::canonicalize(project.path()).unwrap();
    let program = root.join("scm-client.exe");
    std::fs::copy(&executable, &program).unwrap();
    let mut registry =
        super::registry_probe::RegistryInput::new(root.file_name().unwrap().to_str().unwrap());
    let mut contained_dispatches = 0;
    compare_lpac_com_clients(
        &root,
        &program,
        &class,
        &registry.path,
        &[registry_read],
        |observed| {
            contained_dispatches += observed;
        },
    );
    registry.cleanup();
    // Each token variant receives a fresh class with no running server. The
    // contained root attempts activation first; host activation afterward is
    // mandatory even when LPAC reports denial or class unavailability.
    // No direct server spawn: only COM requests can have started the fixture.
    let activated = bounded_host_output(&mut client, 5_000).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let pid: u32 = loop {
        if let Some(pid) = std::fs::read_to_string(project.path().join("ready"))
            .ok()
            // The newline is a completion marker: never interpret a partially
            // written decimal PID as a different process to supervise.
            .and_then(|text| text.strip_suffix('\n').and_then(|pid| pid.parse().ok()))
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "SCM server readiness was not published"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    // The isolated trusted fixture publishes its PID in its private directory.
    // Hold a process handle before requesting shutdown; never signal a PID later.
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    struct ServerProcess(HANDLE);
    impl Drop for ServerProcess {
        fn drop(&mut self) {
            unsafe {
                if WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT {
                    TerminateProcess(self.0, 1);
                    WaitForSingleObject(self.0, 5_000);
                }
                CloseHandle(self.0);
            }
        }
    }
    let server = ServerProcess(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            0,
            pid,
        )
    });
    assert_ne!(server.0, 0, "open SCM fixture process");
    assert!(
        activated.status.success(),
        "{}",
        String::from_utf8_lossy(&activated.stderr)
    );
    assert!(String::from_utf8_lossy(&activated.stdout).contains("com:descendant:activated"));
    assert!(String::from_utf8_lossy(&activated.stdout).contains("com:local-server:activated"));
    assert!(project.path().join("ready").is_file());
    project.write("stop", b"").unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(server.0, 5_000) },
        WAIT_OBJECT_0,
        "SCM fixture must exit after revocation"
    );
    let mut exit_code = 1;
    assert_ne!(unsafe { GetExitCodeProcess(server.0, &mut exit_code) }, 0);
    assert_eq!(exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(project.path().join("completed")).unwrap(),
        (2 + contained_dispatches).to_string()
    );
    registration.cleanup();
    client
        .env("TAPID_TEST_MARKER", "com-revoked")
        .env_remove("TAPID_TEST_COM_DESCENDANT");
    let revoked = bounded_host_output(&mut client, 5_000).unwrap();
    assert!(
        revoked.status.success(),
        "{}",
        String::from_utf8_lossy(&revoked.stderr)
    );
    assert!(
        String::from_utf8_lossy(&revoked.stdout).contains("com:local-server:class-unavailable")
    );
}

#[test]
fn standalone_com_server_activates_and_revokes_its_supplied_class() {
    let project = tapid_test_support::TempProject::new("standalone-com-server").unwrap();
    let class = super::com_probe::unique_class();
    let executable = std::env::current_exe().unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let mut command = std::process::Command::new(&executable);
            command
                .arg("--tapid-test-com-server")
                .arg(&class)
                .arg(project.path())
                .arg("-Embedding");
            bounded_host_output(&mut command, 15_000).unwrap()
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !project.path().join("ready").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "COM server did not register"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut client = std::process::Command::new(&executable);
        client
            .args([
                "--exact",
                &format!(
                    "{}::provider_helper",
                    module_path!().split_once("::").unwrap().1
                ),
                "--nocapture",
            ])
            .env("TAPID_TEST_MARKER", "com-descendant-host")
            .env("TAPID_TEST_COM_CLASS", &class);
        let activated = bounded_host_output(&mut client, 5_000).unwrap();
        project.write("stop", b"").unwrap();
        let output = server.join().unwrap();
        assert!(
            activated.status.success(),
            "{}",
            String::from_utf8_lossy(&activated.stderr)
        );
        assert!(String::from_utf8_lossy(&activated.stdout).contains("com:descendant:activated"));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "com:standalone:creations=1"
        );
        client.env("TAPID_TEST_MARKER", "com-revoked");
        let revoked = bounded_host_output(&mut client, 5_000).unwrap();
        assert!(
            revoked.status.success(),
            "{}",
            String::from_utf8_lossy(&revoked.stderr)
        );
        assert!(
            String::from_utf8_lossy(&revoked.stdout).contains("com:local-server:class-unavailable")
        );
    });
}

fn bounded_child_status(
    child: &mut std::process::Child,
    timeout_ms: u32,
) -> std::io::Result<std::process::ExitStatus> {
    use std::os::windows::io::AsRawHandle;
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
    child.wait()
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
fn host_authority_probe_preserves_output_and_rejects_each_oversized_stream() {
    let helper = format!(
        "{}::provider_helper",
        module_path!().split_once("::").unwrap().1
    );
    for marker in [
        "host-output",
        "host-stdout-overflow",
        "host-stderr-overflow",
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &helper, "--nocapture"])
            .env("TAPID_TEST_MARKER", marker);
        let result = bounded_host_output(&mut command, 10_000);
        if marker == "host-output" {
            let output = result.unwrap();
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("host-output:stdout"));
            assert!(String::from_utf8_lossy(&output.stderr).contains("host-output:stderr"));
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert_eq!(
                error.to_string(),
                "host authority probe output exceeded limit"
            );
        }
    }
}

#[test]
fn provider_helper() {
    if let Ok(marker @ ("com-descendant" | "com-descendant-host")) =
        std::env::var("TAPID_TEST_MARKER").as_deref()
    {
        super::com_probe::probe(marker);
        return;
    }
    // Direct writes bypass libtest capture so the supervisor observes the actual
    // native stdout/stderr streams, including stderr-only overflow.
    match std::env::var("TAPID_TEST_MARKER").as_deref() {
        Ok("host-output") => {
            use std::io::Write;
            std::io::stdout()
                .write_all(b"host-output:stdout\n")
                .unwrap();
            std::io::stderr()
                .write_all(b"host-output:stderr\n")
                .unwrap();
            return;
        }
        Ok(marker @ ("host-stdout-overflow" | "host-stderr-overflow")) => {
            use std::io::Write;
            let bytes = [b'x'; 16_385];
            if marker == "host-stdout-overflow" {
                std::io::stdout().write_all(&bytes).unwrap();
            } else {
                std::io::stderr().write_all(&bytes).unwrap();
            }
            return;
        }
        _ => {}
    }
    if std::env::var("TAPID_TEST_MARKER").as_deref() == Ok("provider-stalled") {
        std::thread::sleep(std::time::Duration::from_secs(30));
        panic!("stalled helper must be terminated");
    }
    super::registry_probe::probe();
    super::provider_probe::probe(&std::env::var("TAPID_TEST_MARKER").unwrap_or_default());
    if let Ok(marker) = std::env::var("TAPID_TEST_MARKER") {
        probe_com_activation(&marker);
        super::com_probe::probe(&marker);
        if std::env::var_os("TAPID_TEST_COM_DESCENDANT").is_some() {
            // The outer bounded supervisor owns the wait and, for LPAC runs,
            // the Job owns this child too. Inherit the controlled environment
            // and native output handles; never introduce a shell or host tool.
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!(
                        "{}::provider_helper",
                        module_path!().split_once("::").unwrap().1
                    ),
                    "--nocapture",
                ])
                .env_remove("TAPID_TEST_COM_DESCENDANT")
                .env(
                    "TAPID_TEST_MARKER",
                    if marker == "provider-host" {
                        "com-descendant-host"
                    } else {
                        "com-descendant"
                    },
                )
                .spawn()
                .unwrap();
            let status = bounded_child_status(&mut child, 3_000).unwrap();
            assert!(status.success(), "COM descendant failed: {status}");
        }
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
    let mut server = super::com_probe::LocalServer::new();
    let mut host_command = std::process::Command::new(&program);
    host_command
        .args(arguments)
        .env_clear()
        .env("SystemRoot", &system_root)
        .env("TAPID_TEST_REGISTRY", &registry.path)
        .env("TAPID_TEST_COM_CLASS", &server.class)
        .env("TAPID_TEST_COM_DESCENDANT", "1")
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
    assert!(String::from_utf8_lossy(&host.stdout).contains("com:local-server:activated"));
    assert!(String::from_utf8_lossy(&host.stdout).contains("com:descendant:activated"));
    assert_eq!(
        server.creations(),
        2,
        "host root and descendant requests must reach the Rust server"
    );
    let mut creations_before = server.creations();
    compare_lpac_com_clients(
        &root,
        &program,
        &server.class,
        &registry.path,
        &[true, false],
        |observed| {
            let creations = server.creations();
            assert_eq!(
                creations - creations_before,
                observed,
                "activation observation must match server-side dispatch"
            );
            creations_before = creations;
        },
    );
    registry.cleanup();
    server.cleanup();
    let creations_before = server.creations();
    host_command
        .env_remove("TAPID_TEST_REGISTRY")
        .env_remove("TAPID_TEST_COM_DESCENDANT")
        .env("TAPID_TEST_MARKER", "com-revoked");
    let revoked = bounded_host_output(&mut host_command, 10_000).unwrap();
    assert!(
        revoked.status.success(),
        "{}",
        String::from_utf8_lossy(&revoked.stderr)
    );
    assert!(
        String::from_utf8_lossy(&revoked.stdout).contains("com:local-server:class-unavailable")
    );
    assert_eq!(server.creations(), creations_before);
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

// Shared native launch path keeps SCM and running-class comparisons under the
// same verified suspended token, explicit grants and non-breakaway Job.
fn compare_lpac_com_clients(
    root: &std::path::Path,
    program: &std::path::Path,
    class: &str,
    registry_path: &str,
    registry_variants: &[bool],
    mut observe_dispatches: impl FnMut(u32),
) {
    let helper = format!(
        "{}::provider_helper",
        module_path!().split_once("::").unwrap().1
    );
    let arguments = ["--exact", helper.as_str(), "--nocapture"];
    let system_root = std::env::var_os("SystemRoot").unwrap();
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
    for &registry_read in registry_variants {
        let environment = super::super::super::windows_environment_block_units(
            &std::collections::BTreeMap::from([
                (std::ffi::OsString::from("SystemRoot"), system_root.clone()),
                (
                    std::ffi::OsString::from("TAPID_TEST_COM_DESCENDANT"),
                    std::ffi::OsString::from("1"),
                ),
                (
                    std::ffi::OsString::from("TAPID_TEST_COM_CLASS"),
                    std::ffi::OsString::from(class),
                ),
                (
                    std::ffi::OsString::from("TAPID_TEST_REGISTRY"),
                    std::ffi::OsString::from(registry_path),
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
                path: root.to_path_buf(),
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
        assert!(stdout.contains("com:local-server:"), "{stdout}");
        assert_eq!(stdout.matches("com:descendant:").count(), 1, "{stdout}");
        observe_dispatches(
            (stdout.matches("com:local-server:activated").count()
                + stdout.matches("com:descendant:activated").count()) as u32,
        );
        println!("registryRead={registry_read}: {stdout}");
        drop(child);
        job.wait_until_empty(5_000).unwrap();
        grants.restore().unwrap();
        container.cleanup().unwrap();
    }
}
