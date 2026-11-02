# Uptime Kuma: unsuccessful first attempt

The [45-phase receipt](receipts.json) captures the entire first specification run,
started on 2026-11-01 UTC. It supersedes reports based on the earlier 28-phase
incremental receipt. One project was attempted; zero builds, application test/start
checks, add/update cycles or npm rollback journeys succeeded. This is evaluation
evidence for [#307](https://github.com/LimeTip/tapid/issues/307) and input to
[#158](https://github.com/LimeTip/tapid/issues/158), not release qualification.

## Inputs and execution boundary

The public MIT source, revision `5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7`,
archive and retained manifest/lock/license hashes are in the [pinned plan](../plan.json).
Source acquisition and hook identification have separate [qualification receipts](../qualification/receipts.json).
The original input hashes passed before application commands were launched.

This attempt used unreleased Tapid commit
`e445eb2a3f9031745b0e34ad91a40973c5b430e6`, reporting version `0.0.13`,
with binary SHA-256 `58e02c16d0eb7ca52226a28d13b120d9155956e1183f2b559aa1757ed810a2b4`.
Tooling was Linux arm64, Node `22.23.3`, npm `10.9.9`; the npm entrypoint digest
is retained in the receipt. These identities do not certify toolchain suitability:
npm reported a transitive package engine mismatch for this Node version.

The disposable container had a read-only root, read-only retained source volume,
private bridge networking without published ports, isolated homes, no host credential
mounts, all capabilities dropped and no-new-privileges. It used a writable temporary
run tree, a 3 GiB memory limit and two CPUs. This establishes the evaluation boundary,
not Tapid ManagedTree lifecycle qualification. No lifecycle authority was inferred.
Machine-specific runtime configuration is retained outside the repository.

## Outcomes

| Operation | Observation | Interpretation |
| --- | --- | --- |
| npm baseline | `npm ci --no-fund` received SIGKILL | Baseline installation failed; application checks not reached |
| Local status | `LOCKFILE_MISSING`, absent installation | Correct diagnosis before import, not an application failure |
| SQLite inspection | Registry identity/integrity claims displayed; artifact bytes not checked in this command | Prototype inspection only; no hook grant or safety verdict |
| Import | `LOCKFILE_INVALID`: `redis@4.5.1` has unsupported `workspaces` field | Migration blocked before replay |
| Native install | `REGISTRY_METADATA_INVALID`: dependency name `../serve-static` in `@types/express` metadata | Resolution blocked; project files reported unchanged |
| Frozen/offline/graph comparison | Prerequisites failed | Not reached; no selected graph or parity result |
| Add `is-odd@3.0.1` | Same registry metadata rejection | No successful maintenance cycle |
| Add restoration | Original three retained input hashes restored | Input restoration passed; subsequent npm installation received SIGKILL, so rollback journey failed |
| Update `dayjs` | Exact declaration preparation passed; mutation received SIGKILL | No successful update; preparation is a manual intervention |
| Update restoration | Refused pending lifecycle journal | Correctly stopped without erasing recovery evidence; npm rollback not reached |
| Approved lifecycle | Explicitly unsupported | No qualified Linux host or reviewed exact authority policy supplied |

There were 17 runner launches. Failed prerequisites suppressed dependent phases;
application build/test/start commands were not launched. The receipt does not count
internal subprocesses, exploratory calls or human assistance. Participant completion,
help requests, incorrect grants, comprehension and stopping reasons are unmeasured;
no participant contact occurred.

## Resource diagnosis and bounded retry

On recovery, the retained container's cgroup `memory.events` recorded three
`oom_kill` events. Three receipt phases received SIGKILL, but the counter has no
per-process attribution or timestamps; it does not prove the cause of each signal.
The missing final receipt was recovered without restarting or deleting the container.

A subsequent diagnostic retry ran the same npm baseline in the isolated npm
checkpoint with `npm_config_maxsockets=2` and
`NODE_OPTIONS=--max-old-space-size=384`. It preserved lifecycle scripts and the
original manifest/lock, used explicit isolated npm configuration, and inherited no
host environment. It exited 137 with `Killed`; `oom_kill` increased from three to
four. Lower connection concurrency and a bounded JS heap did not resolve the resource
failure. This retry is separate from the immutable first-attempt receipt. No build or
rollback success is inferred from its partially populated tree.

Helper verification after recovery ran inside the same disposable container:

```text
node --experimental-strip-types --test tests/evaluate_adoption_test.ts tests/news_site_fixture_test.ts
```

Result: 22 passed, zero failed, one native-binary probe skipped. The runner, pilot
specification, comparator and both test files matched the checkout's SHA-256 hashes.
These helper tests verify capture/replay-fixture behavior, not the external application.
No repository tests were executed directly on the host.

The subsequent offline Rust reductions use the existing importer interface and
registry transport fake, with metadata held by `tapid-test-support::FakeRegistry`.
They need no filesystem projects or network. In the pinned Linux arm64 image,
the affected-crate lane passed 174 tests, including both named reductions, plus
Clippy and workspace formatting:

```text
node --experimental-strip-types scripts/dev.ts test -p tapid-lockfile -p tapid-registry-client --all-features --locked
node --experimental-strip-types scripts/dev.ts clippy -p tapid-lockfile -p tapid-registry-client --all-targets --all-features --locked -- -D warnings
node --experimental-strip-types scripts/dev.ts fmt --all --check
```

Validation used one Cargo job, `CARGO_INCREMENTAL=0`, and
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` after default-debug
compilation received SIGKILL. All 572 tracked source files matched SHA-256
checksums in the validation copy before the final test run. A redundant test
link overlapping a networked pilot retry also received SIGKILL; the final
sequential test run above passed. This is local container evidence, not an
engine validation receipt or native macOS/Windows validation.

## Follow-up order and evidence limits

1. Complete the npm build/start baseline in a larger disposable memory envelope.
   The [subsequent resource retries](../resource-retries-2026-11-02/report.md)
   completed npm installation and the limited backend suite, but builds still
   exhausted V8 or VM memory. Preserve resource counters and lifecycle scripts.
2. Assess published dependency workspace metadata under
   [#230](https://github.com/LimeTip/tapid/issues/230). The offline Rust reduction
   `adoption_published_redis_workspace_metadata_is_not_silently_discarded` in
   [lockfile tests](../../../../crates/tapid-lockfile/src/npm_import/tests.rs)
   reproduces the exact pointer, package and unsupported-field diagnostic, with
   a control that removes the workspace declaration from the reduced input.
   It appeared in three import attempts, all in this single project; these are not
   three independent projects. Do not silently discard workspace semantics.
3. Assess whole-packument rejection of malformed historical versions. The offline
   Rust reduction `adoption_historical_express_dependency_name_fails_closed` in
   [registry tests](../../../../crates/tapid-registry-client/src/npm.rs) places
   `../serve-static` in a synthetic historical record beside a valid candidate:
   the complete fetch rejects the invalid name. Correcting that name allows both
   records to parse. Synthetic versions and artifact data are parser controls,
   not a preserved registry snapshot or a successful application evaluation.
   Native install and add hit this diagnostic twice. Fail-closed package-name
   validation remains intact. Owning defect
   classification remains open; [#307](https://github.com/LimeTip/tapid/issues/307)
   retains this unresolved finding rather than claiming a resolver fix.
4. Complete npm rollback installation and application checks after the
   [subsequently verified transaction recovery and input restoration](../resource-retries-2026-11-02/report.md).
   Recovery after a failed mutation does not establish rollback after a successful update.
5. Qualify lifecycle execution under [#155](https://github.com/LimeTip/tapid/issues/155),
   then repeat application and maintenance checks. Peers, selected versions/origins,
   platform choices and semantic graph parity remain unmeasured, not successful.

These frequencies describe one attempted application and cannot rank population-wide
adoption blockers. Four further projects, native macOS/Windows evidence and actual
participant evidence remain outstanding. Select the expanded corpus after resolving
or explicitly bounding this first result. Reproduce commands with the
[existing specification](../../../../scripts/evaluate-uptime-kuma.ts) inside qualified
disposable isolation; command arguments and bounded outputs are in the receipt.
Do not rerun against a checkpoint with a pending transaction as a fresh install.
