use super::*;

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
