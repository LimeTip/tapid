# Dependency edit validation recovery

The supplied Linux validation log passed the integration tests and architecture
check, then rejected a new source-tree input, `crates/tapid-cli/core`. The new
batch crash-recovery test intentionally aborts a subprocess at five transaction
checkpoints. It inherited Cargo's crate working directory, allowing Linux core
dumps to appear in the checkout. No such file existed in this recovery workspace;
no user file was removed or ignored.

## Repair and regression evidence

Changed `crates/tapid-cli/src/application/install_outcome_tests.rs` to launch the
crash child in its existing `TempProject`. Core dumps using a relative output
path now belong to the disposable fixture. Added a canonical working-directory
assertion in the child. This does not alter abort behavior, checkpoint selection,
rollback, durable commit or recovery assertions. No production containment or
audit policy changed.

The focused `batch_recovers_existing_transaction_crash_points` test first failed
with the new assertion: its child was in `crates/tapid-cli` rather than the
temporary project. After adding `current_dir(project.path())`, it passed:
1 passed, 0 failed, 293 filtered out. All five checkpoints still execute:
store publication, lock replacement, managed-tree backup, activation, and durable
commit decision. Log: `/tmp/tapid-core-recovery.log`.

Inspected the complete issue diff from `13c875e`, including the public decoder,
planner, manifest operations, installer graph comparison, reporting, operation
counters, measurements, safety tests and documentation. The broader feature and
changed-file inventory remain in the [public implementation report](dependency-edits-public.md).
This recovery also clarifies that its benchmark fingerprint predates the
test-only repair. Both raw sample files retain their original evidence, with
metadata and 60 parseable sample records each; they were not rewritten.

## Validation

Commands use the supplied absolute Node 22 runtime, its directory first in PATH,
`CARGO_TARGET_DIR=target`, and `scripts/dev.ts`; Cargo runs sequentially.
| Check | Result on the repaired Rust snapshot |
| --- | --- |
| Focused crash regression | 1 passed, all five checkpoints |
| Workspace formatting | Passed |
| Workspace all-target/all-feature Clippy, warnings denied | Passed |
| Workspace all-feature tests, no-fail-fast | CLI library 294 passed; agent workflows 6 passed; architecture tests 6 passed; CLI integration 275 passed and 5 native containment failures; run planning 11 passed and 2 native containment failures; runner 152 passed and 25 native containment failures. Other targets and doc tests passed. |
| Separate integration workspace | 1 consumer-install and 4 contract tests passed |
| Architecture checker | Passed, existing file-size review recommendations retained |
| Diff whitespace and source status | Passed; no core dump appeared in the checkout |

Results are collected in `/tmp/tapid-core-validation.json` and the corresponding
`/tmp/tapid-core-*.log` files. The workspace exits 101 solely on the same three
native-containment targets. Diagnostics include `UnsupportedContainment`,
`native probe profile installation failed` and `no enforcement receipt was
issued`. These failures remain failures, with no skips or weakened assertions.

Linux isolated validation must confirm the repaired snapshot passes without
creating source inputs. Linux cannot establish native macOS/Windows support;
existing hosted checks remain required. No commit, push, merge, deployment,
external message or privileged service installation was performed.

Implementation is complete; local validation is blocked by the native sandbox.
Request engine-isolated Linux validation of the exact repaired snapshot using
the full lane in `docs/testing.md` (workspace formatting, workspace Clippy,
workspace all-feature tests, separate integration tests, architecture checker).
Do not use an unsandboxed host fallback. Native hosted evidence remains a later
delivery gate; the original feature is not declared accepted by this report.
