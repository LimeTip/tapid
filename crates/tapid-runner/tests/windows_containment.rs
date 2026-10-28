#![cfg(windows)]

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tapid_runner::{
    AssuranceLevel, CleanupConfidence, ExecutionErrorCategory, ExecutionLimits, ExecutionRequest,
    FilesystemPolicy, SandboxMode, SandboxPolicy, Termination, execute,
};

#[path = "support/windows_acl.rs"]
mod windows_acl;
#[path = "support/windows_node.rs"]
mod windows_node;

fn temporary_project(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("tapid-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    // Initialize ONLY this disposable root before any exact restoration snapshot.
    windows_acl::initialize_inheritance(&path);
    path
}

#[derive(Debug, PartialEq, Eq)]
struct DaclSnapshot {
    listing: Vec<u8>,
    control_and_acl: (u16, Vec<u8>),
}

impl std::ops::Deref for DaclSnapshot {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.listing
    }
}

fn project_dacl(path: &std::path::Path) -> DaclSnapshot {
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let icacls = PathBuf::from(system_root).join("System32/icacls.exe");
    let output = Command::new(icacls).arg(path).output().unwrap();
    assert!(
        output.status.success(),
        "icacls failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    DaclSnapshot {
        listing: output.stdout,
        control_and_acl: windows_acl::read_acl(path),
    }
}

fn managed_policy() -> SandboxPolicy {
    managed_policy_with_limits(
        ExecutionLimits::new(Some(30), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    )
}

fn managed_policy_with_limits(limits: ExecutionLimits) -> SandboxPolicy {
    managed_policy_with_flags(limits, false, false)
}

fn managed_policy_with_flags(
    limits: ExecutionLimits,
    allow_subprocess: bool,
    network: bool,
) -> SandboxPolicy {
    SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
        network,
        vec!["TAPID_TEST_MARKER".into()],
        allow_subprocess,
        limits,
    )
    .unwrap()
}

fn command_request(root: &PathBuf, command: &str, limits: ExecutionLimits) -> ExecutionRequest {
    command_request_with_flags(root, command, limits, false, false)
}

fn command_request_with_network(
    root: &PathBuf,
    command: &str,
    limits: ExecutionLimits,
    network: bool,
) -> ExecutionRequest {
    command_request_with_flags(root, command, limits, false, network)
}

fn command_request_with_flags(
    root: &PathBuf,
    command: &str,
    limits: ExecutionLimits,
    allow_subprocess: bool,
    network: bool,
) -> ExecutionRequest {
    command_request_with_policy(
        root,
        command,
        managed_policy_with_flags(limits, allow_subprocess, network),
    )
}

fn command_request_with_policy(
    root: &PathBuf,
    command: &str,
    policy: SandboxPolicy,
) -> ExecutionRequest {
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let mut arguments: Vec<OsString> = vec!["/D".into(), "/S".into(), "/C".into()];
    arguments.push(command.into());
    ExecutionRequest::builder(system32.join("cmd.exe").into_os_string())
        .args(arguments)
        .executable_search_path(&system32)
        .project_root(root)
        .policy(policy)
        .windows_verbatim_arguments(true)
        .build()
        .unwrap()
}

#[test]
fn windows_socket_provider_initialization_with_registry_read_capability() {
    let project = tapid_test_support::TempProject::new("windows-provider-inputs").unwrap();
    windows_acl::initialize_inheritance(project.path());
    let root = fs::canonicalize(project.path()).unwrap();
    let child = root.join("provider-probe.exe");
    fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
    let arguments = [
        "--exact",
        "windows_socket_provider_probe_helper",
        "--nocapture",
    ];
    let host = Command::new(&child)
        .args(arguments)
        .env("TAPID_TEST_MARKER", "provider-host")
        .output()
        .unwrap();
    assert!(
        host.status.success(),
        "{}",
        String::from_utf8_lossy(&host.stderr)
    );
    assert!(String::from_utf8_lossy(&host.stdout).contains("provider:host:socket-created"));
    let request = ExecutionRequest::builder(child.as_os_str())
        .args(arguments)
        .executable_search_path(&root)
        .project_root(&root)
        .policy(managed_policy())
        .env("TAPID_TEST_MARKER", "provider-contained")
        .build()
        .unwrap();
    let outcome = execute(&request).expect("provider probe must execute");
    let stdout = String::from_utf8_lossy(outcome.stdout());
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout={stdout}; stderr={}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(stdout.contains("provider:startup:ok"), "{stdout}");
    assert!(stdout.contains("provider:contained:socket-"), "{stdout}");
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    println!("{stdout}");
}

#[test]
fn windows_socket_provider_probe_helper() {
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, INVALID_SOCKET, IPPROTO_TCP, SOCK_STREAM, WSA_FLAG_NO_HANDLE_INHERIT,
        WSACleanup, WSADATA, WSAEACCES, WSAGetLastError, WSASocketW, WSAStartup, closesocket,
    };
    let mode = std::env::var("TAPID_TEST_MARKER").unwrap_or_default();
    if !matches!(mode.as_str(), "provider-host" | "provider-contained") {
        return;
    }
    // SAFETY: WSADATA is an output structure; successful startup is balanced below.
    let mut data: WSADATA = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { WSAStartup(0x202, &mut data) },
        0,
        "provider startup"
    );
    println!("provider:startup:ok");
    for family in [AF_INET, AF_INET6] {
        // SAFETY: fixed TCP parameters, no protocol pointer or inherited handle.
        // No bind, connect, DNS lookup, or external network service is used.
        let socket = unsafe {
            WSASocketW(
                family as i32,
                SOCK_STREAM,
                IPPROTO_TCP,
                std::ptr::null(),
                0,
                WSA_FLAG_NO_HANDLE_INHERIT,
            )
        };
        if socket == INVALID_SOCKET {
            // SAFETY: read the error immediately after the failed socket operation.
            let error = unsafe { WSAGetLastError() };
            assert_eq!(mode, "provider-contained", "host positive control: {error}");
            assert_eq!(error, WSAEACCES, "unexpected provider failure");
            println!("provider:contained:socket-denied:{family}");
        } else {
            // SAFETY: close exactly the socket returned above.
            assert_eq!(unsafe { closesocket(socket) }, 0);
            let label = if mode == "provider-host" {
                "host"
            } else {
                "contained"
            };
            println!("provider:{label}:socket-created:{family}");
        }
    }
    // SAFETY: balances this helper's successful WSAStartup call.
    assert_eq!(unsafe { WSACleanup() }, 0);
}

#[test]
fn windows_profile_storage_is_fresh_and_not_readable_by_the_next_execution() {
    let project = tapid_test_support::TempProject::new("windows-profile-isolation").unwrap();
    windows_acl::initialize_inheritance(project.path());
    let root = fs::canonicalize(project.path()).unwrap();
    let child = root.join("profile-probe.exe");
    fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
    let mut profiles = Vec::new();
    for mode in ["profile-first", "profile-second"] {
        let request = ExecutionRequest::builder(child.as_os_str())
            .args([
                "--exact",
                "windows_profile_storage_probe_helper",
                "--nocapture",
            ])
            .executable_search_path(&root)
            .project_root(&root)
            .policy(managed_policy())
            .env("TAPID_TEST_MARKER", mode)
            .build()
            .unwrap();
        let outcome = execute(&request).expect("profile probe must execute");
        let stdout = String::from_utf8_lossy(outcome.stdout());
        assert_eq!(
            outcome.termination(),
            &Termination::Exited(0),
            "stdout={stdout}; stderr={}",
            String::from_utf8_lossy(outcome.stderr())
        );
        assert_eq!(
            outcome.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        if mode == "profile-second" {
            assert!(
                stdout.contains("previous-profile-input:unavailable"),
                "{stdout}"
            );
        }
        let profile = stdout
            .lines()
            .find_map(|line| line.strip_prefix("profile-storage:"))
            .expect("actual-token profile path");
        let profile = PathBuf::from(profile);
        assert!(profile.is_absolute());
        assert!(
            !profile.join("tapid-cross-run-input").try_exists().unwrap(),
            "profile sentinel survived cleanup"
        );
        profiles.push(profile.clone());
        project
            .write(
                "previous-profile.txt",
                profile.as_os_str().to_str().unwrap().as_bytes(),
            )
            .unwrap();
    }
    assert_ne!(
        profiles[0], profiles[1],
        "profile storage must not be reused"
    );
}

#[test]
fn windows_profile_storage_probe_helper() {
    let mode = std::env::var("TAPID_TEST_MARKER").unwrap_or_default();
    if !matches!(mode.as_str(), "profile-first" | "profile-second") {
        return;
    }
    let profile = current_appcontainer_storage();
    let sentinel = profile.join("tapid-cross-run-input");
    assert!(
        !sentinel.try_exists().unwrap(),
        "fresh profile contains a previous sentinel"
    );
    fs::write(&sentinel, b"private-profile-input").expect("own profile write control");
    assert_eq!(fs::read(&sentinel).unwrap(), b"private-profile-input");
    if mode == "profile-second" {
        let previous =
            fs::read_to_string("previous-profile.txt").expect("prior profile path input");
        assert_ne!(profile, PathBuf::from(&previous));
        let error = fs::read(PathBuf::from(previous).join("tapid-cross-run-input"))
            .expect_err("prior profile must not provide an input");
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
            ),
            "{error}"
        );
        println!("previous-profile-input:unavailable");
    }
    println!("profile-storage:{}", profile.display());
}

fn current_appcontainer_storage() -> PathBuf {
    use std::{mem::size_of, os::windows::ffi::OsStringExt, ptr::null_mut};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation,
            Isolation::GetAppContainerFolderPath, TOKEN_APPCONTAINER_INFORMATION, TOKEN_QUERY,
            TokenAppContainerSid,
        },
        System::{
            Com::CoTaskMemFree,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    let mut token = 0;
    // SAFETY: valid current-process pseudo-handle and writable output handle.
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    let mut required = 0;
    // SAFETY: null/zero requests the required buffer size.
    unsafe { GetTokenInformation(token, TokenAppContainerSid, null_mut(), 0, &mut required) };
    assert!(required as usize >= size_of::<TOKEN_APPCONTAINER_INFORMATION>());
    let mut buffer = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
    // SAFETY: aligned buffer has the size returned by the API.
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenAppContainerSid,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    };
    // SAFETY: release the handle opened above, after the query.
    unsafe { CloseHandle(token) };
    assert_ne!(queried, 0);
    // SAFETY: successful query returned TOKEN_APPCONTAINER_INFORMATION and its SID.
    let info = unsafe { &*buffer.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>() };
    assert!(!info.TokenAppContainer.is_null());
    let mut sid = null_mut();
    // SAFETY: SID remains owned by buffer through conversion.
    assert_ne!(
        unsafe { ConvertSidToStringSidW(info.TokenAppContainer, &mut sid) },
        0
    );
    let mut path = null_mut();
    // SAFETY: sid is a NUL-terminated API allocation; path is a writable output.
    let result = unsafe { GetAppContainerFolderPath(sid, &mut path) };
    // SAFETY: release exactly the string allocated by conversion.
    unsafe { LocalFree(sid.cast()) };
    assert!(result >= 0, "profile folder query: {result:#x}");
    assert!(!path.is_null());
    let mut length = 0;
    // SAFETY: successful API output is a NUL-terminated UTF-16 string.
    while unsafe { *path.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the scan established this initialized slice's length.
    let owned = OsString::from_wide(unsafe { std::slice::from_raw_parts(path, length) });
    // SAFETY: this API's output is allocated with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(path.cast()) };
    PathBuf::from(owned)
}

#[test]
fn windows_denies_ambient_all_application_packages_reads_and_execution_in_root_and_descendants() {
    ambient_authority_probe("AAP", "*S-1-15-2-1:(OI)(CI)(RX)");
}

#[test]
fn windows_inventories_all_restricted_application_packages_authority() {
    // Characterization, not a denial claim: LPAC's AAP opt-out does not prove
    // exclusion of ARAP. Record each operation for the authority checkpoint.
    ambient_authority_probe("ARAP", "*S-1-15-2-2:(OI)(CI)(RX)");
}

fn ambient_authority_probe(trustee: &str, grant: &str) {
    let project = tapid_test_support::TempProject::new("windows-lpac-project").unwrap();
    let outside = tapid_test_support::TempProject::new("windows-lpac-outside").unwrap();
    project.write("approved.txt", b"approved-input").unwrap();
    let secret = outside.write("undeclared.txt", b"ambient-input").unwrap();
    let executable = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let undeclared_executable = outside.path().join("undeclared.exe");
    fs::copy(&executable, &undeclared_executable).unwrap();
    // Reuse the native addon fixture: it resolves Node exports only when called,
    // so ordinary LoadLibrary/GetProcAddress can validate it without Node.
    let undeclared_library = outside.path().join("undeclared.dll");
    windows_node::compile_launcher(&undeclared_library);
    fs::copy(&undeclared_library, project.path().join("approved.dll")).unwrap();
    windows_acl::initialize_inheritance(project.path());
    windows_acl::initialize_inheritance(outside.path());
    let system32 =
        fs::canonicalize(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"))
            .unwrap();
    let grant = Command::new(system32.join("icacls.exe"))
        .arg(outside.path())
        .args(["/grant", grant])
        .output()
        .unwrap();
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    // The host can read the fixture. Its added package-side grant is ambient;
    // neither this file nor its directory appears in the execution policy.
    assert_eq!(fs::read(&secret).unwrap(), b"ambient-input");
    // A valid PE image and successful host launch distinguish access denial
    // from a missing executable, loader dependency, or invalid fixture.
    let host_control = Command::new(&undeclared_executable)
        .args([
            "--exact",
            "windows_ambient_execution_target_helper",
            "--nocapture",
        ])
        .env("TAPID_TEST_MARKER", "ambient-target")
        .env("TAPID_TEST_LIBRARY", &undeclared_library)
        .output()
        .unwrap();
    assert!(host_control.status.success(), "{host_control:?}");
    assert!(String::from_utf8_lossy(&host_control.stdout).contains("undeclared-target-executed"));
    assert!(String::from_utf8_lossy(&host_control.stdout).contains("host-dynamic-library-valid"));
    let baseline = project_dacl(outside.path());
    let root = fs::canonicalize(project.path()).unwrap();
    let child = root.join("probe.exe");
    fs::copy(&executable, &child).unwrap();
    project
        .write("ambient-trustee.txt", trustee.as_bytes())
        .unwrap();
    project
        .write("outside-path.txt", secret.to_str().unwrap().as_bytes())
        .unwrap();
    project
        .write(
            "outside-executable.txt",
            undeclared_executable.to_str().unwrap().as_bytes(),
        )
        .unwrap();
    project
        .write(
            "outside-library.txt",
            undeclared_library.to_str().unwrap().as_bytes(),
        )
        .unwrap();
    let request = ExecutionRequest::builder(child.as_os_str())
        .args([
            "--exact",
            "windows_ambient_read_probe_helper",
            "--nocapture",
        ])
        .executable_search_path(&root)
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(30), Some(8192), Some(4), None).unwrap(),
            true,
            false,
        ))
        .env("TAPID_TEST_MARKER", "ambient-root")
        .build()
        .unwrap();
    let outcome = execute(&request).expect("approved root and descendant must execute");
    let stdout = String::from_utf8_lossy(outcome.stdout());
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout={stdout}; stderr={}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(
        stdout.contains("approved-root"),
        "root positive control missing: {stdout}"
    );
    assert!(
        stdout.contains("approved-child"),
        "descendant positive control missing: {stdout}"
    );
    for mode in ["ambient-root", "ambient-child"] {
        for operation in ["read", "execute", "load-library", "search-library"] {
            let denied = format!("{trustee}:{mode}:{operation}:denied");
            let allowed = format!("{trustee}:{mode}:{operation}:allowed");
            if trustee == "AAP" {
                assert!(stdout.contains(&denied), "{stdout}");
                assert!(!stdout.contains(&allowed), "{stdout}");
            } else {
                assert!(
                    stdout.contains(&denied) ^ stdout.contains(&allowed),
                    "{stdout}"
                );
            }
        }
    }
    // --show-output in the native workspace lane retains these observations.
    println!("ambient authority inventory ({trustee}):\n{stdout}");
    assert_eq!(project_dacl(outside.path()), baseline);
}

#[test]
fn windows_ambient_read_probe_helper() {
    let mode = std::env::var("TAPID_TEST_MARKER").unwrap_or_default();
    if mode != "ambient-root" && mode != "ambient-child" {
        return;
    }
    assert_eq!(fs::read("approved.txt").unwrap(), b"approved-input");
    let trustee = fs::read_to_string("ambient-trustee.txt").unwrap();
    assert!(matches!(trustee.as_str(), "AAP" | "ARAP"));
    let outside = fs::read_to_string("outside-path.txt").unwrap();
    match fs::read(outside) {
        Ok(bytes) => {
            assert_eq!(bytes, b"ambient-input");
            assert_ne!(trustee, "AAP", "undeclared AAP file must be denied");
            println!("{trustee}:{mode}:read:allowed");
        }
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            println!("{trustee}:{mode}:read:denied");
        }
    }
    let outside_executable = fs::read_to_string("outside-executable.txt").unwrap();
    let launch = Command::new(outside_executable)
        .args([
            "--exact",
            "windows_ambient_execution_target_helper",
            "--nocapture",
        ])
        .env("TAPID_TEST_MARKER", "ambient-target")
        .output();
    match launch {
        Ok(output) => {
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("undeclared-target-executed"));
            assert_ne!(trustee, "AAP", "undeclared AAP executable must be denied");
            println!("{trustee}:{mode}:execute:allowed");
        }
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            println!("{trustee}:{mode}:execute:denied");
        }
    }
    load_authority_probe_library(&fs::canonicalize("approved.dll").unwrap())
        .expect("approved dynamic DLL must load");
    load_searched_authority_probe_library(&fs::canonicalize("approved.dll").unwrap())
        .expect("approved searched DLL must load");
    let outside_library = fs::read_to_string("outside-library.txt").unwrap();
    let outside_library = std::path::Path::new(&outside_library);
    for (operation, result) in [
        (
            "load-library",
            load_authority_probe_library(outside_library),
        ),
        (
            "search-library",
            load_searched_authority_probe_library(outside_library),
        ),
    ] {
        match result {
            Ok(()) => {
                assert_ne!(trustee, "AAP", "undeclared AAP DLL must be denied");
                println!("{trustee}:{mode}:{operation}:allowed");
            }
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                println!("{trustee}:{mode}:{operation}:denied");
            }
        }
    }
    if mode == "ambient-root" {
        println!("approved-root");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows_ambient_read_probe_helper",
                "--nocapture",
            ])
            .env("TAPID_TEST_MARKER", "ambient-child")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "descendant denied-read probe failed: {status}"
        );
    } else {
        println!("approved-child");
    }
}

fn load_searched_authority_probe_library(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetDllDirectoryW(path: *const u16) -> i32;
    }
    let directory: Vec<_> = path
        .parent()
        .unwrap()
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: this helper changes loader state only in the isolated probe process.
    if unsafe { SetDllDirectoryW(directory.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result = load_authority_probe_library(std::path::Path::new(path.file_name().unwrap()));
    // SAFETY: null restores the default search path before returning the observation.
    assert_ne!(unsafe { SetDllDirectoryW(std::ptr::null()) }, 0);
    result
}

fn load_authority_probe_library(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(path: *const u16) -> isize;
        fn GetProcAddress(module: isize, name: *const u8) -> *const std::ffi::c_void;
        fn FreeLibrary(module: isize) -> i32;
    }
    let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: the test owns a NUL-terminated path to a disposable native fixture.
    let module = unsafe { LoadLibraryW(path.as_ptr()) };
    if module == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: module remains loaded through symbol lookup; no N-API code is called.
    let export = unsafe { GetProcAddress(module, c"napi_register_module_v1".as_ptr().cast()) };
    // SAFETY: release exactly the reference returned by LoadLibraryW.
    let released = unsafe { FreeLibrary(module) };
    assert_ne!(released, 0, "release dynamic fixture");
    assert!(!export.is_null(), "loaded the expected native fixture");
    Ok(())
}

#[test]
fn windows_ambient_execution_target_helper() {
    if std::env::var("TAPID_TEST_MARKER").as_deref() == Ok("ambient-target") {
        if let Some(library) = std::env::var_os("TAPID_TEST_LIBRARY") {
            let library = PathBuf::from(library);
            load_authority_probe_library(&library).expect("valid host DLL control");
            load_searched_authority_probe_library(&library)
                .expect("valid host searched DLL control");
            println!("host-dynamic-library-valid");
        }
        println!("undeclared-target-executed");
    }
}

#[test]
fn windows_managed_tree_executes_with_checked_appcontainer_and_job_evidence() {
    let root = temporary_project("managed-tree");
    let baseline_acl = project_dacl(&root);
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let command = system32.join("cmd.exe");
    let request = ExecutionRequest::builder(command.as_os_str())
        .args([
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
            OsString::from("echo stdout-marker & echo stderr-marker 1>&2"),
        ])
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .windows_verbatim_arguments(true)
        .env("TAPID_TEST_MARKER", "allowed-value")
        .build()
        .unwrap();

    let outcome = execute(&request).expect("Windows ManagedTree must execute natively");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains("stdout-marker"));
    assert!(String::from_utf8_lossy(outcome.stderr()).contains("stderr-marker"));
    assert_eq!(
        outcome.enforcement().assurance(),
        AssuranceLevel::ManagedTree
    );
    assert_eq!(
        outcome.enforcement().backend().name(),
        "tapid-runner/windows-appcontainer-job"
    );
    for dimension in [
        tapid_runner::EnforcementDimension::FilesystemRead,
        tapid_runner::EnforcementDimension::Network,
        tapid_runner::EnforcementDimension::EnvironmentSanitization,
        tapid_runner::EnforcementDimension::DescriptorHygiene,
        tapid_runner::EnforcementDimension::DescendantAuthorityPropagation,
        tapid_runner::EnforcementDimension::DescendantLifecycle,
        tapid_runner::EnforcementDimension::ProcessTreeMembership,
        tapid_runner::EnforcementDimension::CompleteCleanup,
        tapid_runner::EnforcementDimension::Timeout,
        tapid_runner::EnforcementDimension::Output,
        tapid_runner::EnforcementDimension::ProcessCount,
        tapid_runner::EnforcementDimension::Memory,
    ] {
        assert!(
            outcome
                .enforcement()
                .established_evidence()
                .iter()
                .any(|evidence| evidence.dimension() == dimension),
            "missing launch evidence for {dimension:?}"
        );
    }
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "normal exit must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_cannot_read_files_outside_the_project_grant() {
    let root = temporary_project("deny-read");
    let outside = root.with_extension("private.txt");
    fs::write(&outside, "TAPID_PRIVATE_OUTSIDE_MARKER").unwrap();
    let command = format!(r#"type "{}""#, outside.display());
    let request = command_request(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("read denial should be an ordinary contained exit");
    assert!(matches!(outcome.termination(), Termination::Exited(code) if *code != 0));
    assert!(!String::from_utf8_lossy(outcome.stdout()).contains("TAPID_PRIVATE_OUTSIDE_MARKER"));
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_cannot_write_outside_the_project_grant() {
    let root = temporary_project("deny-write");
    let outside = root.with_extension("outside write.txt");
    let command = format!(r#"echo TAPID_OUTSIDE_WRITE>"{}""#, outside.display());
    // Positive control: the very same payload must address the exact spaced target.
    let system_root = std::env::var_os("SystemRoot").unwrap();
    use std::os::windows::process::CommandExt;
    let output = Command::new(PathBuf::from(system_root).join("System32").join("cmd.exe"))
        .raw_arg(format!("/D /S /C \"{command}\""))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "host redirect must succeed: {:?}",
        output
    );
    assert_eq!(fs::read(&outside).unwrap(), b"TAPID_OUTSIDE_WRITE\r\n");
    fs::remove_file(&outside).unwrap();
    let baseline_acl = project_dacl(&root);
    let request = command_request(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("write denial should be an ordinary contained exit");
    assert!(matches!(outcome.termination(), Termination::Exited(code) if *code != 0));
    assert!(
        !outside.exists(),
        "AppContainer wrote outside its policy grant"
    );
    assert_eq!(project_dacl(&root), baseline_acl);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_enforces_the_combined_output_limit() {
    let root = temporary_project("output-limit");
    let baseline_acl = project_dacl(&root);
    let request = command_request(
        &root,
        "for /L %i in (1,1,100000) do @echo TAPID_OUTPUT_LINE",
        ExecutionLimits::new(Some(10), Some(128), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("output limit should yield a checked outcome");
    assert_eq!(outcome.termination(), &Termination::OutputLimitExceeded);
    assert!(outcome.stdout().len() + outcome.stderr().len() <= 128);
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "output-limit cleanup must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_timeout_terminates_the_managed_job() {
    let root = temporary_project("timeout");
    let baseline_acl = project_dacl(&root);
    let request = command_request(
        &root,
        "for /L %i in (1,1,2147483647) do @rem",
        ExecutionLimits::new(Some(1), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("timeout should yield a checked outcome");
    assert_eq!(outcome.termination(), &Termination::TimedOut);
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "timeout cleanup must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_ctrl_c_cancels_execution_and_restores_project_dacl() {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    const HELPER_ENV: &str = "TAPID_WINDOWS_CTRL_C_HELPER";
    if std::env::var_os(HELPER_ENV).is_some() {
        for network in [false, true] {
            windows_ctrl_c_helper(network);
        }
        return;
    }

    let started = Instant::now();
    let executable = std::env::current_exe().expect("current test executable path");
    let mut helper = Command::new(&executable)
        .args([
            "--exact",
            "windows_ctrl_c_cancels_execution_and_restores_project_dacl",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "1")
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while the helper runs: diagnostic output must not block its exit.
    let mut stdout_pipe = helper.stdout.take().unwrap();
    let mut stderr_pipe = helper.stderr.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout_pipe.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr_pipe.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let helper_pid = helper.id();
    eprintln!(
        "[windows-ctrl-c parent] spawned helper pid={helper_pid} executable={} flags=CREATE_NEW_CONSOLE elapsed={:?}",
        executable.display(),
        started.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match helper.try_wait() {
            Ok(Some(_)) => {
                let output = std::process::Output {
                    status: helper.wait().unwrap(),
                    stdout: stdout_reader.join().unwrap(),
                    stderr: stderr_reader.join().unwrap(),
                };
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "[windows-ctrl-c parent] helper exited after {:?}: status={}\nhelper stdout:\n{stdout}\nhelper stderr:\n{stderr}",
                    started.elapsed(),
                    output.status
                );
                assert!(
                    output.status.success(),
                    "isolated Ctrl+C helper failed: status={}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                    output.status
                );
                assert_eq!(
                    stdout.matches("CTRL_C_HELPER_SUCCESS").count(),
                    2,
                    "success marker missing; helper stdout:\n{stdout}\nhelper stderr:\n{stderr}"
                );
                break;
            }
            Ok(None) => {}
            Err(error) => {
                let kill = helper.kill();
                let output = std::process::Output {
                    status: helper.wait().unwrap(),
                    stdout: stdout_reader.join().unwrap(),
                    stderr: stderr_reader.join().unwrap(),
                };
                panic!(
                    "cannot poll isolated Ctrl+C helper pid={helper_pid}: {error}; kill={kill:?}; status={}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        if Instant::now() >= deadline {
            let kill = helper.kill();
            let output = std::process::Output {
                status: helper.wait().unwrap(),
                stdout: stdout_reader.join().unwrap(),
                stderr: stderr_reader.join().unwrap(),
            };
            panic!(
                "isolated Ctrl+C helper pid={helper_pid} did not finish before 30s deadline; kill={kill:?}; status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn windows_ctrl_c_helper(network: bool) {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Console::{
        CTRL_C_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList, GetConsoleWindow,
    };

    let started = Instant::now();
    let console_window = unsafe { GetConsoleWindow() } as usize;
    let mut console_processes = [0_u32; 16];
    let console_process_count = unsafe {
        GetConsoleProcessList(
            console_processes.as_mut_ptr(),
            console_processes.len() as u32,
        )
    };
    let copied_process_count = (console_process_count as usize).min(console_processes.len());
    eprintln!(
        "[windows-ctrl-c helper] start pid={} console_window={console_window:#x} console_process_count={console_process_count} console_pids={:?}",
        std::process::id(),
        &console_processes[..copied_process_count]
    );
    let project = tapid_test_support::TempProject::new("ctrl-c").unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    windows_acl::initialize_inheritance(&root);
    let runtime = tapid_test_support::TempProject::new("ctrl-c-runtime").unwrap();
    let reservation = network.then(|| std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap());
    let endpoint = reservation
        .as_ref()
        .map(|listener| listener.local_addr().unwrap());
    let ready = root.join("ready.txt");
    if !network {
        fs::write(&ready, b"").unwrap();
    }
    let baseline_acl = project_dacl(&root);
    eprintln!(
        "[windows-ctrl-c helper] baseline captured after {:?}: root={} dacl={:?}",
        started.elapsed(),
        root.display(),
        String::from_utf8_lossy(&baseline_acl)
    );
    let request = if let Some(endpoint) = endpoint {
        let node = runtime.path().join("node.exe");
        fs::copy(
            std::env::var_os("TAPID_TEST_NODE").expect("native Node is required"),
            &node,
        )
        .unwrap();
        let node = fs::canonicalize(node).unwrap();
        windows_acl::initialize_inheritance(node.parent().unwrap());
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            true,
            vec!["SystemRoot".into()],
            false,
            ExecutionLimits::new(Some(30), Some(4096), Some(8), Some(256 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let script = format!(
            "require('node:net').createServer(s=>s.end('CTRL_C_NETWORK_READY')).listen({},'127.0.0.1',()=>console.log('NETWORK_LISTENING'));",
            endpoint.port()
        );
        ExecutionRequest::builder(node.as_os_str())
            .args(["-e", &script])
            .executable_search_path(node.parent().unwrap())
            .trusted_node_runtime(&node)
            .project_root(&root)
            .policy(policy)
            .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
            .build()
            .unwrap()
    } else {
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec!["ready.txt".into()]).unwrap(),
            false,
            vec![],
            false,
            ExecutionLimits::new(Some(30), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        command_request_with_policy(
            &root,
            "echo CTRL_C_READY>ready.txt& for /L %i in (1,1,2147483647) do @rem",
            policy,
        )
    };
    let runtime_acls = network.then(|| {
        (
            project_dacl(runtime.path()),
            project_dacl(&runtime.path().join("node.exe")),
        )
    });
    drop(reservation);
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        eprintln!("[windows-ctrl-c worker] execute started");
        let result = execute(&request);
        eprintln!(
            "[windows-ctrl-c worker] execute returned after {:?}: {:?}",
            started.elapsed(),
            result.as_ref().map(|outcome| outcome.termination())
        );
        sender.send(result).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    if let Some(endpoint) = endpoint {
        use std::io::Read;
        loop {
            if let Ok(mut stream) =
                std::net::TcpStream::connect_timeout(&endpoint, Duration::from_millis(100))
            {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut payload = String::new();
                stream.read_to_string(&mut payload).unwrap();
                assert_eq!(payload, "CTRL_C_NETWORK_READY");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "network listener did not become ready before Ctrl+C"
            );
            assert!(
                !worker.is_finished(),
                "network execution exited before Ctrl+C"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    if !network {
        loop {
            let marker = fs::read(&ready);
            if marker
                .as_deref()
                .is_ok_and(|bytes| bytes == b"CTRL_C_READY\r\n")
            {
                break;
            }
            if Instant::now() >= deadline || worker.is_finished() {
                panic!(
                    "command did not write its start marker before Ctrl+C; marker={marker:?}; execution={:?}; worker_finished={}",
                    receiver.try_recv(),
                    worker.is_finished()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let active_acl = project_dacl(&root);

    eprintln!(
        "[windows-ctrl-c helper] grant active after {:?}: root={} baseline_dacl={:?} active_dacl={:?}",
        started.elapsed(),
        root.display(),
        String::from_utf8_lossy(&baseline_acl),
        String::from_utf8_lossy(&active_acl)
    );
    println!("DACL_ACTIVE");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    // SAFETY: this helper owns an isolated console. The target has demonstrated startup via
    // its TCP response or file marker, after the runner activated its cancellation scope.
    let event_sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };
    if event_sent == 0 {
        let error = unsafe { GetLastError() };
        panic!(
            "GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) failed with Win32 error {error}; elapsed={:?}; console_window={console_window:#x}; console_pids={:?}",
            started.elapsed(),
            &console_processes[..copied_process_count]
        );
    }
    eprintln!(
        "[windows-ctrl-c helper] control event sent after {:?}; waiting up to 10s; console_pids={:?}",
        started.elapsed(),
        &console_processes[..copied_process_count]
    );
    let outcome = match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result.unwrap_or_else(|error| {
            panic!(
                "execution returned an error after Ctrl+C after {:?}: {error:?}; worker_finished={}",
                started.elapsed(),
                worker.is_finished()
            )
        }),
        Err(error) => {
            let current_acl = project_dacl(&root);
            panic!(
                "execution did not finish after Ctrl+C within 10s: {error:?}; elapsed={:?}; worker_finished={}; dacl_restored={}; root={}; baseline_dacl={:?}; current_dacl={:?}; console_pids={:?}",
                started.elapsed(),
                worker.is_finished(),
                current_acl == baseline_acl,
                root.display(),
                String::from_utf8_lossy(&baseline_acl),
                String::from_utf8_lossy(&current_acl),
                &console_processes[..copied_process_count]
            );
        }
    };
    eprintln!(
        "[windows-ctrl-c helper] outcome after {:?}: termination={:?} cleanup={:?} confirmed={:?} evidence={:?} stdout_bytes={} stderr_bytes={}",
        started.elapsed(),
        outcome.termination(),
        outcome.completion().cleanup_confidence(),
        outcome.completion().confirmed(),
        outcome.completion().evidence(),
        outcome.stdout().len(),
        outcome.stderr().len()
    );
    worker
        .join()
        .unwrap_or_else(|panic| panic!("execution worker panicked: {panic:?}"));
    assert_eq!(outcome.termination(), &Termination::Cancelled);
    if network {
        assert!(String::from_utf8_lossy(outcome.stdout()).contains("NETWORK_LISTENING"));
        assert!(
            outcome
                .enforcement()
                .established_evidence()
                .iter()
                .any(|evidence| evidence.mechanism().contains("socket broker"))
        );
    }
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    let restored_acl = project_dacl(&root);
    assert_eq!(
        restored_acl,
        baseline_acl,
        "cancellation must restore the exact project DACL; baseline={:?}; restored={:?}",
        String::from_utf8_lossy(&baseline_acl),
        String::from_utf8_lossy(&restored_acl)
    );
    if let Some((directory_acl, executable_acl)) = runtime_acls {
        assert_eq!(project_dacl(runtime.path()), directory_acl);
        assert_eq!(
            project_dacl(&runtime.path().join("node.exe")),
            executable_acl
        );
    }
    eprintln!(
        "[windows-ctrl-c helper] cleanup verified after {:?}: DACL restored exactly",
        started.elapsed()
    );
    println!("CTRL_C_HELPER_SUCCESS elapsed={:?}", started.elapsed());
}

#[test]
fn windows_missing_executable_failure_restores_project_dacl() {
    let root = temporary_project("missing-executable");
    let baseline_acl = project_dacl(&root);
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let missing = root.join("missing.exe");
    let request = ExecutionRequest::builder(missing.as_os_str())
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .build()
        .unwrap();

    let error = execute(&request).expect_err("missing executable must fail before spawn");
    assert_eq!(error.category(), ExecutionErrorCategory::Spawn);
    assert_eq!(
        project_dacl(&root),
        baseline_acl,
        "pre-spawn failure must restore the project DACL"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_disabled_subprocess_policy_prevents_child_process_creation() {
    let root = temporary_project("process-limit");
    let command = r#"start "" /B cmd.exe /D /S /C "echo CHILD_PROCESS_MARKER""#;
    let request = command_request(
        &root,
        command,
        ExecutionLimits::new(Some(3), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
    );
    let outcome = execute(&request).expect("subprocess restriction should remain contained");
    assert!(matches!(
        outcome.termination(),
        Termination::Exited(_) | Termination::TimedOut
    ));
    assert!(!String::from_utf8_lossy(outcome.stdout()).contains("CHILD_PROCESS_MARKER"));
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_can_launch_an_executable_subprocess_from_runtime_paths() {
    let root = temporary_project("runtime-child-process");
    let executable =
        fs::canonicalize(std::env::current_exe().expect("containment test executable path"))
            .expect("canonical containment test executable path");
    let executable_directory = executable.parent().expect("test executable directory");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let private_tools = root.join("tools");
    fs::create_dir(&private_tools).unwrap();
    fs::copy(&executable, private_tools.join("probe.exe")).unwrap();
    let private_tools = fs::canonicalize(private_tools).unwrap();
    let request = ExecutionRequest::builder(executable.as_os_str())
        .args([
            OsString::from("--exact"),
            OsString::from("windows_child_process_probe_helper"),
            OsString::from("--nocapture"),
        ])
        .executable_search_path(executable_directory)
        .executable_search_path(&private_tools)
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(10), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .env("TAPID_TEST_MARKER", "runtime-child")
        .build()
        .unwrap();
    let outcome = execute(&request).expect("probe process should remain within the Job Object");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(
        String::from_utf8_lossy(outcome.stdout()).contains("CHILD_PROCESS_MARKER"),
        "child process did not produce its marker"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_child_process_probe_helper() {
    if std::env::var("TAPID_TEST_MARKER").as_deref() != Ok("runtime-child") {
        return;
    }
    let child = std::env::current_dir().unwrap().join("tools/probe.exe");
    let status = Command::new(&child)
        .args([
            "--exact",
            "windows_runtime_child_marker_helper",
            "--nocapture",
        ])
        .env("TAPID_TEST_MARKER", "runtime-marker")
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "CreateProcess for {} failed: kind={:?}, raw_os_error={:?}, error={error}",
                child.display(),
                error.kind(),
                error.raw_os_error()
            )
        });
    assert!(status.success(), "child exited with {status}");
}

#[test]
fn windows_runtime_child_marker_helper() {
    if std::env::var("TAPID_TEST_MARKER").as_deref() == Ok("runtime-marker") {
        println!("CHILD_PROCESS_MARKER");
    }
}

#[test]
fn windows_node_runtime_can_spawn_a_child_inside_the_appcontainer() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let runtime_bin = node.parent().unwrap().to_path_buf();
    let system32 = fs::canonicalize(
        PathBuf::from(std::env::var_os("SystemRoot").expect("Windows SystemRoot is required"))
            .join("System32"),
    )
    .unwrap();
    let root = temporary_project("node-child-process");
    let script = r#"const { spawnSync } = require('node:child_process'); const child = spawnSync(process.execPath, ['--version'], { encoding: 'utf8', stdio: 'inherit' }); if (child.error) { console.error(JSON.stringify({ code: child.error.code, errno: child.error.errno, syscall: child.error.syscall, message: child.error.message })); process.exit(1); } process.stdout.write(child.stdout || ''); process.stderr.write(child.stderr || ''); process.exit(child.status ?? 1);"#;
    let request = ExecutionRequest::builder(node.as_os_str())
        .args([OsString::from("-e"), OsString::from(script)])
        .executable_search_paths([runtime_bin.as_path(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(15), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).expect("Node child-process probe must be contained");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).starts_with('v'));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_node_runtime_can_resolve_a_script_file_inside_the_project_grant() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let runtime_bin = node.parent().unwrap().to_path_buf();
    let system32 = fs::canonicalize(
        PathBuf::from(std::env::var_os("SystemRoot").expect("Windows SystemRoot is required"))
            .join("System32"),
    )
    .unwrap();
    let root = temporary_project("node-script-file");
    let script = root.join("smoke.js");
    fs::write(&script, "process.stdout.write(require('./helper').value);").unwrap();
    fs::write(
        root.join("helper.js"),
        "exports.value = 'PROJECT_SCRIPT_MARKER';",
    )
    .unwrap();
    let request = ExecutionRequest::builder(node.as_os_str())
        .arg(script.as_os_str())
        .executable_search_paths([runtime_bin.as_path(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(15), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).expect("Node script-file probe must be contained");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"PROJECT_SCRIPT_MARKER");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_native_addon_launch_preserves_stock_node_metadata() {
    let project = tapid_test_support::TempProject::new("native-addon-launch").unwrap();
    let runtime = tapid_test_support::TempProject::new("native-addon-runtime").unwrap();
    let ancestor = fs::canonicalize(project.path()).unwrap();
    let root = ancestor.join("project");
    fs::create_dir(&root).unwrap();
    windows_acl::initialize_inheritance(&ancestor);
    let acl = Command::new(
        PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("icacls.exe"),
    )
    .arg(&ancestor)
    .args(["/grant", "*S-1-15-2-1:(RX)"])
    .output()
    .unwrap();
    assert!(acl.status.success(), "{acl:?}");
    let node = runtime.path().join("node.exe");
    fs::copy(std::env::var_os("TAPID_TEST_NODE").unwrap(), &node).unwrap();
    let node = fs::canonicalize(node).unwrap();
    windows_acl::initialize_inheritance(&root);
    windows_acl::initialize_inheritance(node.parent().unwrap());
    windows_node::compile_launcher(&root.join("launcher.node"));
    let script = format!(
        r#"
const assert = require('node:assert/strict');
const addon = require('./launcher.node');
assert.equal(addon.reopen({:?}, ""), 0, "metadata handle must not reopen with listing rights");
const code = "require('node:fs').lstatSync(require('node:path').parse(process.cwd()).root);const child=require('node:child_process').fork('unused', {{execArgv:['-e', 'process.send(7)'],stdio:['ignore','inherit','inherit','ipc']}});child.on('message', value=>{{require('node:assert/strict').equal(value,7);console.log('ADDON_CHILD_METADATA_OK')}});";
assert.equal(addon.launch(process.execPath, code), 0);
"#,
        ancestor.file_name().unwrap().to_str().unwrap()
    );
    let request = ExecutionRequest::builder(node.as_os_str())
        .args(["-e", &script])
        .executable_search_path(node.parent().unwrap())
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(10), Some(4096), Some(4), None).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "{}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"ADDON_CHILD_METADATA_OK\n");
}

#[test]
fn windows_standard_node_ipc_and_ancestor_metadata_preserve_denied_authority() {
    use tapid_runner::{FilesystemAccess, FilesystemGrantKind, FilesystemGrantSource};
    let project = tapid_test_support::TempProject::new("stock-node-compatibility").unwrap();
    let runtime = tapid_test_support::TempProject::new("stock-node-runtime").unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    let node = runtime.path().join("node.exe");
    fs::copy(
        std::env::var_os("TAPID_TEST_NODE").expect("standard Node is required"),
        &node,
    )
    .unwrap();
    let node = fs::canonicalize(node).unwrap();
    windows_acl::initialize_inheritance(&root);
    windows_acl::initialize_inheritance(node.parent().unwrap());
    fs::write(root.join("readable.txt"), b"read control").unwrap();
    let before = windows_acl::read_acl(&root);
    let script = r#"
const fs=require('node:fs'),p=require('node:path'),a=require('node:assert/strict'),{spawn}=require('node:child_process');
const root=p.parse(process.cwd()).root;
a.equal(fs.lstatSync(root).isDirectory(),true);
a.throws(()=>fs.readdirSync(root),e=>['EPERM','EACCES'].includes(e.code));
a.equal(fs.realpathSync.native('readable.txt'),p.join(process.cwd(),'readable.txt'));
a.throws(()=>fs.writeFileSync('readable.txt','overwrite'),e=>['EPERM','EACCES'].includes(e.code));
const leaf="const fs=require('node:fs'),p=require('node:path'),a=require('node:assert/strict');a.equal(fs.lstatSync(p.parse(process.cwd()).root).isDirectory(),true);a.equal(fs.realpathSync.native('readable.txt'),p.join(process.cwd(),'readable.txt'));process.on('message',m=>process.send(m,()=>process.disconnect()));";
let finished=0;
for(let i=0;i<2;i++) {
 const c=spawn(process.execPath,['-e',leaf],{stdio:['ignore','inherit','inherit','ipc']});let replied=false;
 c.on('error',e=>{console.error(e);process.exit(2);});
 c.on('message',m=>{a.equal(m,'roundtrip');replied=true;});
 c.on('exit',n=>{a.equal(n,0);a.equal(replied,true);if(++finished===2)console.log('STOCK_NODE_COMPAT_OK');});
 c.send('roundtrip');
}
"#;
    let request = ExecutionRequest::builder(node.as_os_str())
        .args([OsString::from("-e"), OsString::from(script)])
        .executable_search_path(node.parent().unwrap())
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(5), Some(4096), Some(4), None).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).expect("standard Node must execute with containment");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "{}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"STOCK_NODE_COMPAT_OK\n");
    assert!(
        outcome
            .enforcement()
            .resolved_filesystem()
            .grants()
            .iter()
            .any(|grant| grant.access() == FilesystemAccess::ReadMetadata
                && grant.kind() == FilesystemGrantKind::ExactDirectory
                && grant.source() == FilesystemGrantSource::BackendRuntime
                && grant.path().parent().is_none())
    );
    assert_eq!(
        fs::read(root.join("readable.txt")).unwrap(),
        b"read control"
    );
    assert_eq!(windows_acl::read_acl(&root), before);
    let outside = tapid_test_support::TempProject::new("stock-node-outside").unwrap();
    let sentinel = outside.path().join("private.txt");
    fs::write(&sentinel, b"outside control").unwrap();
    let sentinel_js = sentinel
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "\\'");
    fs::write(root.join("stock.test.mjs"),format!(r#"
import test from 'node:test';import assert from 'node:assert/strict';import fs from 'node:fs';import path from 'node:path';import {{spawn}} from 'node:child_process';
test('default test isolation retains ancestor capabilities and denied access',async()=>{{
 assert.equal(fs.lstatSync(path.parse(process.cwd()).root).isDirectory(),true);
 assert.throws(()=>fs.readFileSync('{sentinel_js}'),e=>['EPERM','EACCES'].includes(e.code));
 const leaf="const fs=require('node:fs'),p=require('node:path'),a=require('node:assert/strict');a.equal(fs.lstatSync(p.parse(process.cwd()).root).isDirectory(),true);a.equal(fs.realpathSync.native('readable.txt'),p.join(process.cwd(),'readable.txt'));process.on('message',m=>process.send(m,()=>process.disconnect()));";
 const middle="const {{spawn}}=require('node:child_process');const c=spawn(process.execPath,['-e',"+JSON.stringify(leaf)+"],{{stdio:['ignore','inherit','inherit','ipc']}});let replied=false;c.on('message',m=>{{if(m!=='grandchild')process.exit(7);replied=true;}});c.on('error',()=>process.exit(8));c.on('exit',n=>process.exit(n||(!replied?9:0)));c.send('grandchild');";
 await new Promise((resolve,reject)=>{{const c=spawn(process.execPath,['-e',middle],{{stdio:'inherit'}});c.on('error',reject);c.on('exit',n=>n?reject(new Error('grandchild exit '+n)):resolve());}});
}});
"#)).unwrap();
    let request = ExecutionRequest::builder(node.as_os_str())
        .args(["--test", "stock.test.mjs"])
        .executable_search_path(node.parent().unwrap())
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(5), Some(4096), Some(4), None).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "{}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(
        String::from_utf8_lossy(outcome.stdout())
            .lines()
            .any(|line| line.ends_with(" pass 1"))
    );
    assert_eq!(fs::read(sentinel).unwrap(), b"outside control");
    assert_eq!(windows_acl::read_acl(&root), before);
}

#[test]
fn windows_node_runtime_can_be_launched_directly_when_test_runtime_is_configured() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let fixture = tapid_test_support::TempProject::new("direct-node-runtime").unwrap();
    let root = fs::canonicalize(fixture.path()).unwrap();
    windows_acl::initialize_inheritance(&root);
    let baseline_acl = project_dacl(&root);
    let runtime_bin = node.parent().unwrap().to_path_buf();
    let system32 = fs::canonicalize(
        PathBuf::from(std::env::var_os("SystemRoot").expect("Windows SystemRoot is required"))
            .join("System32"),
    )
    .unwrap();
    let request = ExecutionRequest::builder(node.as_os_str())
        .arg("--version")
        .executable_search_paths([runtime_bin.as_path(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy())
        .build()
        .unwrap();
    let outcome = execute(&request);
    let outcome = outcome.expect("Node should launch inside its AppContainer");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).starts_with('v'));
    assert_eq!(project_dacl(&root), baseline_acl);
}

#[test]
fn windows_node_timeout_reaps_descendant_before_restoring_project_dacl() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let system32 =
        fs::canonicalize(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"))
            .unwrap();
    let root = temporary_project("node-descendant-timeout");
    let baseline_acl = project_dacl(&root);
    let script = r#"const {spawn} = require('node:child_process'); const child = spawn(process.execPath, ['-e', 'console.log("NODE_DESCENDANT_ACTIVE"); setInterval(() => {}, 1000);'], {stdio: 'inherit'}); child.on('error', error => {console.error(error); process.exit(1);}); child.on('exit', (code, signal) => console.error(JSON.stringify({code, signal})));"#;
    let request = ExecutionRequest::builder(node.as_os_str())
        .args(["-e", script])
        .executable_search_paths([node.parent().unwrap(), system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .policy(managed_policy_with_flags(
            ExecutionLimits::new(Some(3), Some(4096), Some(8), Some(128 * 1024 * 1024)).unwrap(),
            true,
            false,
        ))
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::TimedOut,
        "stderr={}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"NODE_DESCENDANT_ACTIVE\n");
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(project_dacl(&root), baseline_acl);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_node_preserves_argv_environment_exit_and_readonly_boundaries() {
    let Some(node) = std::env::var_os("TAPID_TEST_NODE").map(PathBuf::from) else {
        eprintln!("skipping: TAPID_TEST_NODE is not set");
        return;
    };
    let node = fs::canonicalize(node).unwrap();
    let runtime_bin = node.parent().unwrap();
    let system32 =
        fs::canonicalize(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"))
            .unwrap();
    let root = temporary_project("node-contract");
    let outside = root.with_extension("private.txt");
    fs::write(&outside, "PRIVATE_MARKER").unwrap();
    let forbidden_write = outside.with_extension("write.txt");
    fs::write(root.join("allowed.txt"), "READ_GRANTED").unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(std::net::TcpStream::connect(address).unwrap());
    drop(listener.accept().unwrap());
    let script = root.join("probe.js");
    fs::write(
        &script,
        r#"
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
assert.deepEqual(process.argv.slice(2, 6), ['space value', 'quote"value', 'tail\\', '']);
assert.equal(process.env.TAPID_TEST_MARKER, 'allowed-value');
assert.equal(process.env.TAPID_SECRET_NOT_ALLOWED, undefined);
assert.equal(fs.readFileSync('allowed.txt', 'utf8'), 'READ_GRANTED');
assert.throws(() => fs.readFileSync(process.argv[6]), error => ['EACCES', 'EPERM'].includes(error.code));
assert.throws(() => fs.writeFileSync(process.argv[7], 'escape'), error => ['EACCES', 'EPERM'].includes(error.code));
assert.throws(() => fs.writeFileSync('allowed.txt', 'overwrite'), error => ['EACCES', 'EPERM'].includes(error.code));
assert.throws(() => fs.writeFileSync('new.txt', 'create'), error => ['EACCES', 'EPERM'].includes(error.code));
const socket = net.connect({host: '127.0.0.1', port: Number(process.argv[8])});
socket.on('connect', () => { console.error('NETWORK_ESCAPE'); process.exit(1); });
socket.on('error', () => finish());
socket.setTimeout(1000, () => finish());
let finished = false;
function finish() {
  if (finished) return;
  finished = true;
  socket.destroy();
  process.stdout.write('NODE_CONTRACT_OK');
  process.stderr.write('NODE_STDERR_OK');
  process.exitCode = 23;
}
"#,
    )
    .unwrap();
    let baseline_acl = project_dacl(&root);
    let request = ExecutionRequest::builder(node.as_os_str())
        .args([
            script.as_os_str().to_owned(),
            "space value".into(),
            "quote\"value".into(),
            "tail\\".into(),
            "".into(),
            outside.as_os_str().to_owned(),
            forbidden_write.as_os_str().to_owned(),
            address.port().to_string().into(),
        ])
        .executable_search_paths([runtime_bin, system32.as_path()])
        .trusted_node_runtime(&node)
        .project_root(&root)
        .env("TAPID_TEST_MARKER", "allowed-value")
        .policy(managed_policy())
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(23),
        "stdout={} stderr={}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(outcome.stdout(), b"NODE_CONTRACT_OK");
    assert_eq!(outcome.stderr(), b"NODE_STDERR_OK");
    assert_eq!(
        fs::read_to_string(root.join("allowed.txt")).unwrap(),
        "READ_GRANTED"
    );
    assert!(!forbidden_write.exists());
    assert!(!root.join("new.txt").exists());
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(project_dacl(&root), baseline_acl);
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_runtime_directory_ace_allows_launching_read_execute_binaries() {
    use tapid_test_support::TempProject;

    // Never write probes or apply test ACLs to a shared Node installation.
    let runtime = TempProject::new("runtime-executable-acl").unwrap();
    let project = TempProject::new("runtime-executable-project").unwrap();
    let runtime_bin = fs::canonicalize(runtime.path()).unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    windows_acl::initialize_inheritance(&runtime_bin);
    windows_acl::initialize_inheritance(&root);
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let probe = runtime_bin.join("cmd.exe");
    fs::copy(PathBuf::from(system_root).join("System32/cmd.exe"), &probe).unwrap();
    let baseline_runtime_acl = project_dacl(&runtime_bin);
    let baseline_probe_acl = project_dacl(&probe);
    let baseline_project_acl = project_dacl(&root);
    let request = ExecutionRequest::builder(probe.as_os_str())
        .args(["/D", "/S", "/C", "echo RUNTIME_EXECUTABLE_MARKER"])
        .executable_search_path(&runtime_bin)
        .project_root(&root)
        .policy(managed_policy())
        .windows_verbatim_arguments(true)
        .build()
        .unwrap();
    let outcome = execute(&request).expect("runtime executable should start under AppContainer");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains("RUNTIME_EXECUTABLE_MARKER"));
    assert_eq!(project_dacl(&runtime_bin), baseline_runtime_acl);
    assert_eq!(project_dacl(&probe), baseline_probe_acl);
    assert_eq!(project_dacl(&root), baseline_project_acl);
    // TempProject removes both fixtures even if any assertion above unwinds.
}

#[test]
fn windows_missing_write_target_fails_closed_without_materialization() {
    let root = temporary_project("write-target-missing");
    let writable = root.join("writable");
    let marker = writable.join("must-not-spawn.txt");
    let outside = root.join("outside sentinel.txt");
    fs::write(&outside, b"host-positive-control").unwrap();
    let paths = [&root, &outside];
    let baselines: Vec<_> = paths.iter().map(|path| project_dacl(path)).collect();
    let command = format!("echo should-not-run>\"{}\"", marker.display());
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec!["writable".into()]).unwrap(),
        false,
        vec!["TAPID_TEST_MARKER".into()],
        false,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let request = command_request_with_policy(&root, &command, policy);

    let error = execute(&request).expect_err("missing Windows write target must fail closed");
    assert!(!writable.exists(), "missing target was materialized");
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(!marker.exists(), "unsupported write policy started a child");
    assert_eq!(fs::read(&outside).unwrap(), b"host-positive-control");
    for (path, before) in paths.iter().zip(baselines) {
        assert_eq!(
            project_dacl(path),
            before,
            "prelaunch rejection changed ACLs"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_appcontainer_can_modify_existing_file_in_declared_subtree() {
    let fixture = tapid_test_support::TempProject::new("write-existing-file").unwrap();
    let root = fixture.path().to_path_buf();
    windows_acl::initialize_inheritance(&root);
    let writable = root.join("writable");
    fs::create_dir(&writable).unwrap();
    let authorized = writable.join("authorized.txt");
    fs::write(&authorized, "preexisting content").unwrap();
    let baseline_root = project_dacl(&root);
    let baseline_write = project_dacl(&writable);
    let baseline_file = project_dacl(&authorized);
    let command = format!("echo TAPID_WRITE_GRANTED>\"{}\"", authorized.display());
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec!["writable".into()]).unwrap(),
        false,
        vec!["TAPID_TEST_MARKER".into()],
        false,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let request = command_request_with_policy(&root, &command, policy);

    let outcome = execute(&request).expect("a declared write grant should execute");
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert_eq!(
        fs::read_to_string(&authorized).unwrap().trim(),
        "TAPID_WRITE_GRANTED"
    );
    assert_eq!(project_dacl(&root), baseline_root);
    assert_eq!(project_dacl(&writable), baseline_write);
    assert_eq!(project_dacl(&authorized), baseline_file);
    let command = format!("echo REOPENED>>\"{}\"", authorized.display());
    let request = command_request_with_policy(&root, &command, request.policy().clone());
    let outcome = execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert!(
        fs::read_to_string(&authorized)
            .unwrap()
            .contains("REOPENED")
    );
    assert_eq!(project_dacl(&root), baseline_root);
    assert_eq!(project_dacl(&writable), baseline_write);
    assert_eq!(project_dacl(&authorized), baseline_file);
}

#[test]
fn windows_private_write_tree_creates_and_reopens_files_without_outside_authority() {
    let fixture = tapid_test_support::TempProject::new("private-write-boundary").unwrap();
    let root = fs::canonicalize(fixture.path()).unwrap();
    windows_acl::initialize_inheritance(&root);
    let writable = root.join("package");
    fs::create_dir(&writable).unwrap();
    fixture.write("readonly.txt", b"readonly data").unwrap();
    let outside = tapid_test_support::TempProject::new("private-write-outside").unwrap();
    outside.write("secret.txt", b"outside data").unwrap();
    tapid_runner::prepare_private_dependency_write_tree(&writable).unwrap();
    let root_acl = project_dacl(&root);
    let outside_acl = project_dacl(outside.path());
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec!["package".into()]).unwrap(),
        false,
        vec![],
        false,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let shell_request = command_request_with_policy(&root, "", policy.clone());
    let command = format!(
        "echo denied>..\\readonly.txt & type \"{}\">stolen.txt & echo denied>\"{}\" & echo CREATED>generated.txt&echo REOPENED>>generated.txt",
        outside.path().join("secret.txt").display(),
        outside.path().join("escaped.txt").display(),
    );
    let request = ExecutionRequest::builder(shell_request.program())
        .executable_search_path(
            std::path::Path::new(shell_request.program())
                .parent()
                .unwrap(),
        )
        .args([
            OsString::from("/D"),
            OsString::from("/S"),
            OsString::from("/C"),
            command.into(),
        ])
        .project_root(&root)
        .working_directory(&writable)
        .policy(policy)
        .windows_verbatim_arguments(true)
        .build()
        .unwrap();
    let outcome = execute(&request).unwrap();
    assert_eq!(outcome.termination(), &Termination::Exited(0));
    assert_eq!(
        fs::read_to_string(writable.join("generated.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "CREATED\nREOPENED\n"
    );
    assert_eq!(
        fs::read(root.join("readonly.txt")).unwrap(),
        b"readonly data"
    );
    assert!(!outside.path().join("escaped.txt").exists());
    assert!(
        !fs::read(writable.join("stolen.txt"))
            .unwrap_or_default()
            .windows(b"outside data".len())
            .any(|bytes| bytes == b"outside data")
    );
    assert_eq!(project_dacl(&root), root_acl);
    assert_eq!(project_dacl(outside.path()), outside_acl);
}

#[test]
fn windows_private_write_preparation_rejects_junctions_without_lowering_host_integrity() {
    use std::os::windows::process::CommandExt;
    let project = tapid_test_support::TempProject::new("private-write-junction").unwrap();
    let root = project.path().to_path_buf();
    windows_acl::initialize_inheritance(&root);
    project.write("outside/host.txt", b"host data").unwrap();
    let outside = root.join("outside");
    let private = root.join("private");
    fs::create_dir(&private).unwrap();
    let shell = command_request_with_policy(&root, "", managed_policy());
    let junction = private.join("junction");
    let output = Command::new(shell.program())
        .args(["/D", "/S", "/C"])
        .raw_arg(format!(
            "mklink /J \"{}\" \"{}\"",
            junction.display(),
            outside.display()
        ))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = project_dacl(&outside);
    assert!(tapid_runner::prepare_private_dependency_write_tree(&private).is_err());
    assert_eq!(project_dacl(&outside), before);
    assert_eq!(fs::read(outside.join("host.txt")).unwrap(), b"host data");
    let policy = SandboxPolicy::new_with_assurance(
        SandboxMode::Required,
        AssuranceLevel::ManagedTree,
        FilesystemPolicy::new(vec![".".into()], vec!["private".into()]).unwrap(),
        false,
        vec![],
        false,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
    )
    .unwrap();
    let error = execute(&command_request_with_policy(
        &root,
        "echo unexpected",
        policy,
    ))
    .expect_err("junction write target must fail before spawn");
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert_eq!(project_dacl(&outside), before);
    assert_eq!(fs::read(outside.join("host.txt")).unwrap(), b"host data");
    fs::remove_dir(junction).unwrap();
}

#[test]
fn windows_private_write_preparation_rejects_hardlinks_without_labelling_the_external_file() {
    let project = tapid_test_support::TempProject::new("private-write-hardlink").unwrap();
    project.write("outside.txt", b"host data").unwrap();
    let outside = project.path().join("outside.txt");
    let private = project.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::hard_link(&outside, private.join("linked.txt")).unwrap();
    let before = project_dacl(&outside);
    assert!(tapid_runner::prepare_private_dependency_write_tree(&private).is_err());
    assert_eq!(project_dacl(&outside), before);
    assert_eq!(fs::read(&outside).unwrap(), b"host data");
}

#[test]
fn windows_network_policy_without_selected_node_component_fails_closed_before_spawn() {
    let root = temporary_project("network-allowed-unavailable");
    let marker = root.join("must-not-spawn.txt");
    let command = format!("echo should-not-run>\"{}\"", marker.display());
    let request = command_request_with_network(
        &root,
        &command,
        ExecutionLimits::new(Some(10), Some(4096), Some(4), Some(128 * 1024 * 1024)).unwrap(),
        true,
    );
    let error = execute(&request).expect_err("network policy requires the selected Node component");
    assert_eq!(
        error.category(),
        ExecutionErrorCategory::UnsupportedContainment
    );
    assert!(error.to_string().contains(
        "Windows networking requires the selected standard-Node compatibility component"
    ));
    assert!(
        !marker.exists(),
        "untrusted code must not start for unsupported network policy"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_network_probe_is_denied_with_a_reachable_host_positive_control() {
    let root = temporary_project("network-denial-control");
    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let system32 = fs::canonicalize(PathBuf::from(system_root).join("System32")).unwrap();
    let curl = system32.join("curl.exe");
    let version = std::process::Command::new(&curl)
        .arg("--version")
        .output()
        .expect("Windows curl must run outside the AppContainer");
    assert!(
        version.status.success(),
        "curl --version failed: {version:?}"
    );

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("host positive-control listener must bind");
    let address = listener.local_addr().unwrap();
    let positive_control = std::net::TcpStream::connect(address)
        .expect("ordinary Windows process must reach the listener");
    drop(positive_control);
    drop(
        listener
            .accept()
            .expect("host positive-control connection must arrive"),
    );

    let request = ExecutionRequest::builder(curl.into_os_string())
        .args([
            OsString::from("--connect-timeout"),
            OsString::from("2"),
            OsString::from("--max-time"),
            OsString::from("2"),
            OsString::from("--show-error"),
            OsString::from(format!("http://127.0.0.1:{}/", address.port())),
        ])
        .executable_search_path(&system32)
        .project_root(&root)
        .policy(managed_policy())
        .build()
        .unwrap();
    let outcome = execute(&request).expect("network probe should be contained");
    let stdout = String::from_utf8_lossy(outcome.stdout());
    let stderr = String::from_utf8_lossy(outcome.stderr());

    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ),
        "the AppContainer established a TCP connection to the reachable host listener; stdout={stdout:?}, stderr={stderr:?}"
    );
    // AppContainer isolation can time out the connection; the WFP guard can
    // reject socket assignment first, which curl reports as a connection failure.
    let diagnostic = match outcome.termination() {
        Termination::Exited(7) => "curl: (7)",
        Termination::Exited(28) => "curl: (28)",
        other => panic!(
            "curl did not report connection failure or timeout: {other:?}; stdout={stdout:?}, stderr={stderr:?}"
        ),
    };
    assert!(
        stderr.contains(diagnostic),
        "expected curl's connection-failure or timeout diagnostic; stdout={stdout:?}, stderr={stderr:?}"
    );
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    fs::remove_dir_all(root).unwrap();
}
