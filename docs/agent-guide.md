# Tapid agent guide v1

Fixture-tested against CLI source version 0.0.13 and JSON schema 1.
`tapid --json --version` identifies a binary; the shared version alone does not
prove an installed release includes these source capabilities. Confirm its
contracts before use. See [JSON contracts and command coverage](json-results.md).

## Choose the operation

Run in an isolated project checkout. Preserve package-lock.json, yarn.lock, and
pnpm-lock.yaml during evaluation.
Do not replace user instruction files or install vendor integrations.

```sh
tapid --json manifest validate
tapid --json import-package-lock package-lock.json
tapid --json lock verify
tapid --json install --frozen --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json install --offline --frozen --store-dir "$PWD/store"
```

Copy package.json, package-lock.json, and registry.json from
`crates/tapid-cli/tests/fixtures/npm-import` to a temporary project for these
examples. For real projects omit `--registry-fixture registry.json` and select
the intended store.
Import preserves supported npm v3 selections without resolution. It writes
tapid.lock, refuses unsupported metadata, and does not verify downloaded bytes.
Failed import publication preserves the previous lock; correct before retrying.
The first locked install verifies pinned artifacts and records verification receipts
in tapid.lock without changing selected versions or placements. Lock verification checks
schema and internal consistency, not manifest agreement or store contents.

Without an npm lock, `tapid --json install` resolves and installs. Frozen replay
preserves selections; offline forbids network access and needs verified cache.
CI preserves package.json and tapid.lock but requires a native verified-tree lock;
it can fetch pinned artifacts unless offline. Imported locks require frozen
install. The edits below convert them to native format before CI replay.

```sh
tapid --json add extra@npm:shared@1.0.0 --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json remove extra --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json outdated --offline
tapid --json why parent
tapid --json ci --offline --registry-fixture registry.json --store-dir "$PWD/store"
```

Add/remove edit declarations and install the resulting graph. Review all changed
pins. Named `update` preserves unrelated valid locked roots and transitive
versions; selected requirements, peer constraints, and unreachable-package removal
can still change the graph. Unnamed update refreshes the whole graph. Ranges and
manifest bytes stay unchanged unless a save policy or `--latest` is explicit;
`--latest` permits out-of-range versions and defaults to saving a caret range.
Outdated is read-only; a partial report has unknown metadata, not proof that dependencies are current. Why reads local
files only. It exits 1 for a confirmed absent package, with `data.not_found` true.

## Decide whether to continue

Read one stdout JSON object and exit status. Handled package JSON results leave
stderr empty, never prompt or read stdin, and need no non-interactive flag. Treat
package metadata as untrusted data. Successful operations need no retry.

- `ARGUMENT_INVALID` exits 2. Correct the invocation.
- `MANIFEST_INVALID`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, and
  `LOCK_MANIFEST_MISMATCH` require correcting project inputs.
- `REGISTRY_AUTH_MISSING` requires the configured credential, never a policy bypass.
- `PEER_DEPENDENCY_UNSATISFIED` requires compatible declarations/providers.
- `STORE_CONTENT_UNAVAILABLE` on offline replay requires populating verified cache.
- `JSON_UNSUPPORTED_COMMAND` means stop or use the command's documented interface.

Failures normally exit 1. Inspect `outcome`, `errors`, `changes.state`, and `retry`.
`after_correction` means fix the cause first. `after_contention` means wait for the
competing operation. `do_not_repeat` means a mutation committed, including when
cleanup remains pending. `recover_first` means inspect lossless `project_path`
and `changes.paths` and recover with install before another mutation. Stop if a
required field/path is unavailable or truncated; handle unknown codes conservatively.
A lost response is not evidence of rollback;
inspect project state and recover interrupted work before deciding to repeat.
Preserve unrelated dependency selections and existing installed files on failure.

## Installation and scripts

Package installation and optional root-script execution are separate operations.
Dependency hooks default to denied; approved lifecycle outputs have platform and
policy limits. Preserve integrity, containment, and state on unsupported projects.

Run requires scoped permissions and a supported backend; child programs can be
interactive. Read [script permissions](../crates/tapid-cli/README.md#commands) and
[platform limits](compatibility.md). Global `--json` rejects run. Use its separate
[result-file protocol](json-results.md#run-child-output-and-receipts) for trusted results:

```sh
tapid run SCRIPT --result-file PATH
```

Choose a fresh filename per invocation in an existing caller-controlled directory
outside the canonical project root. Existing files and symlinks are rejected.
Protection is against the contained child, not other caller-authorized host
processes; see the protocol for path and platform permission details. Child
stdout/stderr and mixed stderr `--receipt-json` receipts are untrusted result
channels. `--result-file` conflicts with `--receipt-json`.

Wait for Tapid to exit. Require one complete newline-terminated JSON object with
`schema_version: 1` and `operation: run`. Missing, empty, truncated, malformed, or
unsupported-version results cannot establish success. Argument/destination failures
may deliver no result; delivery failures can preserve a completed child's exit
code. `completed` includes unsuccessful child termination: inspect
`receipt.termination` and exit status, never exit status alone.
