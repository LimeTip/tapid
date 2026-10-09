# Windows Existing-Target Writes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable narrowly scoped writes to existing declared Windows project targets only after native confinement, revocation and real build/test acceptance pass.

**Architecture:** Reuse AppContainer/Job ownership and declared-target ACL grants. Add a private Windows write-target validation seam before mutation, explicit rollback reporting, and narrowly tested mutation rights; keep the production support gate until the supported matrix is green. No new runtime or networking subsystem.

**Tech Stack:** Rust 1.99.0 aligned cargo/rustc/rustdoc/Clippy, windows-sys Win32 APIs, Node 22, Python bounded native harness, Windows 11 Pro VM126 limited interactive account.

## Global Constraints

- Approved spec: `docs/superpowers/specs/2026-10-08-windows-declared-write-design.md`.
- Existing explicitly declared ordinary targets only; missing directories remain fail-closed and are not materialized.
- Never broaden volume-root, SystemRoot, shared-ancestor, home or shared-temp ACLs.
- Never grant full control, WRITE_DAC, ownership or ancestor deletion rights to the child.
- No production mandatory-integrity-label lowering without a separately approved design.
- CanonicalPath remains CanonicalPath; retain its trusted-host race limitation, never claim NativeObject.
- Network-enabled policies remain unsupported. Unchanged news-site start is a prelaunch rejection, not successful readiness. Issue #157 remains open.
- News-site Restricted acceptance and separate ManagedTree completion/resource acceptance are distinct.
- Native VM126 is behavioral authority; hosted windows-latest is complementary. Preserve exact source/artifact hashes and logs.
- Keep original user checkout untouched. Each worker owns a unique worktree/branch; one worker owns native VM at a time.
- No automatic merge, approval or issue closure. Push progress to a draft follow-up PR only under the existing user authorization.

## File Responsibilities

- `crates/tapid-runner/src/windows_filesystem.rs`: per-target grant masks, explicit grant transaction and revocation.
- New `crates/tapid-runner/src/windows_write_validation.rs`: private Windows topology/identity/authority validation for resolved write grants; no grant mutation.
- `crates/tapid-runner/src/execution.rs`: private module wiring only.
- `crates/tapid-runner/src/windows_execution.rs`: support gate and pre-mutation validation wiring; network gate retained.
- `crates/tapid-runner/tests/windows_containment.rs`: integrated positive/negative lifecycle probes; retain neighboring read-only tests.
- `crates/tapid-runner/src/windows_job_tests.rs`: direct actual-token grant/revocation controls and narrowly scoped mask tests.
- `crates/tapid-cli/tests/cli.rs`, `tests/run_planning.rs`: supported writes versus unsupported missing/network cases and exact receipt contracts.
- `docs/windows-tapid-run-scope.md`, `docs/platform-validation.md`: verified scope and evidence only.
- Native evidence/harness initially outside repository under private scratch; publish a sanitized bounded receipt and useful logs in the follow-up evidence directory.

## Task 1: Native ordinary-project write and mutation contract

**Files:** `crates/tapid-runner/tests/windows_containment.rs`, `crates/tapid-runner/src/windows_job_tests.rs`.

**Interfaces:** Consume existing `WindowsPathAcl::grant`, `WindowsAppContainer`, `WindowsSuspendedChild`, `WindowsJob` direct-test APIs and existing `managed_policy_with_flags` helper. Produce named native tests for ordinary medium-integrity writes and mutation operations; do not bypass the integrated production gate.

- [ ] Refresh follow-up branch from latest upstream/main without discarding approved spec. Inspect AGENTS.md, runner README, ADR0005 and current gate/test owners. Run the existing read-only native baseline and preserve exact hashes.
- [ ] Write direct-backend native tests using an ordinary limited-account disposable root, without `icacls` label changes. Exercise this Node payload under a granted existing writable directory:

```javascript
const fs = require('node:fs');
fs.writeFileSync('writable/existing.txt', 'overwrite');
fs.appendFileSync('writable/existing.txt', '+append');
fs.truncateSync('writable/existing.txt', 4);
fs.mkdirSync('writable/nested');
fs.writeFileSync('writable/nested/new.txt', 'created');
if (fs.readFileSync('writable/nested/new.txt', 'utf8') !== 'created') throw Error('reopen');
fs.renameSync('writable/nested/new.txt', 'writable/nested/renamed.txt');
fs.writeFileSync('writable/replacement.txt', 'replacement');
fs.renameSync('writable/replacement.txt', 'writable/existing.txt');
fs.unlinkSync('writable/nested/renamed.txt');
fs.rmdirSync('writable/nested');
```

- [ ] Run focused native tests before changing masks; retain each expected failure and diagnostic. Existing-file write failure from mandatory integrity is a STOP boundary, not justification for changing test labels.
- [ ] Add narrowly justified mutation masks only after direct evidence. A DirectorySubtree may grant deletion to descendants for internal rename/unlink, not delete permission on the declared root or ancestor. ExactFile retains write-data semantics, not parent-entry replacement. Retain separate write-only/read-denied tests.
- [ ] Re-run mask and denial controls and same-SID access after restoration. Verify full DACL/control equality for compatible fixtures and no active ancestor/sibling changes. Commit tested mask work independently; keep production gate closed.

**Commands:** `python3 scripts/dev.py test -p tapid-runner --locked --target x86_64-pc-windows-gnu --no-run --message-format=json`; execute resulting artifacts on VM126 with finite task supervision. Compilation does not satisfy this task.

## Task 2: Validate supported write topology before mutation

**Files:** new `windows_write_validation.rs`, private module wiring in `execution.rs`, tests in the new module and `windows_containment.rs`.

**Interfaces:** Produce `validate_existing_write_grants(grants: &[ResolvedFilesystemGrant]) -> Result<(), ExecutionError>`; consumers must call it before `WindowsFilesystemGrants::apply` makes any changes. Use current resolved grant accessor methods for path/access/kind/source/binding. Do not introduce public API.

- [ ] Write native rejection tests for missing leaf/components, non-directory ancestor, protected root/descendant, denied host WRITE_DAC, NULL-DACL ambiguity, outside-target junction, nested/dangling reparse point and outside hard-link sentinel. Pair outside-write denial with a host positive control and assert no child marker/no changed ACLs.
- [ ] Write native positive tests for existing exact file and ordinary directory subtree with spaced/Unicode names and no unsupported topology.
- [ ] Implement a read-only validation walk using no-follow metadata/opens, Windows reparse attributes, file identity and hard-link counts. Conservatively reject multiple-link writable files rather than trying to infer all aliases. Reject protected unsupported descendants instead of claiming inheritable authority. Reject observed identity/kind changes between resolved target and pinned native ACL handle.
- [ ] Keep handles scoped and all inspection errors fatal; do not use immutable-runtime fallback for project writes. Do not recursively traverse through reparse points. Avoid unrelated runner module refactors.
- [ ] Run native RED/GREEN, exact ancestor/sibling snapshots, cross-target Clippy and formatting. Commit validator/tests with the support gate still closed.

**Receipt contract:** supported grants retain current CanonicalPath labeling; tests must reject any claim of NativeObject. Native opens verify the selected object at grant time but do not remove the documented trusted-host race assumption.

## Task 3: Explicit rollback and write-grant lifetime

**Files:** `windows_filesystem.rs`, `windows_execution.rs`, native lifecycle tests.

**Interfaces:** Preserve `WindowsFilesystemGrants::apply(container, grants) -> Result<Self, ExecutionError>` and `restore(&mut self) -> Result<(), ExecutionError>`. On application failure restore all completed grants in reverse order and include rollback failure in the returned structured error; never turn a preparation error into success.

- [ ] Add a deterministic native partial-application test: first valid writable subtree, then denied target. Verify earlier grants lose this SID and current unrelated ACEs remain. Add test-only failure injection at the restoration seam to prove a rollback failure is surfaced rather than discarded by Drop.
- [ ] Implement explicit transaction failure handling around every loop error path, including validation/runtime inspection failures after prior grants. Preserve RAII as emergency cleanup, not primary observable rollback.
- [ ] Add normal zero/nonzero, timeout, output limit, Ctrl+C, missing executable and actual child-creation failure coverage for write policies via backend seams while integrated gate remains closed. Child payload creates files and nested directories so revocation is tested on newly created objects too.
- [ ] Test read-root/write-child, overlapping/duplicate write roots and concurrent unrelated ACE additions. Preserve current DACL state and protection rather than restoring a stale snapshot. Verify same-SID reopen/write fails after revocation.
- [ ] Require empty Job before ACL restoration for ManagedTree; independently assert Restricted reports its weaker completion dimensions honestly. Retain process/memory/subprocess/resource/environment regression owners.
- [ ] Native RED/GREEN and affected runner crate tests/Clippy/format, then commit.

## Task 4: Enable validated existing-target writes and CLI acceptance

**Files:** `windows_execution.rs`, `windows_containment.rs`, CLI `tests/cli.rs`, `tests/run_planning.rs`.

**Interfaces:** `containment_support` stops rejecting all write requests only after tasks1–3 native evidence passes. Binding and validation still reject missing/unsafe targets before grants or spawn. Preserve the network=true unsupported path exactly.

- [ ] Replace the blanket write-gate regression with integrated supported-target and unsupported-target contracts. Remove test-only integrity lowering from the existing ignored positive; enable it only if ordinary-account native acceptance passes.
- [ ] Run integrated tests first and record expected unsupported failure from the current gate. Then remove only the blanket write rejection and wire the approved validator before mutation.
- [ ] Verify actual CLI write success and exact backend/assurance/binding receipts, existing exact-file limitation, missing target rejection, denied rights/topology rejection with absent marker and receipt, and network=true prelaunch rejection even with writes configured.
- [ ] Run full native library/containment and read-only source consumer validator, plus native CLI cases. Re-run host tests, all-target Windows Clippy and full release/workflow/documentation contracts. Commit and push a draft follow-up PR referencing #157 without closing it.

## Task 5: Real news-site build/test and documented remaining start block

**Files:** native bounded acceptance harness under scratch, sanitized evidence, `docs/windows-tapid-run-scope.md`, `docs/platform-validation.md`. Change CI only where its native supported-write test owner actually needs configuration; retain all platform security checks.

- [ ] Install Windows dependencies with the integrated Tapid binary into a disposable fixture copy, not npm-run substitution. Record exact dependency graph, install result, frozen/offline replay lock hash, runtime/artifact/source provenance and isolated environment.
- [ ] Precreate .next/.tmp explicitly for existing-target semantics. Configure HOME/TMPDIR and necessary platform-required environment through the explicit allowlist only; do not inherit secrets. Assert outside sentinels/dependency manifests unchanged.
- [ ] Run `tapid run build`, `tapid run test`, then repeat both. Require actual .next output and actual test results, stable expected exit/output, local tool resolution and argument forwarding. Diagnose failure before changing scopes; no full-control or unsandboxed workaround.
- [ ] Run unchanged `tapid run start` and require a nonzero structured unsupported network result before Node launch. Record start readiness as BLOCKED, not passed. Run separate ManagedTree acceptance for complete tree/resource/cancellation dimensions.
- [ ] Verify no surviving processes/tasks and revoked temporary SID grants across existing and newly created outputs. Retain native logs/digests and limits of legacy metadata restoration.
- [ ] Update docs to claim only verified existing-target writes/build/test on exact Windows11 configuration. Leave networking/start and missing-directory materialization deferred and #157 open. Full exact-head hosted CI must pass; review/merge remains user-controlled.

## Final self-review and evidence gate

- [ ] Map every approved spec requirement to tasks1–5; do not imply start or automatic materialization support.
- [ ] Check every newly supported permission has a native positive and denial/revocation test.
- [ ] Check ignored/conditional native tests are enumerated, not counted as acceptance.
- [ ] Independent code/security review before ready-for-review transition; verify exact head and latest main integration.
- [ ] No completion claim until real build/test, supported write matrix, cleanup and exact-head CI are green. Report blockers plainly and retain the production write gate if required evidence fails.
