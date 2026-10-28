use super::*;
use std::fs;

#[test]
fn child_output_is_forwarded_to_its_matching_stream_without_text_conversion() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    forward_child_output(
        &[0x66, 0x00, 0x6f],
        &[0x65, 0x72, 0x72],
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    assert_eq!(stdout, [0x66, 0x00, 0x6f]);
    assert_eq!(stderr, b"err");
}

#[test]
fn cli_does_not_replay_output_already_streamed_by_native_backend() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    forward_child_output_for_cli(b"child-out", b"child-err", &mut stdout, &mut stderr).unwrap();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        assert!(
            stdout.is_empty(),
            "native runner already streams child stdout"
        );
        assert!(
            stderr.is_empty(),
            "native runner already streams child stderr"
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        assert_eq!(stdout, b"child-out");
        assert_eq!(stderr, b"child-err");
    }
}

#[test]
fn nonzero_and_limit_terminations_map_to_stable_cli_exits() {
    #[cfg(not(windows))]
    assert_eq!(
        termination_exit_code(&tapid_runner::Termination::Exited(37)),
        ExitCode::from(37)
    );
    #[cfg(windows)]
    {
        // Production preserves the full Windows status by exiting. Exercise
        // that boundary in a child, never in the parent test executable.
        const CHILD_STATUS: &str = "TAPID_TEST_TERMINATION_STATUS";
        if let Ok(value) = std::env::var(CHILD_STATUS) {
            let code = value.parse::<i32>().unwrap();
            let _ = termination_exit_code(&tapid_runner::Termination::Exited(code));
            panic!("Windows child status conversion unexpectedly returned");
        }
        for code in [0_i32, 37, 256, 65537, -1] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "commands::run::tests::nonzero_and_limit_terminations_map_to_stable_cli_exits",
                    "--nocapture",
                ])
                .env(CHILD_STATUS, code.to_string())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(code));
        }
    }
    for termination in [
        tapid_runner::Termination::TimedOut,
        tapid_runner::Termination::OutputLimitExceeded,
        tapid_runner::Termination::ProcessLimitExceeded,
        tapid_runner::Termination::MemoryLimitExceeded,
    ] {
        assert_eq!(termination_exit_code(&termination), ExitCode::from(1));
    }
}

#[test]
fn cancelled_termination_maps_to_sigint_exit_code() {
    assert_eq!(
        termination_exit_code(&tapid_runner::Termination::Cancelled),
        ExitCode::from(130)
    );
}

#[cfg(unix)]
#[test]
#[allow(
    unsafe_code,
    reason = "this native control inspects descriptor inheritance flags"
)]
fn result_file_is_empty_and_close_on_exec_before_execution() {
    use std::os::fd::AsRawFd;
    let project = tapid_test_support::TempProject::new("run-result-project").unwrap();
    let results = tapid_test_support::TempProject::new("run-result-owner").unwrap();
    let path = results.path().join("result.json");
    let file = result::create_file(&path, project.path()).unwrap();
    assert!(fs::read(path).unwrap().is_empty());
    // SAFETY: F_GETFD only inspects the live descriptor owned by `file`.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}

#[test]
fn result_file_delivery_errors_are_reported_without_a_complete_record() {
    let project = tapid_test_support::TempProject::new("run-result-project").unwrap();
    let results = tapid_test_support::TempProject::new("run-result-owner").unwrap();
    let path = results.path().join("result.json");
    drop(result::create_file(&path, project.path()).unwrap());
    let mut read_only = fs::File::open(&path).unwrap();
    assert!(result::write(&mut read_only, &Err(RunFailure::ScriptMissing), None).is_err());
    assert!(fs::read(path).unwrap().is_empty());
}
