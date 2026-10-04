#![cfg(all(windows, test))]
use super::*;
use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

#[test]
fn assigning_an_invalid_process_handle_fails_closed() {
    let job = WindowsJob::new(
        &ExecutionLimits::new(None, None, None, None).unwrap(),
        false,
    )
    .unwrap();
    assert!(job.assign_suspended_process(0).is_err());
}

#[test]
fn appcontainer_child_is_verified_and_assigned_before_resume() {
    let (mut container, job, mut child) = create_appcontainer_child("exit 0");
    assert_eq!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn appcontainer_child_captures_stdout_and_stderr_separately() {
    use std::io::Read;

    let (mut container, job, mut child, pipes) =
        create_appcontainer_child_with_stdio("echo TAPID_OUT & echo TAPID_ERR 1>&2");
    let (mut stdout_pipe, mut stderr_pipe) = pipes.into_parent_readers().unwrap();
    assert_eq!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_pipe.read_to_end(&mut stdout).unwrap();
    stderr_pipe.read_to_end(&mut stderr).unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    assert_eq!(String::from_utf8_lossy(&stdout).trim(), "TAPID_OUT");
    assert_eq!(String::from_utf8_lossy(&stderr).trim(), "TAPID_ERR");
}

#[test]
fn appcontainer_output_limit_terminates_and_bounds_combined_capture() {
    let (mut container, job, mut child, pipes) = create_appcontainer_child_with_stdio(
        "for /L %i in (1,1,1000000) do @echo TAPID_OUTPUT_CHUNK",
    );
    let capture = WindowsOutputCapture::start(pipes.into_parent_readers().unwrap(), Some(128));
    let termination = child
        .resume_and_wait_for_status(&job, 5_000, capture.output_limit_exceeded())
        .unwrap();
    let (stdout, stderr) = capture.finish().unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    assert_eq!(termination, WindowsChildTermination::OutputLimitExceeded);
    assert!(stdout.len() + stderr.len() <= 128);
}

#[test]
fn appcontainer_timeout_terminates_and_reaps_the_job() {
    let (mut container, job, mut child) =
        create_appcontainer_child("for /L %i in (1,1,10000000) do @set /a x=%i >nul");
    let error = child.resume_and_wait_for_exit(&job, 100).unwrap_err();
    assert_eq!(error.category(), ExecutionErrorCategory::Timeout);
    assert!(child.wait_for_signal(5_000).unwrap());
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn explicit_job_termination_signals_a_suspended_assigned_child() {
    let (mut container, job, child) = create_appcontainer_child("exit 0");
    job.terminate_all().unwrap();
    assert!(child.wait_for_signal(5_000).unwrap());
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn job_completion_port_reports_when_the_last_member_exits() {
    let (mut container, job, child) = create_appcontainer_child("exit 0");
    job.terminate_all().unwrap();
    assert!(child.wait_for_signal(5_000).unwrap());
    job.wait_for_active_process_zero_notification(5_000)
        .unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
}

#[test]
fn restoring_appcontainer_grant_preserves_concurrent_dacl_changes() {
    let root = std::env::temp_dir().join(format!(
        "tapid-appcontainer-concurrent-dacl-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut container = WindowsAppContainer::create().unwrap();
    let mut grant = WindowsPathAcl::grant(
        &root,
        container.sid(),
        FilesystemAccess::Write,
        FilesystemGrantKind::DirectorySubtree,
    )
    .unwrap();

    let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
    let icacls =
        std::fs::canonicalize(std::path::PathBuf::from(system_root).join("System32/icacls.exe"))
            .unwrap();
    // BUILTIN\\Remote Desktop Users is a valid, distinct trustee that is not
    // inherited by this temporary directory. The numeric form avoids localization.
    let concurrent_sid = "*S-1-5-32-555";
    let concurrent_ace = format!("{concurrent_sid}:(OI)(CI)(R)");
    let change = std::process::Command::new(&icacls)
        .arg(&root)
        .arg("/grant")
        .arg(&concurrent_ace)
        .output()
        .unwrap();
    assert!(
        change.status.success(),
        "icacls concurrent DACL update failed: {}{}",
        String::from_utf8_lossy(&change.stdout),
        String::from_utf8_lossy(&change.stderr)
    );

    grant.restore().unwrap();
    let saved_acl = std::env::temp_dir().join(format!(
        "tapid-appcontainer-concurrent-dacl-{}.acl",
        std::process::id()
    ));
    let saved = std::process::Command::new(&icacls)
        .arg(&root)
        .arg("/save")
        .arg(&saved_acl)
        .arg("/c")
        .output()
        .unwrap();
    assert!(saved.status.success());
    let bytes = std::fs::read(&saved_acl).unwrap();
    let utf16 = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let acl = String::from_utf16_lossy(&utf16);
    assert!(
        acl.contains(";;;RD)"),
        "restoring Tapid's ACE discarded the concurrent ACE: {acl}"
    );
    std::fs::remove_file(saved_acl).unwrap();
    container.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn appcontainer_child_can_write_a_granted_tree_and_loses_access_on_restore() {
    let root = std::env::temp_dir().join(format!(
        "tapid-appcontainer-grant-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let marker = root.join("allowed.txt");
    let mut container = WindowsAppContainer::create().unwrap();
    let mut grant = WindowsPathAcl::grant(
        &root,
        container.sid(),
        FilesystemAccess::Write,
        FilesystemGrantKind::DirectorySubtree,
    )
    .unwrap();

    let payload = format!("echo authorized>\"{}\"", marker.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    assert_eq!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    drop(child);
    drop(job);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "authorized"
    );

    let payload = format!("type \"{}\" >nul", marker.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    assert_ne!(
        child.resume_and_wait_for_exit(&job, 5_000).unwrap(),
        0,
        "write-only grants must not permit reading the granted subtree"
    );
    drop(child);
    drop(job);

    grant.restore().unwrap();
    let payload = format!("echo denied>\"{}\"", marker.display());
    let (job, mut child) = create_appcontainer_child_in(&container, &payload);
    assert_ne!(child.resume_and_wait_for_exit(&job, 5_000).unwrap(), 0);
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "authorized"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn appcontainer_child_cannot_write_an_ungranted_temp_file() {
    let marker =
        std::env::temp_dir().join(format!("tapid-appcontainer-{}.txt", std::process::id()));
    std::fs::write(&marker, b"parent-owned").unwrap();
    let payload = format!("echo child>\"{}\" & exit 42", marker.display());
    let (mut container, job, mut child) = create_appcontainer_child(&payload);
    let exit_code = child.resume_and_wait_for_exit(&job, 5_000).unwrap();
    let contents = std::fs::read(&marker).unwrap();
    drop(child);
    drop(job);
    container.cleanup().unwrap();
    std::fs::remove_file(marker).unwrap();
    assert_ne!(
        exit_code, 0,
        "redirection to the parent-owned marker must be denied"
    );
    assert_eq!(contents, b"parent-owned");
}

fn create_appcontainer_child(
    payload: &str,
) -> (WindowsAppContainer, WindowsJob, WindowsSuspendedChild) {
    let container = WindowsAppContainer::create().unwrap();
    let (job, child) = create_appcontainer_child_in(&container, payload);
    (container, job, child)
}

fn create_appcontainer_child_with_stdio(
    payload: &str,
) -> (
    WindowsAppContainer,
    WindowsJob,
    WindowsSuspendedChild,
    WindowsStdioPipes,
) {
    let container = WindowsAppContainer::create().unwrap();
    let mut pipes = WindowsStdioPipes::new().unwrap();
    let (job, child) = create_appcontainer_child_inner(&container, payload, Some(&mut pipes));
    (container, job, child, pipes)
}

fn create_appcontainer_child_in(
    container: &WindowsAppContainer,
    payload: &str,
) -> (WindowsJob, WindowsSuspendedChild) {
    create_appcontainer_child_inner(container, payload, None)
}

fn create_appcontainer_child_inner(
    container: &WindowsAppContainer,
    payload: &str,
    stdio: Option<&mut WindowsStdioPipes>,
) -> (WindowsJob, WindowsSuspendedChild) {
    use std::os::windows::ffi::OsStrExt;

    let system_root = std::env::var_os("SystemRoot").unwrap();
    let current_directory = std::path::PathBuf::from(&system_root);
    let program_path = current_directory.join("System32").join("cmd.exe");
    let program = program_path.as_os_str().encode_wide().collect::<Vec<_>>();
    let arguments = ["/D", "/S", "/C", payload]
        .into_iter()
        .map(|argument| argument.encode_utf16().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let command_line =
        super::super::serialize_windows_command_line_units(&program, &arguments, true).unwrap();
    let environment =
        super::super::windows_environment_block_units(&std::collections::BTreeMap::from([(
            std::ffi::OsString::from("SystemRoot"),
            system_root.clone(),
        )]))
        .unwrap();
    let mut application = program;
    application.push(0);
    let mut working_directory = current_directory
        .as_os_str()
        .encode_wide()
        .collect::<Vec<_>>();
    working_directory.push(0);

    let job = WindowsJob::new(
        &ExecutionLimits::new(None, None, None, None).unwrap(),
        false,
    )
    .unwrap();
    let child = match stdio {
        Some(pipes) => WindowsSuspendedChild::create_with_stdio(
            container,
            &application,
            &command_line,
            &environment,
            &working_directory,
            pipes,
        ),
        None => WindowsSuspendedChild::create(
            container,
            &application,
            &command_line,
            &environment,
            &working_directory,
        ),
    }
    .unwrap();
    job.assign_suspended_process(child.process_handle())
        .unwrap();
    (job, child)
}

#[test]
fn disabled_subprocess_policy_caps_the_job_to_its_root_process() {
    let job = WindowsJob::new(&ExecutionLimits::default(), true).unwrap();
    let information = job.query_extended_limits().unwrap();
    assert_eq!(information.BasicLimitInformation.ActiveProcessLimit, 1);
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    );
}

#[test]
fn job_limits_are_job_wide_and_kill_members_when_owner_closes() {
    const PROCESS_LIMIT: u32 = 4;
    const MEMORY_LIMIT: u64 = 64 * 1024 * 1024;
    let limits = ExecutionLimits::new(None, None, Some(PROCESS_LIMIT), Some(MEMORY_LIMIT)).unwrap();
    let job = WindowsJob::new(&limits, false).unwrap();
    let information = job.query_extended_limits().unwrap();

    assert_eq!(
        information.BasicLimitInformation.ActiveProcessLimit,
        PROCESS_LIMIT
    );
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    );
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOB_OBJECT_LIMIT_JOB_MEMORY
    );
    assert_eq!(
        information.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    );
    assert_eq!(information.JobMemoryLimit, MEMORY_LIMIT as usize);
    assert_eq!(information.ProcessMemoryLimit, 0);
}
