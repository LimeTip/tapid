# Dependency edit batch evaluation (issue #194)

Decision: **go to the conditional public implementation**. Real mixed edits reduced command/planner entries, top-level resolutions, materializations, activations, and durable decisions from three to one. The workspace relocation case reduced them from two to one. The one-section control retained one operation. This report records the private prototype snapshot at `c6d696c`; its measurements and validation are historical, not a claim about the current source. Public implementation and source-matched measurements are documented in [the public implementation report](dependency-edits-public.md).

Base revision: `13c875ee4b1e754d8f4e1d62061aefd194f816e4`. Source delta SHA-256: `07a9c3e20a809aa68992e88cd374a130ae326f45792284137c5062c0abf3f260`. The first line of [the raw samples](dependency-edits-samples.jsonl) records the fingerprint recipe and every changed/new Rust source path; the base supplies unchanged source. Documentation and sample output are excluded to avoid a self-referential fingerprint. Fixture: [npm-aliases.json](../../crates/tapid-cli/tests/fixtures/npm-aliases.json), SHA-256 `fb5cdb13a84f1c77f5cd00637b8a89636aa2b33c4604e9970c06ee95ba50d016`. Runtime: rustc 1.99.0 (b940084d7 2026-09-28) (Homebrew); Node v22.23.3; darwin/arm64.

The candidate uses typed private edits in `application/lifecycle/batch.rs`, plans against the original selected manifest, then invokes the existing installer once with its expected/updated manifest pair. It uses existing verified-store publication and the lifecycle journal. The planner and measurement observations are compiled only for tests. The two section-specific manifest operations are available in the owning capability crate; ordinary add/remove behavior is unchanged. No dependency, source protocol, registry credential mode, or transaction engine was added.

## Reproduction and controls

From the repository root, prefix PATH with the supplied Node directory, set `CARGO_TARGET_DIR=target`, and run:

`/Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin/node --experimental-strip-types scripts/dev.ts test -p tapid --lib --locked dependency_edit_measurement_matrix -- --nocapture --test-threads=1`

The harness lives in `crates/tapid-cli/src/commands/lifecycle_evaluation.rs`. It invokes the actual ordinary add/remove command functions in-process, including their normal planner/install composition. The candidate calls the private batch planner/installer entry. There is no CLI parsing or process-launch timing in either arm. Controls emit ordinary human progress/output; the private candidate has no public presentation layer. Timings therefore describe this local prototype, not end-to-end agent performance.

Each arm uses a fresh TempProject and TempHome with an explicit isolated store and deterministic local metadata/artifacts. Cold means the initialized project is installed with its original dependencies but no newly requested artifacts are preseeded. Warm uses that same initial project/lock/layout with final artifacts preseeded through a separate temporary project sharing only that arm's store. Initialization and preseeding are outside the timer. Every cell has five independent samples. Arms run control then candidate; order is not randomized. The measured work is synchronous and no other Cargo command runs alongside the dedicated sample run.

Equivalent ordinary invocations (each also supplies its temporary project/store/registry fixture options):

| Case | Ordinary control | Private candidate |
| --- | --- | --- |
| single | `add h3@2.0.0 @actual/scoped@1.2.0` | Two adds in dependencies |
| mixed | `add h3@2.0.0`; `add --dev @actual/scoped@1.2.0`; `remove old` | Those three explicit edits together |
| workspace | `add --workspace member --dev old@npm:h3@1.0.0 @actual/scoped@1.2.0`; `add --workspace member h3@2.0.0` | Move old to devDependencies plus both additions |

The workspace control uses ordinary add's existing relocation behavior, not an artificial remove/add pair. A second unselected member keeps its original alias declaration and installed availability. Final-state comparisons assert exact declaration values, exact package records, root bindings, workspace lock records, and visible package manifest bytes, accounting for root hoisting. Expected packages must be present, removals absent, and the unselected member remains available. No graph difference was observed. Lockfile byte equality is not the comparator.

There is one real harness invocation for the matrix. Command entries and private batch entries below are observed at their actual entry points, not inferred from a command list. No external agent participated. In an automation model that needs one file-writing tool call to prepare a future JSON request, mixed edits would require two actions (prepare + invoke) versus three grouped CLI calls; the workspace case would have two versus two, and the single-section case two versus one. Those tool-call totals are a disclosed model, not measured external participant evidence. JSON file preparation, strict decoding, CLI parsing, and public result rendering are not timed in this typed prototype and must be reevaluated with the public interface.

## Measurements

Time is median and min–max in milliseconds; counts are identical across all five samples in each cell. Entries are ordinary-command/private-batch entries. Materialization/activation counts are actual managed-root stage/activation calls, not package counts. Durable decisions count successful journal commit decisions in the resolving install branch. Metadata counts are cache misses at the existing metadata retrieval seam, not solver iterations. Artifact requests count fixture byte retrieval after the pinned verified-tree fast path. All metadata and archive access uses local fixtures: **zero registry HTTP requests**. Neither arm reduced metadata or artifact retrieval in these fixtures.

| Case | Cache | Arm | Median ms | Range ms | Entries command/batch | Graph operations | Metadata | Artifacts | Materializations/activations | Durable decisions |
| --- | --- | --- | ---: | --- | --- | ---: | ---: | ---: | --- | ---: |
| single | cold | control | 1008.2 | 205.0–1221.8 | 1/0 | 1 | 2 | 2 | 1/1 | 1 |
| single | cold | candidate | 664.2 | 235.6–1081.8 | 0/1 | 1 | 2 | 2 | 1/1 | 1 |
| single | warm | control | 371.5 | 296.2–1192.0 | 1/0 | 1 | 2 | 2 | 1/1 | 1 |
| single | warm | candidate | 549.6 | 184.8–1055.7 | 0/1 | 1 | 2 | 2 | 1/1 | 1 |
| mixed | cold | control | 3140.1 | 1543.1–4237.0 | 3/0 | 3 | 2 | 2 | 3/3 | 3 |
| mixed | cold | candidate | 996.2 | 588.7–1396.4 | 0/1 | 1 | 2 | 2 | 1/1 | 1 |
| mixed | warm | control | 1334.4 | 599.5–4126.4 | 3/0 | 3 | 2 | 2 | 3/3 | 3 |
| mixed | warm | candidate | 242.9 | 185.2–1178.4 | 0/1 | 1 | 2 | 2 | 1/1 | 1 |
| workspace | cold | control | 459.4 | 413.8–463.8 | 2/0 | 2 | 2 | 2 | 2/2 | 2 |
| workspace | cold | candidate | 269.1 | 259.0–300.2 | 0/1 | 1 | 2 | 2 | 1/1 | 1 |
| workspace | warm | control | 443.0 | 412.0–453.9 | 2/0 | 2 | 2 | 2 | 2/2 | 2 |
| workspace | warm | candidate | 238.9 | 222.8–271.5 | 0/1 | 1 | 2 | 2 | 1/1 | 1 |

Single-section overhead must be judged against the variability and the absent public parser/output work; these samples do not establish a speed percentage or an overhead bound. The operational reduction in the mixed case justifies proceeding, with the final public implementation measured again before any end-to-end performance claim.

## Safety evidence and remaining implementation

Tests cover section-preserving insertion/update/removal in all four sections; ordinary semantics; exact move requirement bytes and alias keys; mixed/overlapping sections; duplicate normalized targets and conflicts in either order; occupied moves; missing declarations; invalid names, aliases, ranges, workspace protocols and copied paths; optional installation and peer-only noninstallation; member selectors and copied-member rejection; empty/over-limit typed batches; invalid-last-edit rejection while a pending journal remains untouched; and installer project-change detection.

A regression test demonstrated that copied peer declarations could reach recovery before rejection. The private planner now rejects them before installation. Another red test caught requirement whitespace being lost during a move; targeted insertion now preserves the supplied value verbatim while ordinary add retains its existing behavior.

Real private batch installs exercise handled resolution, integrity and injected staging failures, checking prior manifest/lock bytes, installed package bytes and pre-existing verified-store validation. Reexecuting the private batch test in an isolated child exercises store_published, lockfile_replaced, node_modules_backed_up, activation_complete and commit_decision. Before the durable decision, recovery restores prior bytes/layout and removes newly published tree objects while preserving the prior tree set. After the decision, recovery retains the combined manifest, new lock/layout and published trees. Post-commit failure and cleanup tests preserve committed/committed-cleanup-pending outcomes. This does not promise rollback after durable commit, cache-byte identity in every failure mode, simultaneous visibility to unrelated readers, or rollback of external script effects.

At the prototype snapshot, the full public work was still pending: strict bounded version-1 JSON file decoding (including duplicate keys and 1 MiB/256-edit boundaries), command registration/help, complete indexed diagnostics and edit dispositions, bounded graph changes from the actual before/after install locks without another resolution, public JSON/agent documentation, public input/output boundary tests, and final measurements including file preparation/decoding/reporting. Typed planner validation is not a substitute for those protocol requirements. Native hosted platform and existing CI/security gates remain later delivery checks; no deployment is needed for this merge-only work item.

Local validation results are recorded in the implementation handoff. No merge, commit, push, deployment or external message was performed.

## Local validation on the recorded source

All Cargo invocations used the absolute supplied Node runtime, its directory first in PATH, `CARGO_TARGET_DIR=target`, and `scripts/dev.ts`. Cargo commands ran sequentially.

| Check | Actual result |
| --- | --- |
| `fmt --all --check` | Passed |
| `clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `tools/check_architecture.ts` | Passed; existing source-size review recommendations remain |
| `test --workspace --all-features --locked -- --show-output` | CLI library 289 passed; agent workflows 6 passed; architecture 6 passed; CLI integration 268 passed, 5 native macOS containment tests failed; Cargo stopped at that target |
| `test --workspace --exclude tapid --all-features --locked --no-fail-fast` | All targets except tapid-runner passed, including all 38 manifest tests; runner library 152 passed, 25 native macOS tests failed |
| `test --manifest-path tests/integration/Cargo.toml --locked` | Passed: consumer_install 1 test and contracts 4 tests |
| `test -p tapid --test json_results --test npm_import --test release_verification --test run_planning --test upgrade --locked --no-fail-fast` | JSON results 31 passed; npm import 20 passed; release verification 1 passed; upgrade 14 passed; run planning 11 passed and 2 native macOS containment tests failed |
| Dedicated `dependency_edit_measurement_matrix` command above | Passed, 60 real samples with final-state comparisons |
| `git diff --check` | Passed |

Native failure diagnostics include `UnsupportedContainment`, `native probe profile installation failed`, and `no enforcement receipt was issued`; native helper tests also report child failures when the backend cannot establish support. These failures are recorded, not suppressed or changed into skips. No runner/containment implementation or advisory policy was changed. The full local lane is therefore **not green**. An engine-isolated Linux run can establish Linux behavior after the remaining implementation; native macOS and Windows evidence still belongs to the existing hosted gates. No unsandboxed host fallback was requested.

The production-source additions are two targeted manifest operations and shared name-validation reuse. The private planner and all measurement/fault seams are test-only. New batch outcome cases stay with the existing transaction fixtures to reuse verified archives, fault guards, and settlement assertions; planning and measurement responsibilities are separate private modules. Ordinary add/remove and installer transaction contracts stay separate from targeted section edits because their cross-section semantics differ.

At that snapshot, implementation remained partial because the public decoding, command, indexed output and graph-reporting slices are executable follow-up work. Native validation limits do not block that independent coding. No participant pilot evidence is claimed or required by this local evaluation, and no human decision is pending.

## Changed files

- `crates/tapid-cli/src/application.rs`
- `crates/tapid-cli/src/application/dependency_edit_evaluation.rs`
- `crates/tapid-cli/src/application/install.rs`
- `crates/tapid-cli/src/application/install_outcome_tests.rs`
- `crates/tapid-cli/src/application/lifecycle.rs`
- `crates/tapid-cli/src/application/lifecycle/batch.rs`
- `crates/tapid-cli/src/commands/lifecycle.rs`
- `crates/tapid-cli/src/commands/lifecycle_evaluation.rs`
- `crates/tapid-cli/src/online.rs`
- `crates/tapid-cli/src/online/artifact.rs`
- `crates/tapid-cli/src/online/graph.rs`
- `crates/tapid-manifest/src/lib.rs`
- `crates/tapid-manifest/src/model.rs`
- `crates/tapid-manifest/README.md`
- `docs/evaluations/dependency-edits.md`
- `docs/evaluations/dependency-edits-samples.jsonl`
