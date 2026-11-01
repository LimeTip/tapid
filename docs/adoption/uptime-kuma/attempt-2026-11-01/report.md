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

## Follow-up order and evidence limits

1. Obtain a disposable resource envelope that completes the npm baseline. Retain
   before/after resource counters and npm logs; do not skip hooks to manufacture a
   successful baseline. This affected baseline and recovery, so it currently prevents
   meaningful application comparisons.
2. Reduce import rejection to a local Rust fixture and assess published dependency
   workspace metadata under [#230](https://github.com/LimeTip/tapid/issues/230).
   It appeared in three import attempts, all in this single project; these are not
   three independent projects. Do not silently discard workspace semantics.
3. Reduce the registry metadata rejection to a local Rust fixture and determine
   whether unrelated historical versions poison resolution. Native install and add
   hit it twice. Fail-closed package-name validation must remain intact. Owning defect
   classification is pending reduction; [#307](https://github.com/LimeTip/tapid/issues/307)
   retains this unresolved finding rather than claiming a resolver fix.
4. Recover the interrupted update through Tapid's existing transaction recovery
   path before attempting restoration. Preserve the journal until recovery succeeds.
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
