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

## Windows checkout guide-budget repair

Starting revision: `e13504b9f7be31eb631d4b563877e0395f0d0f3c`.
The [Windows job](https://github.com/LimeTip/tapid/actions/runs/40426054572/job/121931979728)
failed `agent_guide_examples_complete_with_bounded_calls_and_preserved_npm_lock`
on the raw embedded guide's strict `< 6000` byte assertion. The guide occupied
5,956 bytes with LF and 6,059 with CRLF (103 newlines). This is distinct from
the earlier local guide correction and from native containment failures.

The existing workflow now checks both LF and CRLF representations on every
platform, retaining the raw embedded-guide assertion and the unchanged budget.
With only the regression added, the focused test failed as expected: 0 passed,
1 failed, 5 filtered out, reporting 6,059 CRLF bytes. After shortening prose,
the guide occupies 5,698 LF bytes or 5,797 CRLF bytes, leaving over 200 bytes of
headroom in either format. Commands, version/schema identity, JSON/retry rules,
integrity and containment instructions, batch limits, and contract links remain.
No checkout policy, production code, dependency, or containment policy changed.

On the repaired source, the focused workflow passed (1 passed, 5 filtered out),
and the complete `agent_workflows` target passed all 6 tests. Logs:
`/tmp/tapid-guide-red.log`, `/tmp/tapid-guide-green.log`, and
`/tmp/tapid-guide-workflows.log`. Commands used the supplied absolute Node
runtime, its directory first in PATH, `CARGO_TARGET_DIR=target`, and `scripts/dev.ts`:

```text
test -p tapid --test agent_workflows --locked agent_guide_examples_complete_with_bounded_calls_and_preserved_npm_lock -- --show-output
test -p tapid --test agent_workflows --locked -- --show-output
```

The performance samples retain their original source fingerprints; the measured
production path is unchanged, so this prose/test repair does not repeat them.
Earlier isolated receipts do not validate this repaired source. Repaired-source
Windows CI must rerun the guide workflow and previously skipped targets before
acceptance; no complete Windows coverage or feature acceptance is claimed here.

### Repaired-source local lane

| Check | Actual result |
| --- | --- |
| `fmt --all --check` | Passed |
| `clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `test --workspace --all-features --locked --no-fail-fast` | Exit 101: CLI library 294 passed; agent workflows 6 passed; CLI integration 275 passed / 5 native containment failures; run planning 11 passed / 2 native containment failures; runner 152 passed / 25 native containment failures. Other targets and doc tests passed. |
| `test --manifest-path tests/integration/Cargo.toml --locked` | Passed: 1 consumer-install and 4 contract tests |
| `tools/check_architecture.ts` | Passed; existing source-size recommendations retained |
| Documentation review | Local Markdown targets exist; command examples unchanged; linked request/result section and CLI commands section retained |

Logs are `/tmp/tapid-guide-{fmt,clippy,workspace,integration,architecture}.log`. The 32 native
failures reproduce `UnsupportedContainment`, `native probe profile installation
failed`, and missing enforcement receipts. They were not skipped or weakened.
No further implementation remains in this repair, but the required full lane
is not green in this sandbox. Request engine-isolated Linux validation of the
exact repaired snapshot with the full `docs/testing.md` lane. Native macOS and
Windows checks remain hosted requirements; Linux cannot establish them.

Changed files for this repair:

- `crates/tapid-cli/tests/agent_workflows.rs`: both-format compactness regression
  in the existing executable-guide workflow; no new helper or dependency.
- `docs/agent-guide.md`: shorter prose preserving the batch and safety contracts.
- `docs/evaluations/dependency-edits-public.md`: qualify the earlier guide claim.
- `docs/evaluations/dependency-edits-validation-recovery.md`: red/green evidence
  and remaining validation limits.

The complete pending diff was reviewed. No commit, push, merge, deployment,
external message, or privileged service installation was performed.

## Current-main integration and exclusion reporting

Starting revision: `8ab38b097e58af8f846ee967a0ea87ab87bb2ec2`, an engine-owned
merge of main `0542aa7742f74597a1d48005083c3ef9189a6847` into the managed
feature branch. No conflicts or unmerged files remained. Inspection covered the
pending feature diff, including decoding, complete planning, section operations,
installer graph comparison, transaction tests, instrumentation and documentation.
Main's typed candidate-exclusion evidence and the batch's graph evidence both
survive installation. No resolver or transaction implementation was added.

The batch renderer did omit main's successful-operation exclusion evidence.
A public CLI regression with 12 excluded historical candidates first failed:
1 failed, 0 passed, because `data.candidate_exclusions` was absent despite a
committed batch. The renderer now shares ordinary dependency output's existing
serialization and truncation-pointer operation. The regression checks eight
retained candidates, four omissions, bounded version text, a valid full JSON
pointer, credential redaction, committed edit disposition and complete graph
reporting. The ordinary mixed-case test checks omission of empty evidence.
The renderer test constructor supplies main's new report field.

Changed files in this repair: `src/output/json/dependency.rs`,
`src/output/json/dependency_edits.rs`, and `tests/cli_cases/dependency_edits.rs`
under `crates/tapid-cli`, plus `docs/json-results.md` and this report.
The JSON contract explicitly includes structured edits among resolving operations.
No terminal-error behavior, input limit, containment or advisory policy changed.

Live refresh of [PR #360](https://github.com/LimeTip/tapid/pull/360) found no
review threads. The [approved review](https://github.com/LimeTip/tapid/pull/360#pullrequestreview-5661436273)
contains no actionable defect. Published successful checks still identify old
HEAD `cd32ef2bf1751475522ff3bb2c74d4b74784d72b`; they do not validate this
integrated snapshot. No commit, push, merge or publication was performed here.

The retained benchmark sample files and fingerprints remain historical. The
integrated-source measurement test is a fresh final-state/operation-count check,
not a replacement latency dataset or a claim of universal tool-call savings.

### Integrated-source validation

All commands used the supplied absolute Node 22 runtime with its directory first
in `PATH`, `CARGO_TARGET_DIR=target`, and sequential `scripts/dev.ts` invocations.

| Check | Actual result |
| --- | --- |
| New public exclusion regression before implementation | 1 failed as expected: missing success evidence (`/tmp/tapid-exclusions-red.log`) |
| Public CLI `dependency_edits_` after implementation | 8 passed (`/tmp/tapid-exclusions-green.log`) |
| Library `batch_` | 11 passed, including five crash checkpoints |
| Library `dependency_edit` | 8 passed, including the 60-sample final-state/operation-count matrix |
| JSON results `candidate_exclusions` | 2 passed, preserving ordinary success/replay and terminal-error behavior |
| Agent workflows | 6 passed |
| Workspace formatting | Passed |
| Workspace all-target/all-feature Clippy, warnings denied | Passed |
| Workspace all-feature tests with `--no-fail-fast` | Exit 101, only the three native containment targets failed; CLI library 300 passed, CLI integration 276 passed / 5 failed, run planning 11 passed / 2 failed, runner 152 passed / 25 failed. |
| Separate integration workspace | Passed: 1 consumer-install and 4 contract tests |
| Architecture | Passed: 191 production Rust files scanned; existing size recommendations retained |
| Diff whitespace and documentation review | Passed; existing protocol links and commands retained |

Focused logs are `/tmp/tapid-integrated-{batch_,dependency_edit,exclusions,agents}.log`.
Full-lane logs are `/tmp/tapid-round2-{fmt,clippy,workspace,integration,architecture}.log`.
The native failures report `UnsupportedContainment`, native probe profile
installation failure and unavailable enforcement receipts. No assertions were
skipped or weakened. No product failure was identified by these diagnostics.

Coding is complete. Request engine-isolated validation of the exact final source
using the full lane in `docs/testing.md`, with writable execution-enabled scratch
and `TMPDIR` configured as documented. Linux validation cannot establish native
macOS or Windows behavior; retain the existing exact-candidate hosted gates.
Earlier receipts and published old-head workflows do not validate this source.
