use super::*;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Security::TokenCapabilities;

#[test]
fn provider_helper() {
    super::provider_probe::probe(&std::env::var("TAPID_TEST_MARKER").unwrap_or_default());
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
    let system_root = std::env::var_os("SystemRoot").unwrap();
    let host = std::process::Command::new(&program)
        .args(arguments)
        .env_clear()
        .env("SystemRoot", &system_root)
        .env("TAPID_TEST_MARKER", "provider-host")
        .output()
        .unwrap();
    assert!(
        host.status.success(),
        "{}",
        String::from_utf8_lossy(&host.stderr)
    );
    assert!(String::from_utf8_lossy(&host.stdout).contains("provider:host:socket-created"));
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
        println!("registryRead={registry_read}: {stdout}");
        drop(child);
        job.wait_until_empty(5_000).unwrap();
        grants.restore().unwrap();
        container.cleanup().unwrap();
    }
}
