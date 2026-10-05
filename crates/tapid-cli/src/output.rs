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

pub(crate) fn report_warnings(warnings: &[crate::application::outcome::Warning]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

pub(crate) fn report_failure(failure: &crate::application::outcome::OperationFailure) {
    use crate::application::outcome::{ChangeState, RetryAdvice};
    report_warnings(&failure.outcome.warnings);
    eprintln!("error: {failure}");
    eprintln!("diagnostic: {}", failure.error.kind.code());
    match failure.outcome.state {
        ChangeState::Committed => {
            eprintln!(
                "dependency changes committed in {}; do not repeat the operation",
                failure.outcome.project_dir.display()
            );
        }
        ChangeState::CommittedCleanupPending => {
            eprintln!(
                "dependency changes committed in {}; cleanup remains pending; do not repeat the operation",
                failure.outcome.project_dir.display()
            );
        }
        ChangeState::RecoveryRequired => {
            eprintln!(
                "recovery required in {}; inspect {} affected project output(s) before retrying",
                failure.outcome.project_dir.display(),
                failure.outcome.changed_files.len()
            );
        }
        ChangeState::Unchanged | ChangeState::RolledBack => {}
    }
    if failure.retry == RetryAdvice::AfterContention {
        eprintln!("retry after the competing operation finishes");
    }
}
