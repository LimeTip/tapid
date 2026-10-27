# Structured dependency edits: public implementation

The measured prototype justified the public `tapid edit-dependencies --file PATH`
interface. The current implementation applies real edits and installs once. This
report records local engineering evidence, not acceptance of issue #194 or a
release approval. No merge, push, deployment or external message was performed.

## Implemented behavior and reuse

- File-only version-1 JSON input uses the existing bounded regular-file reader,
  including no-follow protection on Unix. It rejects unknown/duplicate fields,
  invalid UTF-8/JSON, invalid shapes/versions/sections, empty input, more than
  256 edits and more than 1,048,576 bytes. Both exact limits are tested.
- One selected manifest supports explicit runtime, development, optional and peer
  sections. The planner validates the entire request against the original
  manifest before installer acquisition/recovery. It reserves normalized targets,
  rejects overlapping edits, and constructs one final manifest in memory.
- Section-specific operations live in `tapid-manifest`, share existing name
  validation, preserve exact move requirements/aliases and other sections, and
  retain ordinary add/remove's distinct cross-section semantics.
- The public handler calls the same installer once with expected/updated manifests.
  Verified publication, project-change detection, journal rollback and durable
  settlement remain the existing operations. No transaction engine or resolution
  for reporting was added.
- Every decoded edit has an input index and transaction-authoritative disposition.
  Indexed validation errors identify affected edits, and conflict errors identify
  both positions. A malformed edit also identifies its decoding index.
- Graph evidence extends the installer's existing exact-lock comparison. It
  includes registry/copied packages, workspace records and root bindings. Exact
  lock identities retain source, peer and platform contexts. At most 256 graph
  records are presented, with explicit omitted counts and completeness; all edit
  records remain present. Shared scalar redaction/bounds apply.
- CLI help, the CLI README, JSON protocol and agent guide document the public
  interface, limits, retry behavior and the durable-commit boundary.

The pure preview comparison remains separate: it compares resolution snapshots
for an observation and includes unchanged records/reasons. Installation compares
actual persisted lock records; invoking preview would fetch/resolve again and
would not establish committed evidence. The existing install comparison was
extended instead.

## Safety and boundary evidence

CLI tests cover mixed edits, malformed/duplicate fields, an invalid final edit
with a pending journal, indexed malformed final edits, exact byte/edit limits,
nonregular/symlink files, and workspace moves preserving aliases and an unrelated
member. Invalid/ambiguous member selection leaves pending state untouched.

Existing batch tests exercise normalized/reordered conflicts, missing/occupied
moves, aliases, requirements, paths, unsupported workspace/copied declarations,
optional and peer semantics, expected-manifest mismatch, resolution/integrity/
staging rollback and every existing journal crash checkpoint. Pre-commit tests
compare prior manifest/lock bytes, managed package bytes and verified trees.
Post-commit/cleanup tests preserve the new state. Renderer tests independently
check unchanged, rolled-back, committed, cleanup-pending and recovery-required
outcomes, output truncation and exact omissions. Exact-lock tests distinguish
registry origins, peer/platform instances and root-binding-only changes.

A new successful workspace test initially omitted a required root peer provider.
The real installer rejected it and rolled back. The successful fixture now
supplies the provider; unsupported member-peer topology was not enabled or
silently ignored.

Before commit, handled failures restore prior project state through the existing
transaction. Recovery failure remains explicit. After commit, recovery completes
the new state, even if the command exits nonzero. New published objects follow
journal settlement; this does not promise whole-cache byte identity or rollback
of external script effects. Failure output explicitly marks graph evidence
unavailable instead of claiming a complete empty delta.

## Reproduction and automation model

Run the Rust commands through the absolute supplied Node runtime, with its
binary directory first in PATH and `CARGO_TARGET_DIR=target`:

```text
node --experimental-strip-types scripts/dev.ts test -p tapid --lib --locked dependency_edit_measurement_matrix -- --show-output
node --experimental-strip-types scripts/dev.ts test -p tapid --test cli --locked dependency_edits_
```

The final harness uses the same fixtures, initialized snapshots, cold/warm
conditions and minimum grouped controls as the [prototype report](dependency-edits.md).
Both arms now render JSON. The candidate timer includes JSON construction,
request-file creation, bounded reading/decoding, planning, installation and JSON
rendering. Setup/preseeding and final-state assertions are outside the timer.
Handlers run in-process; process startup and Clap parsing are excluded equally.
The test invokes real command handlers, local metadata and archive fixtures,
verified storage, activation and journal decisions; registry HTTP traffic is zero.

For the automation model, count each CLI invocation as one action and file
preparation as one separate candidate action. Mixed edits use two actions versus
three; a one-section batch uses two versus one; the workspace case uses two
versus two. Combining preparation and execution in one shell tool is possible,
but grouped ordinary commands can also share a shell call. Therefore a universal
external-agent tool-call reduction is not claimed. One candidate graph operation
and durable decision versus three in the mixed case remain the measured benefit.
No participant pilot evidence is invented or required for this local evaluation.

Timing is supporting evidence: five independent samples per arm/cache/scenario,
fixed control-before-candidate order, no HTTP, and local storage noise. It does
not establish a production latency percentage or superiority over ordinary
multi-add for one section.

## Public implementation measurements and validation

These measurements describe the public implementation before the subsequent
test-only crash-child isolation repair. That repair changes the crash test's
working directory and verifies isolation; it does not change the measured path
or production behavior. The recorded fingerprint is historical rather than the
fingerprint of the repaired snapshot. See the [validation recovery report](dependency-edits-validation-recovery.md)
for the repair and checks on that snapshot.

All 60 final samples passed exact declaration/lock-record/binding/layout comparisons. Rust source delta fingerprint: `013af0bf372a79a0427312da24a189417e79584eebfe11ae85e05b8763e4733e`, over base `13c875ee4b1e754d8f4e1d62061aefd194f816e4`. The first line of [raw public samples](dependency-edits-public-samples.jsonl) contains the exact path list, hash recipe, fixture digest, runtime and command. The prototype raw samples remain historical evidence for the earlier go decision; the new file records this implementation. Documentation/sample files are excluded from the Rust fingerprint to avoid self-reference.

| Case | Cache | Arm | Samples | Median µs | Range µs | Commands | Graph operations | Metadata | Artifacts | Stages | Activations | Durable decisions |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| single | cold | grouped | 5 | 216039 | 197663–237760 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| single | cold | batch | 5 | 229812 | 194444–240097 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| single | warm | grouped | 5 | 185929 | 177847–192551 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| single | warm | batch | 5 | 200188 | 189421–208884 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| mixed | cold | grouped | 5 | 594209 | 566915–722520 | 3 | 3 | 2 | 2 | 3 | 3 | 3 |
| mixed | cold | batch | 5 | 215885 | 195482–276856 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| mixed | warm | grouped | 5 | 709139 | 593447–852728 | 3 | 3 | 2 | 2 | 3 | 3 | 3 |
| mixed | warm | batch | 5 | 211294 | 186601–245277 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| workspace | cold | grouped | 5 | 565618 | 421244–2753392 | 2 | 2 | 2 | 2 | 2 | 2 | 2 |
| workspace | cold | batch | 5 | 550107 | 239401–1353255 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |
| workspace | warm | grouped | 5 | 1414697 | 703495–2549334 | 2 | 2 | 2 | 2 | 2 | 2 | 2 |
| workspace | warm | batch | 5 | 1240072 | 473233–1405496 | 1 | 1 | 2 | 2 | 1 | 1 | 1 |

The single-section batch has modest extra median cost in these samples and no graph-count advantage. Workspace timing varies widely. The go decision rests on mixed-case transaction/resolution reduction with correctness preserved, not a promised latency percentage. Metadata/artifact counts did not improve. The automation-action accounting above includes file preparation rather than equating one batch invocation with one universal agent tool call.

| Final local check | Actual evidence |
| --- | --- |
| `fmt --all --check` | Passed |
| `clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `test --workspace --all-features --locked --no-fail-fast` | CLI library 294 passed; CLI integration 275 passed, 5 native containment failures; JSON 31, npm import 20, release verification 1, upgrade 14 passed; run planning 11 passed, 2 native containment failures; runner 152 passed, 25 native containment failures. Other crate targets and doc tests passed, including manifest 38. |
| Agent workflows after initial guide correction | 6 passed locally at 5956 LF bytes. Later Windows CI exposed 6059 CRLF bytes; the [checkout-format repair](dependency-edits-validation-recovery.md#windows-checkout-guide-budget-repair) adds both-format coverage and shortens the guide to 5698 LF / 5797 CRLF bytes. No test limit was changed. |
| `test --manifest-path tests/integration/Cargo.toml --locked` | consumer_install 1 passed; contracts 4 passed |
| `tools/check_architecture.ts` | Passed, with existing source-size review recommendations |
| `node --experimental-strip-types --test tests/doc_examples_test.ts` | 46 passed; 5 native PowerShell cases skipped by their existing local conditions |
| Dedicated final measurement matrix | 1 test passed; 60 samples and all final-state comparisons passed |
| Documentation links/commands and `git diff --check` | Reviewed/passed |

Cargo checks used the supplied absolute Node runtime with its directory first in PATH, worktree-local `CARGO_TARGET_DIR=target`, and `scripts/dev.ts`. The final full-lane orchestration ran Cargo commands sequentially. Successful unchanged-input checks were reused; the guide-only correction was verified by rerunning its complete consumer test target. Logs are `/tmp/tapid-public-{format,clippy,workspace,integration,architecture,doc-tests,measurements,agent-workflows}.log`; the orchestration statuses are in `/tmp/tapid-public-validation.json`.

Source-size review kept the existing exact-lock comparator with installer reporting and kept fault cases with their shared transaction fixtures/guards. Strict file decoding, command composition, private batch planning and JSON rendering have separate cohesive modules. Ordinary add/remove and section-targeted edits remain separate where their cross-section contracts differ.

## Validation handoff and remaining gates

At this report's original snapshot, implementation was complete but the required local lane was **not green**. Native macOS failures reproduced `UnsupportedContainment`, `native probe profile installation failed`, and `no enforcement receipt was issued`. Runner/containment implementation, tests and advisory policy were not changed, suppressed or weakened. See the [recovery report](dependency-edits-validation-recovery.md) for subsequent repairs and validation; the original local guide fix alone did not cover Windows checkout bytes.

Use the engine's isolated Linux validation lane on this exact source snapshot, with Node/Rust and cached locked dependencies, to run:

`CARGO_TARGET_DIR=target node --experimental-strip-types scripts/dev.ts test --workspace --all-features --locked --no-fail-fast`

and the separate integration workspace command above. This requests isolated validation rather than an unsandboxed host fallback. No current source-matched isolated receipt is available. A Linux receipt cannot establish native macOS or Windows behavior; preserve their existing hosted checks and all existing CI security, compatibility, coverage and packaging gates. No additional provider gate, participant pilot, deployment or human product decision is required by this implementation.

The engineering handoff is blocked only on environment validation. No further independent feature coding remains. Do not mark the original feature accepted or release it based on local measurements alone.


## Changed files since the issue baseline

- [crates/tapid-cli/README.md](../../crates/tapid-cli/README.md)
- [crates/tapid-cli/src/application.rs](../../crates/tapid-cli/src/application.rs)
- [crates/tapid-cli/src/application/dependency_edit_evaluation.rs](../../crates/tapid-cli/src/application/dependency_edit_evaluation.rs)
- [crates/tapid-cli/src/application/install.rs](../../crates/tapid-cli/src/application/install.rs)
- [crates/tapid-cli/src/application/install_outcome_tests.rs](../../crates/tapid-cli/src/application/install_outcome_tests.rs)
- [crates/tapid-cli/src/application/lifecycle.rs](../../crates/tapid-cli/src/application/lifecycle.rs)
- [crates/tapid-cli/src/application/lifecycle/batch.rs](../../crates/tapid-cli/src/application/lifecycle/batch.rs)
- [crates/tapid-cli/src/application/outcome.rs](../../crates/tapid-cli/src/application/outcome.rs)
- [crates/tapid-cli/src/application/outcome/diagnostic.rs](../../crates/tapid-cli/src/application/outcome/diagnostic.rs)
- [crates/tapid-cli/src/commands/dependency_edits.rs](../../crates/tapid-cli/src/commands/dependency_edits.rs)
- [crates/tapid-cli/src/commands/lifecycle.rs](../../crates/tapid-cli/src/commands/lifecycle.rs)
- [crates/tapid-cli/src/commands/lifecycle_evaluation.rs](../../crates/tapid-cli/src/commands/lifecycle_evaluation.rs)
- [crates/tapid-cli/src/commands/mod.rs](../../crates/tapid-cli/src/commands/mod.rs)
- [crates/tapid-cli/src/online.rs](../../crates/tapid-cli/src/online.rs)
- [crates/tapid-cli/src/online/artifact.rs](../../crates/tapid-cli/src/online/artifact.rs)
- [crates/tapid-cli/src/online/graph.rs](../../crates/tapid-cli/src/online/graph.rs)
- [crates/tapid-cli/src/output/json.rs](../../crates/tapid-cli/src/output/json.rs)
- [crates/tapid-cli/src/output/json/dependency_edits.rs](../../crates/tapid-cli/src/output/json/dependency_edits.rs)
- [crates/tapid-cli/tests/cli.rs](../../crates/tapid-cli/tests/cli.rs)
- [crates/tapid-cli/tests/cli_cases/dependency_edits.rs](../../crates/tapid-cli/tests/cli_cases/dependency_edits.rs)
- [crates/tapid-manifest/README.md](../../crates/tapid-manifest/README.md)
- [crates/tapid-manifest/src/lib.rs](../../crates/tapid-manifest/src/lib.rs)
- [crates/tapid-manifest/src/model.rs](../../crates/tapid-manifest/src/model.rs)
- [docs/agent-guide.md](../../docs/agent-guide.md)
- [docs/evaluations/dependency-edits-public-samples.jsonl](../../docs/evaluations/dependency-edits-public-samples.jsonl)
- [docs/evaluations/dependency-edits-public.md](../../docs/evaluations/dependency-edits-public.md)
- [docs/evaluations/dependency-edits-samples.jsonl](../../docs/evaluations/dependency-edits-samples.jsonl)
- [docs/evaluations/dependency-edits.md](../../docs/evaluations/dependency-edits.md)
- [docs/json-results.md](../../docs/json-results.md)
