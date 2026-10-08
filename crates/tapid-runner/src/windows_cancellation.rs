#[cfg(windows)]
use super::{ExecutionError, ExecutionErrorCategory};
use std::sync::Mutex;
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{FALSE, GetLastError, TRUE};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{CTRL_C_EVENT, SetConsoleCtrlHandler};

// Windows invokes console handlers on a dedicated thread, not in an async-signal context.
// This lock covers only integer bookkeeping: never Win32 calls, waits, cleanup or user code.
// Event handling and scope completion must share one linearization boundary, otherwise an
// event can be swallowed after the final cancellation check but before deactivation.
#[cfg(windows)]
static CANCELLATION: Mutex<CancellationState> = Mutex::new(CancellationState::new());
#[cfg(windows)]
static HANDLER_INSTALL: OnceLock<Result<(), u32>> = OnceLock::new();

struct CancellationState {
    active: usize,
    generation: u64,
}

impl CancellationState {
    const fn new() -> Self {
        Self {
            active: 0,
            generation: 0,
        }
    }

    fn activate(&mut self) -> Option<u64> {
        self.active = self.active.checked_add(1)?;
        Some(self.generation)
    }

    fn is_cancelled(&self, starting: u64) -> bool {
        starting != self.generation
    }

    fn finish(&mut self, starting: u64) -> bool {
        self.active -= 1;
        self.is_cancelled(starting)
    }

    fn handle_ctrl_c(&mut self) -> bool {
        if self.active == 0 {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        true
    }
}

#[cfg(windows)]
pub(super) struct WindowsCancellation {
    starting_generation: Option<u64>,
}

#[cfg(windows)]
impl WindowsCancellation {
    pub(super) fn install_and_activate() -> Result<Self, ExecutionError> {
        let install_result = HANDLER_INSTALL.get_or_init(|| {
            // SAFETY: the callback is process-static and matches the Windows console-handler ABI.
            // Registration is attempted exactly once and outside the cancellation-state lock.
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
        let starting_generation = CANCELLATION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .activate()
            .ok_or_else(|| {
                ExecutionError::new(
                    ExecutionErrorCategory::Internal,
                    "active Windows execution counter overflowed",
                )
            })?;
        Ok(Self {
            starting_generation: Some(starting_generation),
        })
    }

    pub(super) fn is_cancelled(&self) -> bool {
        CANCELLATION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_cancelled(self.starting_generation.expect("active cancellation scope"))
    }

    /// End event ownership and snapshot cancellation under the same lock as the handler.
    pub(super) fn finish(mut self) -> bool {
        self.deactivate()
    }

    fn deactivate(&mut self) -> bool {
        let Some(starting) = self.starting_generation.take() else {
            return false;
        };
        CANCELLATION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(starting)
    }
}

#[cfg(windows)]
impl Drop for WindowsCancellation {
    fn drop(&mut self) {
        self.deactivate();
    }
}

#[cfg(windows)]
unsafe extern "system" fn console_handler(control_type: u32) -> i32 {
    if control_type != CTRL_C_EVENT {
        return FALSE;
    }
    if CANCELLATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .handle_ctrl_c()
    {
        TRUE
    } else {
        FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn unrelated_console_event_is_unhandled() {
        use windows_sys::Win32::System::Console::CTRL_BREAK_EVENT;
        // SAFETY: invoke the process-static callback with an event it must not own.
        assert_eq!(unsafe { console_handler(CTRL_BREAK_EVENT) }, FALSE);
    }

    #[test]
    fn cancellation_during_drain_or_cleanup_is_retained_at_finish() {
        let mut state = CancellationState::new();
        let starting = state.activate().unwrap();
        assert!(!state.is_cancelled(starting)); // Child wait has already returned.
        assert!(state.handle_ctrl_c()); // Output draining or ACL restoration is still active.
        assert!(state.finish(starting));
        assert!(!state.handle_ctrl_c());
    }

    #[test]
    fn finished_scope_does_not_swallow_ctrl_c() {
        let mut state = CancellationState::new();
        let starting = state.activate().unwrap();
        assert!(!state.finish(starting));
        assert!(!state.handle_ctrl_c());
    }

    #[test]
    fn later_execution_does_not_inherit_an_earlier_ctrl_c() {
        let mut state = CancellationState::new();
        let earlier = state.activate().unwrap();
        assert!(state.handle_ctrl_c());
        assert!(state.finish(earlier));
        let later = state.activate().unwrap();
        assert!(!state.finish(later));
    }

    #[test]
    fn finishing_one_scope_preserves_other_active_scopes() {
        let mut state = CancellationState::new();
        let first = state.activate().unwrap();
        let second = state.activate().unwrap();
        assert!(!state.finish(first));
        assert!(state.handle_ctrl_c());
        assert!(state.finish(second));
        assert!(!state.handle_ctrl_c());
    }

    #[test]
    fn racing_finish_and_ctrl_c_agree_on_event_ownership() {
        use std::sync::{Arc, Barrier};
        for _ in 0..100 {
            let state = Arc::new(Mutex::new(CancellationState::new()));
            let starting = state.lock().unwrap().activate().unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let handler_state = Arc::clone(&state);
            let handler_barrier = Arc::clone(&barrier);
            let handler = std::thread::spawn(move || {
                handler_barrier.wait();
                handler_state.lock().unwrap().handle_ctrl_c()
            });
            barrier.wait();
            let cancelled = state.lock().unwrap().finish(starting);
            assert_eq!(
                handler.join().unwrap(),
                cancelled,
                "a handled event must be reflected in the completed scope"
            );
        }
    }

    #[test]
    fn counter_overflow_does_not_activate_a_scope() {
        let mut state = CancellationState {
            active: usize::MAX,
            generation: 17,
        };
        assert_eq!(state.activate(), None);
        assert_eq!(state.active, usize::MAX);
        assert_eq!(state.generation, 17);
    }
}
