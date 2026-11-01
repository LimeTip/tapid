# Validation handoff after base sync

Read-only inspection on 2026-11-01 examined HEAD
`1230b80` against `origin/main`. The combined feature adds only the disposable
runner example, its private research fixtures, evidence and platform documentation.
Production execution interfaces and macOS strict rejection are unchanged by this
feature diff.

The separate-client-run evidence index still verifies all 42 retained artifact
hashes. Of its 562 recorded source files, 41 differ after the base sync, including
Cargo.lock, integration Cargo.lock and production consumers. None of the recorded
research example sources differ. Historical results remain development evidence;
they do not validate the combined checkout. No repository tests were executed on
the host in this handoff.

Docker now responds and reports Linux/aarch64. The exact configured image check:

```text
docker image inspect sha256:9f1c2c4244625133b85bef1c78f3f3cff7a4aea3a136c70cfe8e5877973b3b73 --format '{{json .}}'
```

returns exit 1, `No such image`. This supersedes the earlier missing-daemon
diagnostic. The engine must provision and verify its pinned worker image using
its prescribed `pnpm run setup`, then run the configured isolated validation
command against the exact source snapshot. This repository has no root
package.json and does not own that engine setup command. No replacement image,
host test fallback or validation receipt was created.

The isolated Linux lane cannot establish native macOS containment. Capable
macOS sandbox/XPC execution and macOS 26 matrix evidence remain required for
platform claims. Historical setup failures are unresolved, not native support
or impossibility verdicts. Independent QA and Acceptance remain outstanding.

`git diff --check origin/main...HEAD` reports whitespace in retained raw logs.
Those bytes were preserved so their recorded digests remain verifiable. This is
not a passing diff check; the reported paths are research evidence, not source.

Next: restore the pinned engine image, obtain current-source portable validation,
and run the documented native positive/adversarial matrix in the isolated macOS
test environment. Keep the strict gate and the distinction between diagnostic
observations and enforcement guarantees throughout.

## Compiler SIGKILL recovery

The subsequent engine-provided isolated Linux run reached Clippy compilation
and stopped while compiling `syn` 3.0.5: `rustc` exited on signal 9 (SIGKILL).
Formatting completed before that command; the test and architecture commands
after Clippy did not run. This is not a compiler diagnostic or a failing test
assertion. Memory exhaustion is a plausible cause, not an established OOM
diagnosis: the supplied log has no kernel OOM record.

Fresh read-only Docker inspection on 2026-11-02 reports 2,054,770,688 bytes
of daemon memory. The pinned image remains absent from this local daemon.
The engine's supplied run is evidence of its own worker execution; it does
not establish that this local daemon has the worker image. Do not repeat an
unmodified parallel build or run repository tests on the host.

Retry the complete configured lane in the engine's private worker with these
environment variables applied to the entire command:

```sh
export CARGO_BUILD_JOBS=1
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
node --experimental-strip-types scripts/dev.ts fmt --all --check &&
node --experimental-strip-types scripts/dev.ts clippy --workspace --all-targets --all-features --locked -- -D warnings &&
node --experimental-strip-types scripts/dev.ts test --workspace --all-features --locked &&
node --experimental-strip-types scripts/dev.ts test --manifest-path tests/integration/Cargo.toml --locked &&
node --experimental-strip-types tools/check_architecture.ts
```

The wrapper preserves these environment variables. Serial compilation lowers
peak concurrent compiler memory; disabling incremental output and debug symbols
reduces artifact overhead. These settings preserve debug assertions, test scope,
features, locked dependencies and warnings-as-errors. They are worker settings,
not a change to repository-wide developer build defaults or advisory policy.
If a single compiler is still killed, retain worker memory/cgroup and OOM
diagnostics and increase the isolated worker's memory allocation before retrying.
No successful retry or source-matched validation receipt is claimed here.
Native sandbox/XPC controls and macOS 26 evidence remain separate requirements.

### Second engine run: mitigation was not applied

The next supplied engine run again passed formatting and stopped in Clippy,
this time with both `libc` and `syn` compiler processes killed by SIGKILL.
The emitted `libc` command still contains `-C debuginfo=2`; the log also reports
waiting for other jobs after a compiler failure. Thus this is not evidence that
the reduced-memory command above failed: the configured command shown in the
handoff still lacks its environment prefix. No test assertion ran or identified
a source defect. OOM remains unconfirmed without worker diagnostics.

The engine must apply the four variables above to its actual isolated execution,
not only retain them in this document. Capture the effective values and worker
memory limit before compilation, and retain cgroup `memory.events` (or equivalent
worker OOM diagnostics) afterward. If SIGKILL persists with one job, increase the
isolated worker memory rather than changing containment tests or advisory policy.
Do not infer success from this proposed retry.

Current read-only inspection still finds the configured image absent locally;
an available differently pinned image is not a permitted substitute. No host
tests, replacement worker, engine configuration mutation or successful validation
receipt were created. The missing-image and compiler-kill observations belong
to different execution environments and must not be conflated.
