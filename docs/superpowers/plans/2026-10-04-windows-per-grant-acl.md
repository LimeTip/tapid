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

## Current Evidence and Open Question

The native test `windows_appcontainer_can_modify_existing_file_in_declared_subtree` on VM 126 starts the child, but the child exits 1 with `Access is denied`. During the run, `icacls` showed the existing target file with an inherited ACE for the per-run AppContainer SID (`(I)(W,Rc)`), inherited SYSTEM full control, and an inherited Low mandatory label (`(I)(NW)`). A separate token probe measured the AppContainer token user as `S-1-5-18` (SYSTEM) and integrity RID `4096` (Low). These observations rule out the simple explanations “no AppContainer ACE was applied” and “the file is still Medium integrity,” but do not prove effective access: a directory ancestor, exact requested access mask, token group/capability state, or command-level behavior may still deny the operation.

Do not treat any single hypothesis as the root cause until the probe below differentiates it. The latest work is commit `a359f97` on `feat/windows-2025-runner`; the native write test is still failing and Windows write support remains unverified.

## File Map

- `crates/tapid-runner/src/windows_filesystem.rs` — per-run DACL grants, inheritance/traversal rights, and revocation.
- `crates/tapid-runner/src/windows_execution.rs` — support gate and lifecycle ordering; grants must remain alive until the complete process tree exits.
- `crates/tapid-runner/src/windows_job.rs` and `crates/tapid-runner/src/windows_job_tests.rs` — AppContainer token identity and process setup.
- `crates/tapid-runner/tests/windows_containment.rs` — native positive/negative containment tests.
- `crates/tapid-cli/src/commands/run.rs` — CLI integration, to be exercised after runner behavior is proven.
- `docs/windows-tapid-run-scope.md` — acceptance contract and support status; keep status explicitly unsupported until all criteria pass.

## Task 1: Keep Unverified Writes Fail-Closed

**Files:** `crates/tapid-runner/src/windows_execution.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [ ] Restore a structured `UnsupportedContainment` support-gate response for non-empty project write policies until the native write test passes. Keep the message specific that Windows project writes are unavailable pending native verification.
- [ ] Keep the currently failing native success-path test as the red acceptance test, but run a separate low-level ACL diagnostic test that bypasses the support gate; do not change the acceptance test to expect a silent success or weaken its assertion.
- [ ] Cross-compile the Windows targets and verify the support-gate test proves no child marker was created. The user-facing CLI must not start a child for an unsupported write policy.

## Task 2: Build a Decisive Native Access Probe

**Files:** `crates/tapid-runner/src/windows_filesystem.rs`, `crates/tapid-runner/src/windows_job_tests.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [ ] Add one Windows-only probe that creates a project directory, an existing file, and a new-file target; applies the same read-root and write-subtree grants as the failing integration test; and launches an AppContainer child on VM 126.
- [ ] In the probe, capture and report: AppContainer SID, token user SID, enabled groups, restricted SIDs, capability SIDs, integrity RID, exact file and directory SDDL while grants are active, requested access mask, and Win32 error code for each denied open. Use the actual child token, not assumptions from the parent process.
- [ ] Separate three operations: open/write the existing file; create/write a new file; invoke the existing `cmd.exe` redirection path. This distinguishes target-file ACL, directory-create rights, and shell behavior.
- [ ] Use a duplicated impersonation token plus `AccessCheck`/`CreateFileW` for the exact desired access against the existing file and each path ancestor. Record effective access for both the package SID and normal user/group identity; do not infer access solely from `icacls` text.
- [ ] Run only one variable-changing experiment at a time. Test separately whether the denial is caused by a missing ancestor `FILE_TRAVERSE`, an insufficient file/directory access mask, a protected/non-inheriting DACL, token SID/group mismatch, or the shell's open mode. Do not modify the volume-root DACL in the product path; any temporary diagnostic ACE must be unique to the test AppContainer and its before/after ACL must be checked.
- [ ] Confirm the probe itself is red on the current VM setup and emits enough evidence to discriminate these hypotheses before changing production code.

## Task 3: Apply the Smallest Proven ACL Fix

**Files:** `crates/tapid-runner/src/windows_filesystem.rs`, `crates/tapid-runner/tests/windows_containment.rs`

- [ ] Write the regression assertion for the specific missing right or boundary identified by Task 2; demonstrate it fails before the fix.
- [ ] Implement only the proven change. For directory traversal, grant `FILE_EXECUTE` only on directories and never inherit it to files unless an explicit executable grant requires it. For write access, preserve the distinction between rights on the grant root and rights inherited by descendant files/directories.
- [ ] If a required ancestor cannot be safely granted without changing a shared/protected/system DACL, fail closed with a precise unsupported-containment diagnostic rather than broadening access or silently skipping enforcement.
- [ ] Re-run the targeted native positive test for existing-file modification and new-file creation, then negative tests proving writes to a sibling, parent, undeclared temp/home path, and a reparse/link escape remain denied.
- [ ] Verify read-only grants do not acquire write access and write grants do not grant execute access to ordinary files.

## Task 4: Prove Grant Lifetime, Revocation, and Failure Safety

**Files:** `crates/tapid-runner/src/windows_filesystem.rs`, `crates/tapid-runner/src/windows_execution.rs`, Windows tests under `crates/tapid-runner/src/` and `crates/tapid-runner/tests/`

- [ ] Assert each temporary ACE is applied before child resume and remains until the Job Object confirms the entire process tree is empty.
- [ ] Test cleanup after normal exit, nonzero exit, timeout, output-limit termination, child-spawn failure, and cancellation. After each case, compare DACLs before and after and verify no per-run SID ACE remains.
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
