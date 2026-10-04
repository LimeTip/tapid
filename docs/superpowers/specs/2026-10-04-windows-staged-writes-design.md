# Windows staged writable filesystem with brokered copyback

## Status

Design direction approved by Arvid for specification and planning on 2026-10-04. This document is submitted for user review; implementation must not begin until the spec is accepted.

## Context

The Windows runner grants filesystem rights by adding an AppContainer SID to target DACLs. Native Windows 11 Pro VM probes show that the execution token has low integrity (RID 4096). An explicit write ACE does not let that token modify an ordinary host-created file, and a subtree grant that permits creating a new file does not permit appending to that file later. Consequently, DACL grants alone do not implement the declared write policy.

The existing execution contract resolves project-relative read/write entries into typed grants (`ExactFile`, `ExactDirectory`, `DirectorySubtree`) and executes with a validated project root, executable search paths, explicit environment, and bounded process/output controls. The Windows implementation must not claim write enforcement unless changes to the original project are mediated and verified.

## Goals

- Make declared Windows project writes work for existing and new files without granting the untrusted script direct write access to host project files.
- Preserve the declared distinction between read-only, write-only, and read/write paths.
- Keep project data outside declared grants inaccessible to the workload.
- Commit only changes covered by declared write grants, after workload processes can no longer modify staged data.
- Detect original-tree conflicts and unsafe staged objects before copyback.
- Bound staging storage and report exact enforcement and copyback outcomes.
- Keep Windows experimental until native runner and `tapid run` acceptance pass at a clean commit.

## Non-goals

- Making Windows platform support claims or changing other OS backends.
- Granting network access; Windows `network = true` remains fail-closed.
- Rewriting opaque user arguments or arbitrary command text to replace original absolute paths with staged paths.
- Providing globally atomic multi-file commits. Replacement is atomic per file; a later commit failure can leave an explicitly reported partial set.
- Following or committing reparse points, hard links, or alternate data streams in v1.

## Proposed architecture

### 1. Original project and policy mapping

The validated original project root remains the source of truth. A per-run staging root mirrors the project-relative paths needed by the union of effective read and write grants. Runtime grants (shells, runtimes, and backend files) remain separate and read-only; they are not copied into the project stage.

The runner creates an explicit mapping from each original granted path to its staged counterpart. It must map the staged project root, working directory, and project-relative executable search directories consistently. It must not rewrite opaque forwarded arguments or arbitrary script text; references that name the original absolute project path continue to refer to the original and are denied write access by the workload AppContainer.

### 2. Trusted stage bootstrap

Before launching untrusted code, a minimal trusted bootstrap runs under its own low-integrity AppContainer identity with no network access and a separate bounded Job Object. The host-side broker creates a private ingress snapshot inside the same bounded stage volume from only the validated policy inputs, using no-follow opens and stable file identities. Its ACL grants ingress reads to the bootstrap and host broker only; the distinct workload AppContainer SID receives no access. For an existing write-only target, the trusted broker includes baseline bytes so append and overwrite semantics can be preserved; those bytes are never exposed as readable data to the workload. The bootstrap reads ingress files and creates staged files itself under the low-integrity token, so the workload can modify them. It runs no project scripts, invokes no project-controlled helpers, inherits no ambient credentials, and passes no source handles to the workload. No DACL is modified on original project paths. If ingress creation, stage creation, or bootstrap cleanup fails, workload launch is refused.

The exact bootstrap transport and helper packaging are implementation-plan items. Their first native prototype must prove that the bootstrap can copy an existing source file into a low-integrity staged file while the workload cannot read ingress, original paths, or write-only staged contents.

### 3. Workload AppContainer

The workload runs in its own suspended-before-resume process lifecycle and its own Job Object. It receives access only to the stage paths corresponding to its effective project grants, plus required runtime paths and explicit standard handles. Original project paths receive no write ACE for the workload identity. Staged ACLs reflect policy exactly:

- read-only grant: read access to the staged object;
- write-only grant: write access without file-data read access;
- read/write overlap: both rights;
- exact-file/directory and subtree kinds retain their declared boundaries.

Descendants inherit the same AppContainer restrictions and Job membership. Existing Node-specific compatibility behavior (`NODE_OPTIONS` symlink flags) remains separately documented and tested.

### 4. Validation and brokered copyback

Copyback starts only after the workload Job is confirmed empty. The trusted broker then:

1. Enumerates the stage without following reparse points and builds a bounded change manifest.
2. Rejects entries outside declared write scopes, unexpected types, reparse points, hard links, alternate streams, and changes exceeding the staging budget.
3. Acquires no-follow destination handles/locks that exclude concurrent writes or replacement for the commit window, then revalidates original identities and compares them with the run-start snapshot. Any conflict before the first replacement aborts all copyback. New targets require holding and validating the destination parent identity and confirming nonexistence.
4. Prepares replacement data in same-volume temporary files beside their destinations, avoiding an atomic-rename assumption across volumes.
5. Replaces, creates, or deletes only declared paths using no-follow, identity-checked operations while retaining the destination locks. It does not copy staged owner, DACL, SACL, or other security descriptors onto host files. If Windows cannot provide a race-safe operation for a target type, copyback fails closed.
6. Reports per-path commit results. Each individual replacement must be atomic. Multi-file copyback is not globally atomic; on a mid-commit failure the outcome must list committed and uncommitted paths and must never report complete success.

Copyback occurs after any contained terminal outcome, including nonzero exit or timeout, if and only if the entire Job is confirmed empty and validation succeeds. Startup failure, uncertain cleanup, validation failure, or a detected original-tree conflict discards the staged changes rather than mutating originals.

### 5. Limits, cleanup, and evidence

The staging root resides on an OS-enforced per-run capacity volume whose maximum size is `max_staging_bytes`; a user-mode watcher alone is not a disk quota. V1 permits at most one write-enabled Windows execution at a time per user profile. A second request fails before spawn with a busy/resource error while a run or its reaper owns the reservation. Reserve the stage capacity and one bounded copyback temporary before spawn. If Windows cannot provide the bounded volume without extra privileges or a hard cap, writable execution fails closed.

Add portable `max_staging_bytes` and `max_staging_entries` execution limits. A Windows policy with write grants must have finite values for both; missing or unrepresentable limits fail before untrusted spawn. The byte limit covers copied baseline content and new/modified staged content. Copyback uses one same-volume sibling temporary file at a time, capped at `max_staging_bytes`; reserve the stage and temporary capacity before spawn and recheck available destination space before the first replacement. Exceeding either limit terminates the workload, skips all copyback, and reports a capacity failure.

Cleanup has a fixed five-second deadline after termination is requested. If the Job cannot be confirmed empty, do not copy back. Close/terminate the Job using the kernel-owned kill boundary, return cleanup-incomplete evidence, and transfer the Job handle plus private stage to a bounded host reaper that removes the stage only after active-process count reaches zero. If the reaper queue is full, reject new writable executions before spawn. At startup, recover an abandoned stage only after proving its named Job is absent or empty; if that cannot be proven, retain the stage and keep writable execution disabled for that profile. A persistently unqueryable or unkillable Job keeps its stage quarantined; it must never be committed or reported as clean. Cleanup code must not block the caller indefinitely or retry forever on persistent query failure.

Launch evidence must distinguish direct OS read restrictions, staged write authority, and brokered copyback. Completion evidence records whether copyback was skipped, fully completed, or partially completed, plus the exact committed paths or a safe summary. It must not describe stage writes as direct writes to the original project.

## Security invariants

- The untrusted workload never receives a writable handle or DACL grant to an original project path; before resume, negative controls must prove it cannot read or write original project objects outside the staged view, including objects whose DACL grants broad principals. If that isolation cannot be established without editing original DACLs, the run fails closed.
- Bootstrap and workload identities are separated by process lifetime and access rights; only the workload executes project-controlled code.
- The stage root is unique, private to the run, protected from other users, and bounded by explicit disk and entry-count limits.
- Copyback validates paths relative to held destination-directory identities, rejects path traversal/reparse substitution, and does not follow attacker-created links.
- Original files are changed only after full-tree cleanup confirmation and conflict validation.
- Policy failure, staging failure, incomplete cleanup, and copyback uncertainty never produce a success receipt.
- A failed execution may still commit declared writes only after verified cleanup, matching the user-approved policy semantics; startup and cleanup failures never commit.

## Acceptance tests

All tests run on the Windows 11 Pro VM, then through the exact `tapid run` CLI path at the clean commit being evaluated.

- Modify, overwrite, and append to an existing host file when write is granted; verify the original changes only after the workload Job empties.
- Create files and nested directories under a granted subtree; reject sibling, parent, home, temp, and out-of-scope writes.
- Verify write-only staged files can be modified but cannot be read by the workload; verify read-only and read/write combinations separately.
- Confirm the bootstrap can seed existing files under the low-integrity token, exits before workload launch, and does not leak source handles or read access to descendants.
- Verify the workload is denied direct reads and writes on the original project tree outside the staged view, including a fixture whose DACL grants broad principals.
- Verify subprocesses, timeout, output limits, process/memory limits, and network denial continue to work with staged working directories and mapped executable search paths.
- Verify ungranted project data is not copied into or readable from the stage; test hostile environment values and inherited handles.
- Reject reparse-point, hard-link, alternate-stream, and unexpected object-type changes without modifying originals.
- Detect concurrent create/replace/content changes between snapshot and copyback; verify conflict failure occurs before the first replacement.
- Test per-file atomic replacement, additions, deletions, partial commit failure reporting, and nonzero/timeout copyback semantics.
- Prove staging byte/entry budgets fail closed and do not fill the host volume without bound.
- Verify cleanup-deadline behavior: no copyback or success receipt while a descendant remains; retained stage is isolated until cleanup completes.
- Re-run the complete platform-validation matrix, including positive and negative filesystem controls, receipts, cleanup, Node compatibility, and exact CLI acceptance.

## Rollout and support gate

Keep Windows experimental and unsupported until the acceptance matrix passes on Windows 11 at an exact clean commit and independent review resolves the existing external-DACL race and cleanup lifecycle findings. Do not use Windows Server 2025 for acceptance. Until then, update documentation to describe the writable-filesystem gap and avoid support claims.
