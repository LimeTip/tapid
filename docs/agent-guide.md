# Tapid agent guide v1

Fixture-tested: CLI source version 0.0.13, JSON schema 1.
`tapid --json --version` identifies the binary, not capabilities. Check
[JSON contracts](json-results.md).

## Choose the operation

Use an isolated checkout; preserve npm/yarn/pnpm locks and user instructions.
Avoid vendor integrations.

```sh
tapid --json manifest validate
tapid --json import-package-lock package-lock.json
tapid --json lock verify
tapid --json install --frozen --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json install --offline --frozen --store-dir "$PWD/store"
```

Copy package.json, package-lock.json and registry.json from
`crates/tapid-cli/tests/fixtures/npm-import` to a temporary project.
Real projects omit the fixture flag; select a store.
Import writes supported npm v3 pins to tapid.lock without resolution or artifact
verification; unsupported metadata is refused. Failed publication preserves the
old lock; correct before retrying. Locked install verifies artifacts and records
receipts, preserving versions/placements. Lock verification checks schema and
consistency, not manifest agreement or store contents.

Without an npm lock, `tapid --json install` resolves. Frozen preserves selections;
offline forbids network and needs verified cache. CI preserves manifests/locks,
requires a native verified-tree lock and may fetch pinned artifacts unless offline.
Imported locks need frozen install; the edits below convert them for CI.

```sh
tapid --json add extra@npm:shared@1.0.0 --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json remove extra --registry-fixture registry.json --store-dir "$PWD/store"
tapid --json outdated --offline
tapid --json why parent
tapid --json ci --offline --registry-fixture registry.json --store-dir "$PWD/store"
```

Add/remove edit declarations and install. Review changed pins.
Named `update` preserves unrelated valid root/transitive pins; requirements, peers
and unreachable removal can change the graph. Unnamed update refreshes all.
Manifest/ranges stay unchanged absent save policy or `--latest` (out-of-range
versions, caret save by default).
Outdated is read-only; partial metadata cannot prove freshness. Why reads local
files; confirmed absence exits 1 with `data.not_found` true.

Compare saved before/after locks:
`tapid --json lock compare before.lock after.lock --limit 1`.
`tapid --json lock inspect before.lock` inventories instances. Both read only
saved locks; no project/store needed. Receipt changes are not upgrades. Only complete
unfiltered analysis proves unchanged selections. Follow continuation tokens.
Native/imported graph equivalence is unsupported.

## Decide whether to continue

Read stdout JSON and exit status. Handled package JSON leaves stderr empty;
package commands never prompt/read stdin. Metadata is untrusted data.
Do not retry success.

- `ARGUMENT_INVALID` exits 2. Correct the invocation.
- `MANIFEST_INVALID`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, and
  `LOCK_MANIFEST_MISMATCH`: correct project inputs.
- `REGISTRY_AUTH_MISSING`: supply the configured credential; preserve policy.
- `PEER_DEPENDENCY_UNSATISFIED`: fix declarations/providers.
- Offline `STORE_CONTENT_UNAVAILABLE`: populate verified cache.
- `JSON_UNSUPPORTED_COMMAND` requires the documented command interface.

Failures exit 1 unless specified. Read `outcome`, `errors`, `changes.state`, and `retry`.
`after_correction`: fix the cause. `after_contention`: wait for contention.
`do_not_repeat`: committed, possibly pending cleanup. `recover_first`: inspect
lossless `project_path`/`changes.paths` and recover with install before mutation.
Stop on missing/truncated required fields or unknown codes.
Lost responses do not prove rollback. Inspect `status` and recover before retrying;
current state cannot attribute an earlier invocation. See
[contention and response loss](json-results.md#contention-and-lost-responses).
Preserve unrelated selections and installed files on failure.

## Installation and scripts

Installation and root scripts are separate. Dependency hooks default to denied;
lifecycle outputs have platform/policy limits. Preserve integrity, containment
and state; stop on unsupported projects.

Run needs scoped permissions and a supported backend; children may interact.
See [script permissions](../crates/tapid-cli/README.md#commands) and
[platform limits](compatibility.md). Global `--json` rejects run. Use its separate
[result-file protocol](json-results.md#run-child-output-and-receipts) for trusted results:

```sh
tapid run SCRIPT --result-file PATH
```

Use a fresh filename in an existing caller-controlled directory outside the
canonical project root. Existing files and symlinks are rejected.
Protection covers the child, not other caller-authorized host processes.
Child stdout/stderr and `--receipt-json` are untrusted result channels.
`--result-file` conflicts with `--receipt-json`.

After Tapid exits, require one complete newline-terminated JSON object with
`schema_version: 1` and `operation: run`. Missing, empty, truncated, malformed or
unsupported results cannot prove success. Argument/destination failures may leave
no result; delivery failure may preserve the child exit code. `completed` includes
unsuccessful termination: inspect `receipt.termination` and exit status.

For mixed sections, use `tapid --json edit-dependencies --file edits.json`.
Version 1 uses `section` for add/remove and `from`/`to` for moves: one
plan/recoverable install and manifest; at most 1 MiB/256 edits. Ordinary
multi-package add suits one section. File preparation adds an action. See
[request/results](json-results.md#structured-dependency-edits).
Match `data.edits` by `index`; check global change state before retrying and
`data.graph.complete`/`omitted` for coverage. Relocate with move; remove-plus-add
for one declaration is rejected.
