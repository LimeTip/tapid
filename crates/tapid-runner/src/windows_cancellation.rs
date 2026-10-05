use super::{ExecutionError, ExecutionErrorCategory};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{FALSE, GetLastError, TRUE};
use windows_sys::Win32::System::Console::{CTRL_C_EVENT, SetConsoleCtrlHandler};

static ACTIVE_EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
static CTRL_C_GENERATION: AtomicU64 = AtomicU64::new(0);
static HANDLER_INSTALL: OnceLock<Result<(), u32>> = OnceLock::new();

fn increment_active_executions() -> bool {
    let mut active = ACTIVE_EXECUTIONS.load(Ordering::Acquire);
    loop {
        let Some(next) = active.checked_add(1) else {
            return false;
        };
        match ACTIVE_EXECUTIONS.compare_exchange_weak(
            active,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => active = observed,
        }
    }
}

fn generation_changed(starting: u64, current: u64) -> bool {
    starting != current
}

pub(super) struct WindowsCancellation {
    starting_generation: u64,
}

impl WindowsCancellation {
    pub(super) fn install_and_activate() -> Result<Self, ExecutionError> {
        let install_result = HANDLER_INSTALL.get_or_init(|| {
            // SAFETY: the callback is process-static, uses only atomic state, and matches the
            // Windows console-handler ABI. Registration is attempted exactly once.
            if unsafe { SetConsoleCtrlHandler(Some(console_handler), TRUE) } == FALSE {
                // SAFETY: GetLastError reads the error from the immediately preceding Win32 call.
                Err(unsafe { GetLastError() })
            } else {
                Ok(())
            }
        });
        if let Err(code) = install_result {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::UnsupportedContainment,
                format!("cannot install Windows Ctrl+C handler (Win32 error {code})"),
            ));
        }

        let starting_generation = CTRL_C_GENERATION.load(Ordering::Acquire);
        if !increment_active_executions() {
            return Err(ExecutionError::new(
                ExecutionErrorCategory::Internal,
                "active Windows execution counter overflowed",
            ));
        }
        Ok(Self {
            starting_generation,
        })
    }

    pub(super) fn is_cancelled(&self) -> bool {
        generation_changed(
            self.starting_generation,
            CTRL_C_GENERATION.load(Ordering::Acquire),
        )
    }
}

impl Drop for WindowsCancellation {
    fn drop(&mut self) {
        let previous = ACTIVE_EXECUTIONS.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "active Windows execution counter underflowed");
    }
}

unsafe extern "system" fn console_handler(control_type: u32) -> i32 {
    let active = ACTIVE_EXECUTIONS.load(Ordering::Acquire);
    handle_console_event(control_type, active, &CTRL_C_GENERATION)
}

fn handle_console_event(
    control_type: u32,
    active_executions: usize,
    generation: &AtomicU64,
) -> i32 {
    if control_type != CTRL_C_EVENT || active_executions == 0 {
        return FALSE;
    }
    generation.fetch_add(1, Ordering::AcqRel);
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Console::CTRL_BREAK_EVENT;

    #[test]
    fn unrelated_console_event_is_unhandled() {
        let generation = AtomicU64::new(17);
        assert_eq!(
            handle_console_event(CTRL_BREAK_EVENT, 1, &generation),
            FALSE
        );
        assert_eq!(generation.load(Ordering::Acquire), 17);
    }

    #[test]
    fn ctrl_c_without_active_execution_is_unhandled() {
        let generation = AtomicU64::new(17);
        assert_eq!(handle_console_event(CTRL_C_EVENT, 0, &generation), FALSE);
        assert_eq!(generation.load(Ordering::Acquire), 17);
    }

    #[test]
    fn ctrl_c_with_active_execution_advances_generation_and_is_handled() {
        let generation = AtomicU64::new(17);
        let starting = generation.load(Ordering::Acquire);
        assert_eq!(handle_console_event(CTRL_C_EVENT, 1, &generation), TRUE);
        assert!(generation_changed(
            starting,
            generation.load(Ordering::Acquire)
        ));
    }

    #[test]
    fn later_execution_does_not_inherit_an_earlier_ctrl_c() {
        let generation = AtomicU64::new(17);
        assert_eq!(handle_console_event(CTRL_C_EVENT, 1, &generation), TRUE);
        let later_execution_start = generation.load(Ordering::Acquire);
        assert!(!generation_changed(
            later_execution_start,
            generation.load(Ordering::Acquire)
        ));
    }
}
