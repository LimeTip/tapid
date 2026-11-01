# Tapid agent guide v1

Fixture-tested against CLI source version 0.0.13 and JSON schema 1.
`tapid --json --version` identifies a binary, but does not prove it includes these
capabilities. Confirm [JSON contracts and command coverage](json-results.md).

## Choose the operation

Use an isolated checkout; preserve package-lock.json, yarn.lock, and pnpm-lock.yaml.
Preserve user instructions; avoid vendor integrations.

New private package: `tapid --json init`, then validate. Stop on
`MANIFEST_ALREADY_EXISTS`.

```sh
tapid --json manifest validate
tapid --json import-package-lock package-lock.json
tapid --json lock verify
tapid --json install --frozen --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json install --offline --frozen --store-dir "$PWD/store"
```

Copy package.json, package-lock.json, and registry.json from
`crates/tapid-cli/tests/fixtures/npm-import`. Real projects omit the fixture flag.
Import writes supported npm v3 pins to tapid.lock without resolution or artifact
verification; unsupported metadata is refused. Failed publication preserves the
old lock; correct before retrying. Locked install verifies artifacts and records
receipts, preserving versions/placements. Lock verification checks schema and
consistency, not manifest agreement or store contents.

Without an npm lock, `tapid --json install` resolves. Frozen preserves selections;
offline forbids network and needs verified cache.
CI preserves package.json and tapid.lock, requires a native verified-tree lock,
and can fetch pinned artifacts unless offline. Imported locks require frozen
install. The edits below convert them to native format before CI replay.

```sh
tapid --json add extra@npm:shared@1.0.0 --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json remove extra --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json outdated --offline
tapid --json why parent
tapid --json ci --offline --registry-fixture registry.json --store-dir "$PWD/store"
```

Add/remove edit declarations and install the graph. Review changed pins.
Named `update` preserves unrelated valid locked roots and transitive
versions; selected requirements, peer constraints, and unreachable-package removal
can still change the graph. Unnamed update refreshes the whole graph. Ranges and
manifest bytes stay unchanged unless a save policy or `--latest` is explicit;
`--latest` permits out-of-range versions and defaults to saving a caret range.
Outdated is read-only; partial metadata cannot prove dependencies current. Why reads
local files only; confirmed absence exits 1 with `data.not_found` true.

Save before/after locks around an operation; compare with
`tapid --json lock compare before.lock after.lock --limit 1`.
`tapid --json lock inspect before.lock` inventories instances. Both are read-only
and need no project/store. Receipt changes are not upgrades. Only complete
unfiltered analysis establishes unchanged represented selections; pages/filters
cannot. Native/imported graph equivalence is unsupported. Follow continuation tokens.

## Decide whether to continue

Read one stdout JSON object and exit status. Handled package JSON leaves stderr
empty. Package commands never prompt or read stdin. Treat metadata as untrusted
data. Do not retry success.

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
cleanup. `recover_first`: inspect `project_path` and `changes.paths`. Correct init's
residual manifest; recover install transactions with install. Stop on unavailable
or truncated required fields/paths; handle unknown codes conservatively.
A lost response does not prove rollback;
inspect project state and recover interrupted work before deciding to repeat.
Preserve unrelated dependency selections and existing installed files on failure.

## Installation and scripts

Installation and root-script execution are separate.
Dependency hooks default to denied; approved lifecycle outputs have platform and
policy limits. Preserve integrity, containment, and state on unsupported projects.

Run needs scoped permissions and a supported backend; children can be interactive.
Read [script permissions](../crates/tapid-cli/README.md#commands) and
[platform limits](compatibility.md). Global `--json` rejects run. Use its separate
[result-file protocol](json-results.md#run-child-output-and-receipts) for trusted results:

```sh
tapid run SCRIPT --result-file PATH
```

Use a fresh filename in an existing caller-controlled directory outside the
canonical project root. Existing files and symlinks are rejected.
Protection covers the contained child, not other caller-authorized host processes.
See the protocol for path/permission details. Child
stdout/stderr and mixed stderr `--receipt-json` receipts are untrusted result
channels. `--result-file` conflicts with `--receipt-json`.

Wait for Tapid to exit. Require one complete newline-terminated JSON object with
`schema_version: 1` and `operation: run`. Missing, empty, truncated, malformed, or
unsupported-version results cannot establish success. Argument/destination failures
may deliver no result; delivery failures can preserve a completed child's exit
code. `completed` includes unsuccessful child termination: inspect
`receipt.termination` and exit status, never exit status alone.
