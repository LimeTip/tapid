# Windows Per-Grant ACLs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make declared Windows project writes work only inside the granted subtree, restore all temporary ACL changes reliably, and pass native Windows 11 acceptance before enabling or claiming support.

**Architecture:** Keep the user-approved per-run AppContainer SID ACL model; do not add staging/copy-back. First isolate why an AppContainer child receives `Access is denied` despite the observed inherited write ACE and Low integrity label. Make one minimal ACL/token change only after a native probe proves the missing right or boundary, then verify grant, denial, and revocation behavior before enabling write policies.

**Tech Stack:** Rust `tapid-runner`, `windows-sys` Win32 security/process APIs, `icacls` for independent ACL inspection, Windows 11 Pro x64 Proxmox VM 126, Node.js 22.

## Global Constraints

- Test only on the Windows 11 Pro x64 Proxmox VM; do not substitute Windows Server 2025.
- Do not claim Windows support until the integrated `tapid run` acceptance in `docs/windows-tapid-run-scope.md` passes on Windows 11.
- Preserve fail-closed behavior for unsupported or unverified write policies; never add an unsandboxed fallback.
- Preserve the approved per-grant ACL approach; do not introduce staging/copy-back without renewed approval.
- Grant only the per-run AppContainer SID and only the rights/paths proven necessary; do not permanently widen volume-root or system ACLs.
- Keep PR #171 draft while acceptance is incomplete; push small, clearly labeled progress commits.

---

## Current Evidence and Confirmed Cause

The original failure is now explained. VM 126's default temp directory is `C:\Windows\SystemTemp`, which is below `SystemRoot`. `WindowsFilesystemGrants::apply` skipped every non-write grant beneath `SystemRoot`, regardless of whether it came from backend runtime or the project policy. Consequently it skipped the project-root read grant. The existing file still inherited the per-run AppContainer write ACE (`(I)(W,Rc)`) and Low mandatory label (`(I)(NW)`), but the AppContainer lacked traversal/read access to the project path. Under the exact production grant list, the child-token probe reproduced Win32 error 5 on the project root, write directory, and existing-file opens; an earlier test label mistakenly described a duplicate project-root check as the SystemTemp ancestor. Creating a new file still succeeded. The DACL inspection showed no inherited project-read ACE. This distinguishes the issue from a missing file-write ACE or a bad integrity level.

The fix now skips only `BackendRuntime` read grants under `SystemRoot`; project-policy grants remain active for projects in `SystemTemp`. A second native probe found unnecessary parent ACL changes: the per-run AppContainer SID was temporarily added to shared SystemTemp as `(X,RA)`, while the volume-root and Windows-directory DACLs remained unchanged. Direct `CreateFileW` probes for `FILE_TRAVERSE`, `FILE_READ_ATTRIBUTES`, and `FILE_LIST_DIRECTORY` returned Win32 error 5 on SystemTemp and the volume root, despite successful writes to declared descendants. Removing the SystemTemp ACE restored its baseline DACL, and the actual child still passed existing-file writes, new-file create/reopen/write, and `cmd.exe` redirection. Therefore the evidence showed no need to modify shared SystemRoot ACLs. `WindowsPathAcl::grant_with_execute` now stops before adding parent ACEs below SystemRoot; the guard canonicalizes the ancestor and SystemRoot and fails closed if either cannot be resolved. A regression test using the noncanonical temp path failed before the guard and passed with SystemTemp unchanged. The actual-token diagnostic probe passed again and verified its sampled ACLs were restored. The integrated existing-file write test had also passed earlier in a one-off diagnostic build with the support gate temporarily bypassed; the gate remains restored. Rust 1.99.0 was used for Windows builds and native test binaries. With the gate restored, the native `tapid-runner` unit suite passed 110 tests (1 diagnostic ignored), including existing grant/restore, undeclared-write denial, timeout, and concurrent-DACL tests; the separate fail-closed write-policy integration test also passed.

A normal-exit cleanup check found no per-run SID ACE remaining on `C:\`, `C:\Windows`, or `C:\Windows\SystemTemp`, and the probe directory was removed. The diagnostic child reports that `cmd.exe` defaults away from the extended (`\\?\`) current-directory path; this did not prevent the absolute-path write, but relative working-directory behavior remains to be checked during CLI acceptance. Negative-path isolation and cleanup after errors, timeouts, and cancellation remain unverified. Keep Windows writes fail-closed until those checks and integrated `tapid run` acceptance pass.

## File Map

- `crates/tapid-runner/src/windows_filesystem.rs` — per-run DACL grants, inheritance/traversal rights, and revocation.
- `crates/tapid-runner/src/windows_execution.rs` — support gate and lifecycle ordering; grants must remain alive until the complete process tree exits.
- `crates/tapid-runner/src/windows_job.rs` and `crates/tapid-runner/src/windows_job_tests.rs` — AppContainer token identity and process setup.
- `crates/tapid-runner/tests/windows_containment.rs` — native positive/negative containment tests.
- `crates/tapid-cli/src/commands/run.rs` — CLI integration, to be exercised after runner behavior is proven.
- `docs/windows-tapid-run-scope.md` — acceptance contract and support status; keep status explicitly unsupported until all criteria pass.

## Task 1: Keep Unverified Writes Fail-Closed

**Files:** `crates/tapid-runner/src/windows_execution.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [x] Restore a structured `UnsupportedContainment` support-gate response for non-empty project write policies until the native write test passes. Keep the message specific that Windows project writes are unavailable pending native verification.
- [x] Keep the existing native success-path test as a red/green acceptance test, and run a separate low-level ACL diagnostic test that bypasses the support gate; do not weaken the acceptance assertion.
- [x] Cross-compile the Windows targets and verify the support-gate test proves no child marker was created. The user-facing CLI must not start a child for an unsupported write policy.

## Task 2: Build a Decisive Native Access Probe

**Files:** `crates/tapid-runner/src/windows_filesystem.rs`, `crates/tapid-runner/src/windows_job_tests.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [x] Add one Windows-only probe that creates a project directory, an existing file, and a new-file target; applies the same read-root and write-subtree grants as the failing integration test; and launches an AppContainer child on VM 126.
- [ ] In the probe, capture and report: AppContainer SID, token user SID, enabled groups, restricted SIDs, capability SIDs, integrity RID, exact file and directory SDDL while grants are active, requested access mask, and Win32 error code for each denied open. Use the actual child token, not assumptions from the parent process.
- [x] Separate three operations: open/write the existing file; create/write and reopen a new file; invoke the `cmd.exe` redirection path. This distinguished the skipped project-root read/traversal grant from file-write rights and shell behavior.
- [x] Use a duplicated impersonation token from the actual child with `CreateFileW` to probe `FILE_TRAVERSE`, `FILE_READ_ATTRIBUTES`, and `FILE_LIST_DIRECTORY` on the existing file and each sampled ancestor. Record operation-level access results and exact Win32 errors separately from DACL inspection; the diagnostic probe did not call `AccessCheck` on descriptors.
- [x] Test one variable at a time for the confirmed issues: compare the original grant list with the SystemRoot project-read grant restored; then remove only the temporary SystemTemp parent ACE while keeping project/write grants active. Descendant writes still passed without that ACE, and no volume-root DACL change was made.
- [x] Confirm the probe itself is red on the current VM setup and emits enough evidence to discriminate these hypotheses before changing production code.

## Task 3: Apply the Smallest Proven ACL Fix

**Files:** `crates/tapid-runner/src/windows_filesystem.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [x] Write the regression assertion for the specific missing right or boundary identified by Task 2; demonstrate it fails before the fix.
- [x] Implement the proven grant fixes: skip `SystemRoot` read grants only for `BackendRuntime` paths, and do not add parent traversal ACEs below `SystemRoot` (notably SystemTemp). Project-policy grants still apply to the project itself; the actual child passes declared descendant operations with the shared SystemTemp ACL unchanged.
- [ ] If a required ancestor cannot be safely granted without changing a shared/protected/system DACL, fail closed with a precise unsupported-containment diagnostic rather than broadening access or silently skipping enforcement.
- [ ] Re-run the targeted native positive test for existing-file modification and new-file creation, then negative tests proving writes to a sibling, parent, undeclared temp/home path, and a reparse/link escape remain denied.
- [ ] Verify read-only grants do not acquire write access and write grants do not grant execute access to ordinary files.

## Task 4: Prove Grant Lifetime, Revocation, and Failure Safety

**Files:** `crates/tapid-runner/src/windows_filesystem.rs`, `crates/tapid-runner/src/windows_execution.rs`, Windows tests under `crates/tapid-runner/src/` and `crates/tapid-runner/tests/`

- [x] Normal-exit ACL cleanup is verified: the native probe snapshots project root, writable subtree, existing file, SystemTemp, SystemRoot, and volume-root DACLs while grants are active and compares them to the pre-grant snapshots after restore. The standalone regression also verifies SystemTemp stays unchanged during the run.
- [ ] Assert each temporary ACE is applied before child resume and remains until the Job Object confirms the entire process tree is empty.
- [ ] Test cleanup after nonzero exit, timeout, output-limit termination, child-spawn failure, and cancellation. After each case, compare DACLs before and after and verify no per-run SID ACE remains.
- [ ] During revocation, preserve unrelated concurrent DACL changes and remove only the current run's unique AppContainer ACE. If revocation fails, return an explicit cleanup error and do not report complete cleanup.
- [ ] Test that denied writes do not create/modify data and that no child starts when grant application fails.

## Task 5: Re-Enable Writes Only After Native Pass

**Files:** `crates/tapid-runner/src/windows_execution.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [ ] Remove the temporary support gate only after the existing-file and new-file positive controls, outside-grant negative controls, and revocation/failure tests pass on VM 126.
- [ ] Run all `tapid-runner` Windows containment tests natively on Windows 11 Pro x64 build 26300; record exact test names, exit codes, logs, and artifact digest. Cross-compilation is not native acceptance.
- [ ] Keep the feature marked unsupported if any filesystem boundary or cleanup case remains inconclusive; do not convert access-denied child exits into claimed success.

## Task 6: Integrated CLI Acceptance and PR Evidence

**Files:** `crates/tapid-cli/src/commands/run.rs`, `docs/windows-tapid-run-scope.md`, PR #171

- [ ] Build the current feature branch for the Windows 11 VM and run `tapid run`—not `npm run`—for `examples/news-site-consumer` build, test, and start using the Tapid-managed fixture dependencies and Node.js 22.6.0.
- [ ] Verify project-local tool resolution, exact arguments, stdout/stderr, exit codes, readiness response, declared filesystem writes, undeclared-path denials, network policy controls, environment filtering, descendants, limits, cancellation, and complete cleanup against `docs/windows-tapid-run-scope.md`.
- [ ] Record the VM OS/build, architecture, Node/Rust versions, artifact digest, commands, exit codes, and results in the scope document. Keep its support status “not supported yet” until every listed criterion passes.
- [ ] Push incremental commits to the existing draft PR #171 and verify its head SHA and live checks after each push. Do not mark the PR ready or claim Windows support before the integrated acceptance is green.

## Explicit Alternatives and Stop Conditions

1. **Preferred:** retain per-run AppContainer SID ACLs and correct only the demonstrated access-check failure, while keeping ACL lifetime/revocation inside the existing execution lifecycle.
2. **If ACL correctness cannot be demonstrated:** keep writes unsupported and fail closed; present the evidence and a separate Windows isolation design for review before implementation.
3. **Not approved:** staged copy/copy-back or broad permanent grants. Do not switch to either as an expedient workaround.

Stop and revisit the architecture if three evidence-backed, one-variable ACL fixes fail or if the only way to pass is a persistent/shared ACL change. The decision then is whether a different Windows enforcement primitive can satisfy the same positive-write, outside-denial, and cleanup contract—not whether to relax the contract.
