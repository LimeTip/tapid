//! Process-directed SIGINT cancels every currently owned ManagedTree attempt.
//!
//! Keep the handler alive through error cleanup, not just the child wait. A
//! process-global signal cannot identify one request, so overlapping attempts
//! share registration but each snapshots its own generation before launch.
use super::{ExecutionError, ExecutionErrorCategory};

fn unsupported(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCategory::UnsupportedContainment, message)
}
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

// Low bit owns SIGINT; upper bits count events. Finishing the final scope
// and an interrupt compete on one atomic, so an event cannot be swallowed
// between the final cancellation check and signal disposition restoration.
const ACTIVE: usize = 1;
static STATE: AtomicUsize = AtomicUsize::new(0);
static REGISTRATION: Mutex<Registration> = Mutex::new(Registration {
    users: 0,
    previous: None,
});

struct Registration {
    users: usize,
    previous: Option<libc::sigaction>,
}

extern "C" fn interrupt(_: libc::c_int) {
    // AtomicUsize is lock-free on the supported Linux targets. No allocation,
    // I/O, mutex, or non-signal-safe runtime operation occurs in this handler.
    if STATE
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |state| {
            (state & ACTIVE != 0).then(|| state.wrapping_add(2))
        })
        .is_err()
    {
        // A handler already dispatched on another thread can arrive after the
        // final scope relinquishes ownership. Preserve the default event rather
        // than swallow it. Both operations are async-signal-safe on POSIX.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::kill(libc::getpid(), libc::SIGINT);
        }
    }
}

pub(super) struct Cancellation {
    starting_generation: Option<usize>,
}

impl Cancellation {
    pub(super) fn activate() -> Result<Self, ExecutionError> {
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask) } != 0
            || unsafe { libc::sigismember(&mask, libc::SIGINT) } != 0
        {
            return Err(unsupported(
                "ManagedTree cancellation requires unblocked SIGINT on the invoking thread",
            ));
        }
        let mut registration = REGISTRATION
            .lock()
            .map_err(|_| unsupported("ManagedTree SIGINT registration is poisoned"))?;
        let starting_generation = STATE.load(Ordering::SeqCst) & !ACTIVE;
        if registration.users == 0 {
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut previous) } != 0 {
                return Err(unsupported("cannot inspect ManagedTree SIGINT disposition"));
            }
            // Do not silently replace an embedding application's signal owner,
            // including an explicitly ignored SIGINT disposition.
            if previous.sa_sigaction != libc::SIG_DFL {
                return Err(unsupported(
                    "ManagedTree cancellation requires the default SIGINT disposition",
                ));
            }
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = interrupt as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            STATE.fetch_or(ACTIVE, Ordering::SeqCst);
            if unsafe { libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) } != 0 {
                STATE.fetch_and(!ACTIVE, Ordering::SeqCst);
                return Err(unsupported(
                    "cannot install ManagedTree SIGINT cancellation",
                ));
            }
            registration.previous = Some(previous);
        }
        registration.users += 1;
        Ok(Self {
            starting_generation: Some(starting_generation),
        })
    }

    pub(super) fn is_cancelled(&self) -> bool {
        (STATE.load(Ordering::SeqCst) & !ACTIVE)
            != self.starting_generation.expect("active cancellation scope")
    }

    pub(super) fn finish(&mut self) -> bool {
        let starting = self
            .starting_generation
            .take()
            .expect("active cancellation scope");
        let mut registration = REGISTRATION
            .lock()
            .expect("ManagedTree SIGINT registration");
        registration.users -= 1;
        let final_state = if registration.users == 0 {
            STATE.fetch_and(!ACTIVE, Ordering::SeqCst)
        } else {
            STATE.load(Ordering::SeqCst)
        };
        if registration.users == 0 {
            let previous = registration
                .previous
                .take()
                .expect("registered SIGINT disposition");
            // A failed restoration must not leave the process swallowing future
            // Ctrl+C events after it has relinquished all execution ownership.
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGINT, &previous, std::ptr::null_mut()) },
                0,
                "cannot restore ManagedTree SIGINT disposition"
            );
        }
        final_state & !ACTIVE != starting
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        if self.starting_generation.is_some() {
            self.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_cancellation_scopes_share_events_and_restore_disposition() {
        const HELPER: &str = "TAPID_TEST_MANAGED_CANCELLATION_HELPER";
        if std::env::var_os(HELPER).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution::linux_cancellation::tests::managed_cancellation_scopes_share_events_and_restore_disposition", "--nocapture"])
                .env(HELPER, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("SIGINT scopes verified"));
            return;
        }
        let first = Cancellation::activate().unwrap();
        let second = Cancellation::activate().unwrap();
        assert!(!first.is_cancelled());
        assert!(!second.is_cancelled());
        assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0);
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
        drop(first);
        let mut later = Cancellation::activate().unwrap();
        assert!(
            !later.is_cancelled(),
            "earlier events must not cancel later attempts"
        );
        drop(second);
        assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0);
        assert!(later.is_cancelled());
        assert!(
            later.finish(),
            "cancellation during cleanup is retained at finish"
        );
        drop(later);
        let mut restored: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut restored) },
            0
        );
        assert_eq!(restored.sa_sigaction, libc::SIG_DFL);
        let mut ignored = restored;
        ignored.sa_sigaction = libc::SIG_IGN;
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, &ignored, std::ptr::null_mut()) },
            0
        );
        assert!(Cancellation::activate().is_err());
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, &restored, std::ptr::null_mut()) },
            0
        );
        let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGINT);
        }
        let mut previous_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous_mask) },
            0
        );
        assert!(
            Cancellation::activate().is_err(),
            "blocked SIGINT must fail before launch"
        );
        assert_eq!(
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous_mask, std::ptr::null_mut())
            },
            0
        );
        println!("SIGINT scopes verified");
    }
}
