# Windows Graceful Ctrl+C Cancellation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Windows `tapid run` handle graceful Ctrl+C by terminating and reaping the complete Job Object, restoring per-run ACLs, and returning an explicit cancellation result.

**Architecture:** Add a process-level Windows console handler whose callback only updates atomic state while one or more executions are active; inactive periods pass Ctrl+C through to the normal process handler. Each execution watches the cancellation generation in the existing bounded process wait loop. Cancellation terminates the Job Object, waits for the entire tree to exit, then reuses the current output-capture and ACL cleanup path; the CLI reports `Termination::Cancelled` and exits 130.

**Tech Stack:** Rust 1.99.0, `windows-sys` 0.52 Windows Console APIs, Windows 11 Pro x64 Proxmox VM 126, native `tapid-runner` and `tapid-cli` tests.

## Global Constraints

- Implement graceful Ctrl+C only; hard termination, crashes, power loss, and external cleanup/recovery are out of scope.
- Keep Windows project writes fail-closed; this cancellation work does not enable or claim Windows `tapid run` support.
- Test only on Windows 11 Pro x64 Proxmox VM 126; do not use Windows Server 2025.
- Keep the active draft PR #171 and push incremental commits to `feat/windows-2025-runner`.
- Do not broaden system or volume-root ACLs; cancellation cleanup must use the existing Job Object and per-run ACL lifecycle.
- A cancellation result is valid only after the Job Object is empty and cleanup evidence is complete; cleanup failures remain errors.

---

## File Map

- `crates/tapid-runner/src/windows_cancellation.rs` — new console handler registration, active-run tracking, cancellation generation, and unit tests.
- `crates/tapid-runner/Cargo.toml` — enable the Windows Console API feature in the existing `windows-sys` dependency.
- `crates/tapid-runner/src/execution.rs` — add and validate public `Termination::Cancelled`; register the private Windows-only cancellation module.
- `crates/tapid-runner/src/windows_process.rs` — inspect cancellation during the child/Job Object wait and return an internal cancelled termination only after killing/reaping the job.
- `crates/tapid-runner/src/windows_execution.rs` — own the cancellation scope before grant/process setup and translate internal cancellation to the public result after the normal cleanup sequence.
- `crates/tapid-cli/src/commands/run.rs` — render the cancellation diagnostic and map it to exit code 130.
- `crates/tapid-cli/src/commands/run/tests.rs` — test stable CLI cancellation exit behavior.
- `crates/tapid-runner/tests/windows_containment.rs` — native Ctrl+C helper-process integration test and exact DACL restoration assertion.
- `docs/superpowers/specs/2026-10-05-windows-graceful-ctrl-c-design.md` — update implementation/verification status after native proof.
- `docs/superpowers/plans/2026-10-04-windows-per-grant-acl.md` and `docs/windows-tapid-run-scope.md` — record tested cancellation evidence and remaining write/CLI acceptance gaps.

## Task 1: Define the public cancellation result

**Files:** `crates/tapid-runner/src/execution.rs`, `crates/tapid-cli/src/commands/run.rs`, `crates/tapid-cli/src/commands/run/tests.rs`

**Interfaces:** Add `Termination::Cancelled`. CLI mapping must return `ExitCode::from(130)` and print a specific Ctrl+C cancellation message. Existing termination values retain their current behavior.

- [ ] **Step 1: Write the failing CLI mapping test** in `crates/tapid-cli/src/commands/run/tests.rs`:

```rust
#[test]
fn cancelled_termination_maps_to_sigint_exit_code() {
    assert_eq!(
        termination_exit_code(&tapid_runner::Termination::Cancelled),
        ExitCode::from(130)
    );
}
```

- [ ] **Step 2: Run the focused test and verify RED** with `cargo test -p tapid cancelled_termination_maps_to_sigint_exit_code`. Expected: compile failure because `Termination::Cancelled` does not exist.
- [ ] **Step 3: Add the public result variant** in `crates/tapid-runner/src/execution.rs`:

```rust
pub enum Termination {
    Exited(i32),
    Signaled(i32),
    TimedOut,
    OutputLimitExceeded,
    ProcessLimitExceeded,
    MemoryLimitExceeded,
    Cancelled,
}
```

Update exhaustive matches without changing existing mappings. In `crates/tapid-cli/src/commands/run.rs`, render `error: root package script cancelled by Ctrl+C` and return 130 for `Termination::Cancelled`.

- [ ] **Step 4: Run focused CLI tests** with `cargo test -p tapid cancelled_termination_maps_to_sigint_exit_code`; expected: PASS.
- [ ] **Step 5: Run runner contract tests** with `cargo test -p tapid-runner`; expected: all existing tests pass, confirming the new result is not incorrectly constrained by a resource limit.
- [ ] **Step 6: Commit** the result contract and CLI mapping as `feat(run): represent graceful cancellation`.

## Task 2: Add the Windows console cancellation source

**Files:** `crates/tapid-runner/src/windows_cancellation.rs`, `crates/tapid-runner/src/lib.rs`, `crates/tapid-runner/Cargo.toml`

**Interfaces:** Add private `WindowsCancellation::install_and_activate() -> Result<WindowsCancellation, ExecutionError>` and `WindowsCancellation::is_cancelled(&self) -> bool`. The scope increments/decrements an active-execution count. Its baseline generation is captured before incrementing the count. A Ctrl+C event while active advances a global generation; one event cancels every execution active in that Tapid process. Runs started after the event do not inherit prior cancellation.

- [ ] **Step 1: Add unit tests first** in `windows_cancellation.rs` for (a) unrelated control event is unhandled, (b) Ctrl+C with no active run is unhandled, (c) Ctrl+C with an active run advances its generation, and (d) a run created after an earlier event is not cancelled by that earlier event.
- [ ] **Step 2: Run the focused Windows test** by cross-compiling the unit-test binary and running the pure state tests natively on VM 126. Expected RED: missing module/types.
- [ ] **Step 3: Implement the Windows-only module.** Use `OnceLock<Result<(), u32>>` for one-time `SetConsoleCtrlHandler` registration; `AtomicUsize` for active executions; `AtomicU64` for the event generation. The callback handles only `CTRL_C_EVENT`, returns false when no run is active, and does no allocation, locking, I/O, or cleanup. Map handler-installation errors to a structured pre-spawn `ExecutionError`.
- [ ] **Step 4: Enable `Win32_System_Console`** in the existing Windows `windows-sys` feature list and declare `mod windows_cancellation` under `#[cfg(windows)]` in `lib.rs`.
- [ ] **Step 5: Run the native unit tests** and verify all four state tests pass; also cross-compile the runner library tests with Rust 1.99.0 for `x86_64-pc-windows-gnu`.
- [ ] **Step 6: Commit** as `feat(windows): add scoped Ctrl+C notification`.

## Task 3: Terminate and reap the Job Object on cancellation

**Files:** `crates/tapid-runner/src/windows_process.rs`, `crates/tapid-runner/src/windows_execution.rs`

**Interfaces:** Pass `&WindowsCancellation` into `WindowsSuspendedChild::resume_and_wait_for_status`. Add an internal `WindowsChildTermination::Cancelled` result. The process loop checks cancellation before timeout/output-limit completion; cancellation calls the existing `terminate_job_and_reap(job)` and returns only after that call confirms both root reaping and Job Object emptiness.

- [ ] **Step 1: Add a focused process-wait regression test** using the existing Windows native test helpers. Run a long-lived AppContainer child, activate cancellation, and assert the wait returns `WindowsChildTermination::Cancelled` only after the child is signaled and the Job Object active-process count is zero.
- [ ] **Step 2: Run the new test on VM 126 and verify RED** because no cancellation parameter/result exists yet.
- [ ] **Step 3: Implement the minimal wait-loop change.** At each existing bounded poll, check `cancellation.is_cancelled()`. If true, call `self.terminate_job_and_reap(job)?` and return `Ok(WindowsChildTermination::Cancelled)`. Do not bypass Job Object termination or report cancellation if reaping fails.
- [ ] **Step 4: Update `WindowsExecutionLifecycle::prepare`** to install/activate the cancellation scope before any per-run grants are applied or child processes are created. Keep that scope alive through `cleanup_resources`; pass it to the wait loop. Translate internal `Cancelled` to public `Termination::Cancelled` only after output capture and `cleanup_resources()` succeed.
- [ ] **Step 5: Run focused native tests** for cancellation, timeout, output-limit termination, normal exit, and DACL restoration. Expected: all pass; the fail-closed write-policy test remains unchanged and passing.
- [ ] **Step 6: Commit** as `feat(windows): reap managed tree on Ctrl+C`.

## Task 4: Verify an actual Ctrl+C and ACL restoration on Windows 11

**Files:** `crates/tapid-runner/tests/windows_containment.rs`, Windows test support in `crates/tapid-runner/src/windows_cancellation.rs`

**Interfaces:** Add a helper-process test mode to the Windows containment test executable. The parent starts the helper in an isolated console; the helper starts one long-running managed execution, waits until the project's temporary DACL grant is observable, sends Ctrl+C within its own console, and exits successfully only if it receives `Termination::Cancelled` and its project DACL matches the baseline.

- [ ] **Step 1: Write the parent/helper integration test first.** The parent starts the current test executable with `--exact windows_ctrl_c_helper --nocapture`, a private environment flag, `CREATE_NEW_CONSOLE`, and piped output. The helper creates a temp project and snapshots `icacls` output, then runs a long-lived read-only-policy `execute()` on a worker thread. The helper polls the project DACL until it differs from baseline (the production lifecycle installs the cancellation scope before applying that grant), prints `DACL_ACTIVE`, and calls `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)` from its isolated console. It waits on a channel for the execution result. The parent waits for `DACL_ACTIVE` and then for helper exit within bounded deadlines; every failure path kills and reaps the helper. Since the helper has a new console, the event is isolated from the parent test runner and VM management process.
- [ ] **Step 2: Run the exact integration test on VM 126 and verify RED.** Expected: the current runner has no active Ctrl+C handling and therefore does not return the expected cancelled result. Send Ctrl+C only from the helper after its private-console execution has exposed the active project grant; do not signal the parent test process or the VM control console.
- [ ] **Step 3: Make the helper prove cleanup.** The helper asserts the execution result is `Termination::Cancelled`, `cleanup_confidence()` is `KernelOwnedComplete`, and the post-run project DACL bytes equal the pre-run snapshot. The parent asserts the helper exits before its deadline; a timeout is a test failure and the parent terminates/reaps the helper.
- [ ] **Step 4: Run the native test repeatedly** on VM 126 to check console isolation, then run the full `windows_containment` suite. Expected: no unrelated test process or VM control process receives the event; DACLs match exactly; no child process survives.
- [ ] **Step 5: Cross-build** runner library tests and the Windows containment integration test using Rust 1.99.0 and `x86_64-pc-windows-gnu`.
- [ ] **Step 6: Commit** as `test(windows): verify Ctrl+C cleanup on Windows 11`.

## Task 5: Document evidence and keep the support gate

**Files:** `docs/superpowers/specs/2026-10-05-windows-graceful-ctrl-c-design.md`, `docs/superpowers/plans/2026-10-04-windows-per-grant-acl.md`, `docs/windows-tapid-run-scope.md`

- [ ] **Step 1: Update the design status** with exact Windows 11 build, Rust version, test names, result counts, and the verified Ctrl+C/DACL evidence. Do not mark the full Windows `tapid run` goal complete.
- [ ] **Step 2: Update the ACL plan** to mark only graceful Ctrl+C cleanup as verified. Retain hard termination, write-grant cleanup, external-boundary denials, and full CLI/ManagedTree acceptance as outstanding where they remain outstanding.
- [ ] **Step 3: Re-run the full Windows containment test suite** and the CLI unit tests; record exact output and exit codes before changing docs.
- [ ] **Step 4: Verify no accidental support-gate change** with `git diff -- crates/tapid-runner/src/windows_execution.rs`; project writes must still return `UnsupportedContainment` before spawn.
- [ ] **Step 5: Commit and push** the documentation update to draft PR #171, then verify the local SHA, remote branch SHA, PR #171 head SHA, draft/open state, and current checks.

## Stop Conditions

- If the handler cannot be scoped so ordinary Ctrl+C behavior is preserved when no execution is active, do not ship it; revise the console-handler design first.
- If the native test cannot safely isolate Ctrl+C from the test harness and VM control path, stop before sending events and report the blocker rather than weakening the test.
- If cancellation does not prove Job Object emptiness before ACL restoration, keep the implementation fail-closed and do not claim cancellation cleanup.
- Do not remove the Windows write-policy support gate or claim Windows `tapid run` support as part of this work.
