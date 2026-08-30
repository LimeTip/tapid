use std::process::ExitCode;

/// Convert a child status to the process status used by the CLI.
///
/// Windows needs to preserve the complete status. Unix exposes the low byte
/// through its normal process-status interface.
#[cfg(windows)]
pub(crate) fn child_exit_code(code: i32) -> ExitCode {
    std::process::exit(code);
}

#[cfg(not(windows))]
pub(crate) fn child_exit_code(code: i32) -> ExitCode {
    ExitCode::from(code as u8)
}
