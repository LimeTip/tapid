# Isolated Node Runner Failure Recovery Implementation Plan

> **For agentic workers:** Use task-specific isolated worktrees and TDD. Integrate reviewed commits into the existing draft PRs without merging.

**Goal:** Deliver verified read-only isolated Node execution on Windows 11 VM 126 and fix independently confirmed runner regressions.

**Architecture:** Retain AppContainer, Job Object ownership and per-run ACL restoration. No replacement sandbox or new platform service. Treat macOS output replay and runtime relocation separately from Windows execution.

**Tech Stack:** Rust workspace, windows-sys, native Windows 11 tests, Node consumer fixtures, GitHub draft PRs.

## Global Constraints
- Windows behavioral acceptance only on Windows 11 VM 126, never Windows Server 2025.
- Never broaden volume-root/system ACLs. Required unsafe grants fail closed.
- Writes and networking remain unsupported/fail-closed in this read-only slice.
- Preserve all unrelated edits in /Users/doug/tapid.
- Each worker uses its own branch/worktree. Parent owns integration and publication.
- Match cargo, rustc and rustdoc versions; reuse cached builds and compressed artifact transfers.
- Do not claim issue #157 complete until its full acceptance is satisfied.

## Evidence and task boundaries
- CI 37366277290 macOS consumer failed `2 !== 1`: captured child output replayed after native streaming.
- Same CI architecture test rejected `outcome.stdout()` despite Windows requiring output forwarding.
- Windows consumer timed out on first `run` after 60s, not on install; this does not identify its root cause.
- Windows Ctrl+C passed three native repetitions with Cancelled and exact DACL restoration.
- Separate local Homebrew Node snapshot failed loading @rpath/libnode.127.dylib.

### Task 1: Output contract and CI diagnostic fixes (parent)
Files: crates/tapid-cli/src/commands/run.rs, commands/run/tests.rs, tests/architecture.rs, tests/fixtures/validate_consumer_project.js, .github/workflows/ci.yml.
- [ ] Prove regression test fails when macOS captured output is replayed.
- [ ] Keep Windows output forwarding while preventing macOS replay; behavioral test asserts exact bytes/empty replay depending on platform.
- [ ] Update stale architecture assertion without replacing behavioral checks with source-only checks.
- [ ] Preserve diagnostic invocation metadata and bounded stdout/stderr on fixture failures.
- [ ] Remove Windows Server consumer execution; native acceptance is VM 126. Retain compile/installer checks separately.
- [ ] Run matching-toolchain CLI tests and formatting; commit and push to #208; read back exact remote SHA.

### Task 2: Windows real Node execution (isolated Windows worker)
Files: focused Windows process/execution/containment files and a native acceptance harness as dictated by reproduced cause.
- [ ] Run smallest real Node probe on VM 126 as interactive limited tapidtest; verify artifact hashes, OS and runtime version.
- [ ] Record failing stage, Win32 error or child status before editing production code.
- [ ] Write targeted failing regression; implement minimal fix without broad ACLs or unsupported write/network grants.
- [ ] Run real Node argv/environment/exit tests and read/write/network denial tests.
- [ ] Run containment/cancellation tests and verify exact ACL restoration and task/artifact cleanup.
- [ ] Commit evidence and code locally; parent reviews and integrates into appropriate draft PR.

### Task 3: macOS runtime relocation (isolated macOS worker)
Files: runtime binding/snapshot code, focused regression tests.
- [ ] Reproduce Homebrew dynamic-library snapshot failure.
- [ ] Test secure relocatable dependency handling, or explicit preflight rejection if safe support cannot be bounded.
- [ ] Preserve source runtime identity and write-denial protections; do not mutate trusted source binaries.
- [ ] Run focused runtime and hardlink tests with matching toolchain.
- [ ] Commit independently; parent reviews and publishes separately if needed to preserve PR split.

### Task 4: Integration gates (parent)
- [ ] Review exact worker diffs, verify reported test output and security constraints.
- [ ] Integrate independently testable commits and push progress.
- [ ] Run combined native acceptance and check hosted CI against exact pushed SHA.
- [ ] Report actual passing gates, remaining failures and incomplete #157 scope without treating compilation as Windows acceptance.
