# Graceful Windows Ctrl+C Cancellation Design

## Status

Design approved in chat on 2026-10-05. Graceful Ctrl+C implementation and its native cancellation/ACL-cleanup acceptance are verified on Windows 11 VM 126. This design covers Ctrl+C only; forced termination, crashes, and crash-recovery cleanup remain out of scope. Windows project writes remain fail-closed, and this does not establish overall Windows `tapid run` support.

## Goal

When a user presses Ctrl+C while a Windows `tapid run` execution is active, stop the complete managed process tree, wait for the Windows Job Object to become empty, restore per-run filesystem ACLs, and return a clear cancellation result instead of terminating Tapid before cleanup.

## Current behavior and constraints

The Windows runner's public `execute()` path is synchronous. Its process wait loop already polls the child and routes timeout/output-limit termination through Job Object termination and cleanup. The CLI currently has no Windows Ctrl+C interception. The lifecycle's `Drop` fallback is not sufficient for a user interrupt if the OS terminates the process before ordinary cleanup runs.

The approved scope is graceful console Ctrl+C while Tapid is alive. No guarantee is made for `taskkill /F`, process crashes, power loss, or other hard termination. No external cleanup broker is introduced. This work does not re-enable project write grants or establish overall Windows support.

## Design

1. **Console notification:** Add the Windows console-control API feature and a process-level Ctrl+C handler. Install it lazily for the lifetime of the process, but make it transparent while no runner execution is active: return unhandled when the active-run count is zero. During execution, record an atomic cancellation generation and report Ctrl+C handled so Tapid can perform cleanup.
2. **Concurrent runs:** Track active executions and a generation counter atomically. Each active execution captures its generation; one Ctrl+C event cancels every execution active in that Tapid process. Runs started after the event capture the new generation and are not spuriously cancelled.
3. **Runner wait loop:** Poll the generation alongside the existing timeout and output-limit checks. On cancellation, terminate the whole Job Object, wait for the root process and Job Object active-process count to reach zero, then follow the existing output-capture and ACL/AppContainer cleanup path.
4. **Result contract:** Add `Termination::Cancelled`. Preserve captured output and normal completion evidence. The CLI prints a cancellation diagnostic and exits with code 130, matching Tapid's existing Unix SIGINT convention.
5. **Failure behavior:** If the console handler cannot be installed, fail before spawning the child with a structured error; do not run a process that Tapid cannot gracefully cancel under this contract. A cleanup failure remains an explicit error and must not be reported as a successful cancellation.

## Verification

- Unit-test the handler's inactive/active behavior and generation semantics, including concurrent active runs.
- Add a Windows-native integration test that launches a helper execution, delivers Ctrl+C to that helper, and verifies: cancellation is reported; the complete Job Object is empty; the helper and descendants do not survive; and the project DACL exactly matches its baseline afterward.
- Verify timeout and output-limit paths still pass, and Ctrl+C does not cancel a run when no execution is active.
- Cross-compile with Rust 1.99.0 and run the native tests on Windows 11 Pro x64 VM 126. Do not use Windows Server 2025.
- Keep the existing write-policy fail-closed test passing. Do not claim Windows support until the separate full CLI/ManagedTree acceptance is complete.

## Alternatives rejected

- **Immediate default Ctrl+C termination:** skips Tapid's orderly Job Object wait and ACL cleanup.
- **Hard-termination recovery:** requires an external process/service or later recovery mechanism, which is outside the user's chosen graceful-only scope.
- **Generic execution error instead of a cancellation result:** obscures an expected user action and does not provide a stable CLI result.

## Resolved implementation detail

The native integration test isolates Ctrl+C in a `CREATE_NEW_CONSOLE` helper process. The test passed five consecutive times on VM 126 without signaling the parent test runner or VM management console.

## Verification result (2026-10-05)

- Environment: Windows 11 Pro x64 VM 126, build 26300; Rust 1.99.0; Windows GNU target `x86_64-pc-windows-gnu`.
- `windows_ctrl_c_cancels_execution_and_restores_project_dacl`: passed 5/5 on the native VM. Each run sent `CTRL_C_EVENT` from a `CREATE_NEW_CONSOLE` helper only after the per-run project DACL changed; it returned `Termination::Cancelled`, reported `KernelOwnedComplete`, and restored the project DACL exactly.
- Full `windows_containment` test binary: 17 passed, 0 failed, 1 ignored; exit 0. This includes timeout/output-limit paths and `windows_write_policy_fails_closed_before_spawn_until_native_acceptance`.
- Runner library unit tests: 115 passed, 0 failed, 1 ignored; exit 0. All four cancellation-state tests passed.
- CLI cancellation mapping test `cancelled_termination_maps_to_sigint_exit_code`: passed. The full 95-test CLI unit binary had 93 passed and 2 failed (exit 101); the failures are existing tar-upgrade fixture tests that require an unavailable program on the Windows VM (`program not found`).
- Cross-compilation of the runner library tests, Windows containment integration test, and CLI library tests succeeded with Rust 1.99.0.
- Project writes still return `UnsupportedContainment` before spawn. Nonzero-exit/write-grant cleanup, external-path and reparse-point denial coverage, and full CLI/ManagedTree acceptance remain outstanding.
