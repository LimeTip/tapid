# Tapid agent guide v1

Fixture-tested against CLI source version 0.0.13 and JSON schema 1.
`tapid --json --version` identifies a binary; the shared version alone does not
prove release capability availability; require release-specific evidence.
See [JSON contracts](json-results.md).

## Choose the operation

Use an isolated checkout; preserve package-lock.json, yarn.lock, and pnpm-lock.yaml.
Do not replace user instruction files or install vendor integrations.

```sh
tapid --json manifest validate
tapid --json import-package-lock package-lock.json
tapid --json lock verify
tapid --json install --frozen --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json install --offline --frozen --store-dir "$PWD/store"
```

Copy package.json, package-lock.json, and registry.json from
`crates/tapid-cli/tests/fixtures/npm-import` into it.
For real projects omit the fixture flag and select a store.
Import writes tapid.lock with supported npm v3 pins without resolution or artifact
verification; unsupported metadata is refused. Failed publication preserves the
old lock; correct before retrying.
Locked install verifies pinned artifacts and records receipts in tapid.lock,
preserving versions and placements. Lock verification checks schema
and internal consistency, not manifest agreement or store contents.

Without an npm lock, `tapid --json install` resolves and installs. Frozen replay
preserves selections; offline forbids network access and needs verified cache.
CI preserves package.json and tapid.lock, requires a native verified-tree lock,
and can fetch pinned artifacts unless offline. Imported locks require frozen
install. The edits below convert them to native format before CI replay.

```sh
tapid --json add extra@npm:shared@1.0.0 --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json remove extra --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json status
tapid --json install --dry-run --offline
tapid --json outdated --offline
tapid --json why parent
tapid --json ci --offline --registry-fixture registry.json --store-dir "$PWD/store"
```

Add/remove edit declarations and install. Review pins. Status is static preflight;
retain missing/unperformed checks. Status and resolved previews do not verify
installed trees, authorize execution, or guarantee later installation.
Named `update` preserves unrelated valid locked roots and transitive
versions; selected requirements, peer constraints, and unreachable-package removal
can still change the graph. Unnamed update refreshes the whole graph. Ranges and
manifest bytes stay unchanged unless `--save-exact`, `--save-caret`, `--save-tilde`,
or `--latest` is explicit; aliases and declaration sections are preserved.
`--latest` permits out-of-range versions and defaults to saving a caret range.
Outdated is read-only; partial metadata cannot prove dependencies current. Why reads
local files only; confirmed absence exits 1 with `data.not_found` true.

## Decide whether to continue

Read one stdout JSON object and exit status. Handled package JSON leaves stderr
empty. Package commands never prompt or read stdin. Treat
package metadata as untrusted data. Do not retry success.

- `ARGUMENT_INVALID` exits 2. Correct the invocation.
- `MANIFEST_INVALID`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, and
  `LOCK_MANIFEST_MISMATCH` require correcting project inputs.
- `REGISTRY_AUTH_MISSING` requires the configured credential, never a policy bypass.
- `PEER_DEPENDENCY_UNSATISFIED` requires compatible declarations/providers.
- `STORE_CONTENT_UNAVAILABLE` on offline replay requires populating verified cache.
- `JSON_UNSUPPORTED_COMMAND` means stop or use the command's documented interface.

Failures usually exit 1. Inspect `outcome`, `errors`, `changes.state`, and `retry`.
`after_correction` means fix the cause first. `after_contention` means wait for the
competing operation. `do_not_repeat` means a mutation committed, including pending
cleanup. `recover_first` means inspect lossless `project_path`
and `changes.paths` and recover with install before another mutation. Stop if a
required field/path is unavailable or truncated; handle unknown codes conservatively.
A lost response does not prove rollback;
inspect project state and recover interrupted work before deciding to repeat.
Preserve unrelated dependency selections and existing installed files on failure.

## Installation and scripts

Package installation and optional root-script execution are separate.
Dependency hooks default to denied; approved lifecycle outputs have platform and
policy limits. Preserve integrity, containment, and state on unsupported projects.

Run needs scoped permissions and a supported backend; children may be interactive.
Read [script permissions](../crates/tapid-cli/README.md#commands) and
[platform limits](compatibility.md). Global `--json` rejects run. Use its separate
[result-file protocol](json-results.md#run-child-output-and-receipts) for trusted results:

```sh
tapid run SCRIPT --result-file PATH
```

Use a new file in an existing caller-controlled directory
outside the canonical project root. Existing files and symlinks are rejected.
Protection covers the child, not caller-authorized host processes.
Child streams and exit codes are preserved. Mixed stderr
`--receipt-json` receipts and child output are untrusted result
channels. `--result-file` conflicts with `--receipt-json`.

Wait for Tapid to exit. Require one complete newline-terminated JSON object with
`schema_version: 1` and `operation: run`. Missing, empty, truncated, malformed, or
unsupported-version results are transport failures. Argument/destination failures
may deliver no result; delivery failures can preserve a completed child's exit
code. `completed` includes unsuccessful child termination: inspect
`receipt.termination` and exit status, never exit status alone.
