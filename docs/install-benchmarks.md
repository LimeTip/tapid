# Install benchmark development status

The command slice in `scripts/benchmark-installs.ts` is development infrastructure
for [issue #308](https://github.com/LimeTip/tapid/issues/308). It does not yet provide
the qualified npm/pnpm/Tapid benchmark matrix or published performance evidence.
Do not use its observations to make comparative speed claims.

## Current command interface

Run with a supported Node runtime and `--experimental-strip-types`:

```text
node --experimental-strip-types scripts/benchmark-installs.ts preflight SPEC.json
node --experimental-strip-types scripts/benchmark-installs.ts run SPEC.json /absolute/new-run-directory
node --experimental-strip-types scripts/benchmark-installs.ts summarize /absolute/existing-run-directory
```

`preflight` checks schema version 1, repetition/timeout bounds, tool entry-point
and payload SHA-256 digests, and selected fixture file digests. Tool paths and the
fixture source must be absolute. Fixture paths are relative, unique files without
symlinks or traversal components. Pin JavaScript payloads as well as the executable;
entry-point bytes alone do not identify an interpreter's application payload.

The current specification has this shape (paths and hashes below are placeholders):

```json
{
  "schemaVersion": 1,
  "tool": {
    "executable": "/absolute/node",
    "sha256": "executable-sha256",
    "args": ["/absolute/fake-tool.cjs"],
    "payloads": [{ "path": "/absolute/fake-tool.cjs", "sha256": "payload-sha256" }]
  },
  "fixture": {
    "source": "/absolute/source",
    "files": [{ "path": "package.json", "sha256": "manifest-sha256" }]
  },
  "repetitions": 5,
  "timeoutMs": 60000
}
```

An optional nonempty `unsupportedReason` records unsupported attempts without
executing the command. This is a declared capability limitation, never a way to
relabel an observed command failure.

The run directory must not exist. Every repetition gets fresh project, home,
configuration, temporary, and cache directories. Children receive an explicit
minimal environment, with no inherited credentials, PATH, proxy settings, or
package-manager configuration. This is environment isolation, not filesystem
containment: run trusted tools and fixture checks only. There is no cleanup of
existing directories and no automatic resumption yet.

The command persists the specification/runtime manifest and atomic per-attempt
JSON records. Failures retain exit code, signal, bounded stdout/stderr, and timeout
diagnostics. Each stream retains its first 64 KiB of input bytes; truncation and
aggregate output byte counts are recorded. Decoding split UTF-8 can insert a
replacement character. Logs should still be reviewed before publication.

Wall time uses the parent's monotonic clock immediately before spawn until the
child's `close` event, including output handling. Fixture copying and disk scans
are outside this interval. Raw nanoseconds are decimal strings. A nonzero command
exit makes the invocation fail after retaining observations. Timeout attempts stop
the schedule. POSIX process-group termination bounds ordinary descendants; it
cannot establish termination of detached descendants. Pipe closure only bounds
observation. Qualified host process-tree ownership remains required before real
reference measurements.

`summarize` reads persisted attempts without invoking tools. It reports successful
samples in milliseconds, count, median, minimum, maximum, and IQR. Quartiles are
medians of the lower and upper halves, excluding the overall median for odd sample
counts. IQR is unavailable for fewer than two successful samples. Failed,
unsupported, and missing attempts are counted separately and never enter timing
statistics. The summary is replaceable; existing raw runs are never overwritten.

## Accounting and evidence limits

Project, cache, and combined disk inventories use `lstat`, do not follow symlinks,
and deduplicate regular files by device/inode within each inventory. Logical
bytes are regular-file lengths. Allocated bytes use `stat.blocks * 512` where
available, otherwise null. Directory metadata and symlink payload bytes are
excluded. Project and cache totals may overlap through hard links: use the combined
inventory rather than adding them. These inventories do not measure reclaimable
space, compression, copy-on-write sharing, or OS cache residency. They are distinct
from `tapid cache info` and its published-artifact/tree exclusions.

Peak memory and cache/download counts are explicitly unavailable. Store growth,
lock reuse, and logical byte differences are not cache-hit/download counts. Graph
and application eligibility remain unverified for every command observation.

Remaining work includes host-qualified supervision and offline denial, interruption
and matching-identity resumption, concrete tool/scenario adapters, lock checks,
application validation, instance-aware graph comparison, two licensed immutable
application snapshots, exact tool qualification, and the Linux reference matrix.
Native versus imported locks, hook approvals/execution, containment,
authenticated-output preparation, and toolchain invalidation must be recorded
before equivalent-work eligibility. Missing external pilot evidence must not be
replaced with synthetic fixture claims. Operator journeys remain separate from
automated timing. No reference observations or human usability evidence have been
published by this implementation slice.

Deterministic command-level tests use local tools and fixtures:

```text
node --experimental-strip-types --test tests/install_benchmarks_test.ts
```
