//! Explicitly uncontained execution. This path produces no enforcement receipt.
use super::{ExecutionError, ExecutionErrorCategory, ExecutionRequest, Termination};
use std::{
    fs::OpenOptions,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Runs with a controlled environment, null stdin and file-backed output.
///
/// Callers must obtain explicit authorization for uncontained execution. This
/// function does not enforce filesystem, network, process or memory authority,
/// close ambient descriptors, or own descendants. Timeout and output checks
/// supervise only the root; surviving descendants can continue writing capture
/// files. No outcome from this path is evidence for a contained build cache.
pub fn execute_uncontained(
    request: &ExecutionRequest,
    capture: &Path,
) -> Result<Termination, ExecutionError> {
    request.validate()?;
    let failure = |e| {
        ExecutionError::new(
            ExecutionErrorCategory::Spawn,
            format!("uncontained execution failed: {e}"),
        )
    };
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(capture.join("stdout"))
        .map_err(failure)?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(capture.join("stderr"))
        .map_err(failure)?;
    let mut command = Command::new(request.program());
    command
        .current_dir(request.working_directory())
        .env_clear()
        .envs(request.child_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone().map_err(failure)?))
        .stderr(Stdio::from(stderr.try_clone().map_err(failure)?));
    #[cfg(windows)]
    if request.uses_windows_verbatim_arguments() {
        use std::os::windows::process::CommandExt;
        for argument in &request.arguments()[..3] {
            command.arg(argument);
        }
        command.raw_arg(&request.arguments()[3]);
    } else {
        command.args(request.arguments());
    }
    #[cfg(not(windows))]
    command.args(request.arguments());
    let mut child = command.spawn().map_err(failure)?;
    let started = Instant::now();
    let ceiling = request
        .policy()
        .limits()
        .max_output_bytes()
        .unwrap_or(16 * 1024 * 1024)
        .min(16 * 1024 * 1024);
    loop {
        let result = (|| {
            let size = stdout
                .metadata()
                .map_err(failure)?
                .len()
                .checked_add(stderr.metadata().map_err(failure)?.len());
            if size.is_none_or(|size| size > ceiling) {
                return Ok(Some(Termination::OutputLimitExceeded));
            }
            if request
                .policy()
                .limits()
                .timeout_seconds()
                .is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds))
            {
                return Ok(Some(Termination::TimedOut));
            }
            Ok(None)
        })();
        match result {
            Ok(Some(termination)) => {
                child.kill().map_err(failure)?;
                child.wait().map_err(failure)?;
                return Ok(termination);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(None) => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Termination::Exited(status.code().unwrap_or(1))),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failure(error));
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy};
    use tapid_test_support::TempProject;

    fn request(root: &Path, script: &str, limits: ExecutionLimits) -> ExecutionRequest {
        ExecutionRequest::builder("/bin/sh")
            .args(["-c", script])
            .project_root(root)
            .policy(
                SandboxPolicy::new(
                    SandboxMode::Required,
                    FilesystemPolicy::new(vec![], vec![]).unwrap(),
                    false,
                    vec![],
                    true,
                    limits,
                )
                .unwrap(),
            )
            .executable_search_path("/usr/bin")
            .build()
            .unwrap()
    }

    #[test]
    fn uncontained_execution_clears_ambient_environment_and_uses_null_stdin() {
        let project = TempProject::new("uncontained-environment").unwrap();
        let request = request(
            project.path(),
            "printf '%s\\n' \"${HOME-unset}\" \"$PATH\"; if read value; then exit 42; fi",
            ExecutionLimits::default(),
        );
        assert_eq!(
            execute_uncontained(&request, project.path()).unwrap(),
            Termination::Exited(0)
        );
        assert_eq!(
            std::fs::read(project.path().join("stdout")).unwrap(),
            b"unset\n/usr/bin\n"
        );
    }

    #[test]
    fn uncontained_root_timeout_and_output_checks_produce_no_receipt() {
        for (script, limits, expected) in [
            (
                "while :; do :; done",
                ExecutionLimits::new(Some(1), None, None, None).unwrap(),
                Termination::TimedOut,
            ),
            (
                "while :; do printf 'output\\n'; done",
                ExecutionLimits::new(Some(5), Some(64), None, None).unwrap(),
                Termination::OutputLimitExceeded,
            ),
        ] {
            let project = TempProject::new("uncontained-root-limits").unwrap();
            assert_eq!(
                execute_uncontained(&request(project.path(), script, limits), project.path())
                    .unwrap(),
                expected
            );
        }
    }
}
