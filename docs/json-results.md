# JSON command results

Use `tapid --json install` or `tapid install --json` to receive one newline-terminated JSON object on stdout. Supported commands also leave stderr empty, including on handled failures. Tapid does not emit progress, prompts, ANSI sequences, or registry error prose in this mode. An OS termination or a broken output pipe can prevent delivery of the result.

## Coverage

| Command | Global `--json` |
| --- | --- |
| `install`, alias `i` | Install or replay result; resolved dependency preview with `--dry-run` |
| `ci` | Locked install result with the existing install transaction states |
| `manifest validate`, `lock verify` | Read-only validation result |
| `import-package-lock` | Offline npm v3 import result; verification remains pending |
| `add`, `remove`, `update`, `prune` | Dependency operation result; `add`, `remove`, and `update` support resolved previews with `--dry-run` |
| `tool install`, `tool list`, `tool update`, `tool remove` | Tool inventory and command mappings with operation change/recovery state |
| `outdated` | Sorted direct dependency entries, with partial metadata outcomes |
| `explain` | Exact package evidence under `data`; registry metadata or a local snapshot, with optional artifact byte verification |
| `status` | Read-only local project preflight; blockers and pending recovery retain `data` and exit 1 |
| `why` | Dependency explanation paths under `data.paths`; a confirmed empty match returns `data.not_found: true` and exit 1, while a bounded empty search is partial and exits 0 |
| `cache`, `cache info`, `cache clean`, `clean` | Package cache inspection, preview, or cleanup result |
| `init` | Creates `package.json` with committed change state; handled failures return a typed error |
| `license`, `upgrade` | `JSON_UNSUPPORTED_COMMAND`, exit 1, before execution |
| `run` | `JSON_UNSUPPORTED_COMMAND`, exit 1, before launching a child |
| Private release helpers, no command | `JSON_UNSUPPORTED_COMMAND`, exit 1 |
| Invalid arguments | `ARGUMENT_INVALID`, exit 2 |
| `--help`, `-h`, or `help` with `--json` | Successful `help` result, exit 0 |
| `--version` or `-V` with `--json` | Successful `version` result, exit 0 |

The exact `--json` token before the first `--` selects machine-readable parsing errors, even if Clap cannot parse the command. Values after `--` never select JSON mode. Parsing errors use operation `parse`; unsupported commands retain their canonical top-level command name. A missing command uses operation `none`. Neither echoes command-line values. Human help remains available without `--json`.

## Schema version 1

Every object contains these fields:

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1` |
| `operation` | Canonical top-level command name, `help`, `version`, `parse`, or `none` |
| `outcome` | `success`, `failure`, or `partial` |
| `project` | Bounded display of the effective project directory; null before command execution |
| `project_path` | Lossless native project path record; null before command execution |
| `changes.state` | `unchanged`, `rolled_back`, `committed`, `committed_cleanup_pending`, or `recovery_required` |
| `changes.files` | Sorted, deduplicated display paths for affected outputs |
| `changes.paths` | Sorted, deduplicated lossless native path records for affected outputs |
| `truncated_fields` | Sorted, deduplicated JSON pointers to display or metadata scalars shortened by the byte limit |
| `warnings` | Sorted, deduplicated warning codes |
| `errors` | Error objects with a stable `code`; operational errors also have `phase` of `operation` or `recovery`, and bounded typed `context` when available |
| `retry` | null or typed retry advice |
| `data` | Command-specific object, or null on failure |

Help results contain `data.text`, the selected command's help text with no ANSI formatting. For example, `tapid --json help run` returns run help without launching a child. Both short and long help forms preserve their usual content. Version results contain `data.name` of `tapid` and `data.version`, the executable's package version. These informational results use `outcome: "success"`, empty errors and warnings, null project and retry, and unchanged state with no affected files. They do not read project files. Help text is presentation content, not a stable command-discovery schema; its wording and layout may change within schema version 1.

Successful `update` results include `data.graph_changes`, an array of changed package instances with `package` and `reason`. Reasons identify requiring dependency or peer edges, or explain removal of an unreachable instance. The envelope includes `UPDATE_GRAPH_CHANGE` when this array is nonempty. Strings use the usual display limits and truncation pointers. Named updates retain unrelated valid pins; these entries explain changes required by selected updates.

`explain` successful `data` contains `package`, `version`, `registry`, `source`, `metadata_fetched_at`, `metadata_read_at`, `source_timestamp`, `source_timestamp_status`, `published_at`, `freshness`, `artifact`, `registry_integrity`, `byte_integrity`, `registry_signatures`, `provenance`, `publisher_identity`, `vulnerabilities`, `malware_analysis`, `human_review`, and `limitations`; shortened display values are listed in the envelope's `truncated_fields`. The nested fields are `registry_integrity.status/value`, `byte_integrity.status/actual/scope/checked_at`, `registry_signatures.status/count`, `provenance.status/reference/fetched`, `publisher_identity.status`, `vulnerabilities.status/reason`, `malware_analysis.status`, and `human_review.status`. Reported claims are not treated as verified evidence, and freshness is `unknown`. The serializer does not emit `data.not_found`: an operational error, including unavailable or invalid requested metadata, uses the standard `failure` envelope with `data: null`, a typed `errors` entry, and exit status 1. With `--artifact-file`, a byte mismatch is a failure result with populated evidence data and `INTEGRITY_MISMATCH`; a match remains `success`. `--registry-metadata` records a local snapshot source rather than contacting the registry.

`why` data contains `package`, deterministic `paths`, `truncated`, `not_found`, and `graph_warnings`. Each path contains `steps` with package identity, incoming dependency name and kind, and source kind (`registry`, `workspace`, or `copied`). A confirmed missing match is an unchanged successful operation envelope with `data.not_found: true`, but the command exits 1 to preserve its CLI no-match contract. If traversal reaches a safety bound before finding a path, the envelope outcome is `partial`, `data.truncated` is true, `data.not_found` is false, and the command exits 0. Operational errors use the standard `failure` envelope and typed `errors` entry.

## Package cache results

`cache`, `cache info`, `cache clean`, and `clean` use the version 1 envelope. The operation is `cache` for its subcommands and `clean` for the shortcut. `project` is null and `changes.files` and `changes.paths` are empty because maintenance does not change project outputs. `data.scope` is `published_package_data`; `data.store` is bounded display text and `data.store_path` uses the existing lossless native-path encoding. `data.action` is `info`, `preview`, or `clean`. The `data.summary` object contains `artifacts` and `trees`, each with `entries` and logical file `bytes`, plus `preserved_entries` for unrecognized entries inside those namespaces. Staging and upgrade-recovery caches are excluded. The version 1 `bytes` field names are unchanged: they count file content lengths, not allocated disk space or predicted reclaimed disk space. Human output labels the same values as logical bytes.

Inspection and previews report `changes.state: "unchanged"`. Successful eviction exits 0 and reports `committed` when entries were removed, or `unchanged` for an empty deletion set. `CACHE_BUSY` exits 1 with `unchanged` state and `retry: "after_contention"`. Other failures before eviction exit 1 with `unchanged` state and `CACHE_MAINTENANCE_FAILED`. A deletion or directory-sync failure exits 1 and reports `committed_cleanup_pending` because some cached data may have been removed; this state concerns cache eviction, not a project transaction. Cleanup has stopped, and no background cleanup is scheduled. Inspect the cache or retry after correcting the filesystem error. The state does not promise that removal or synchronization will finish automatically. Failed results have `data.summary: null`. Cache-location failures, including explicit roots that are files or symlinks, use `CACHE_PATH_INVALID`. All cache operational errors include `phase: "operation"`. An empty explicit `--store-dir` is rejected during argument parsing as `ARGUMENT_INVALID`, exit 2.

Validation success data contains `action` of `validate` or `verify` and
`valid: true`, with unchanged state and no changed files. Manifest validation
uses the selected manifest's parent directory as the project; lock verification
uses the current directory. Failures use typed `MANIFEST_INVALID`,
`LOCKFILE_INVALID`, or `LOCKFILE_MISSING` errors and `after_correction`.
These commands do not compare manifest and lock, access the store, or recover.

Npm import success data contains `format: "npm_v3"` and
`verification: "pending"`. It reports `committed` with tapid.lock as the
changed output, preserving the original npm lock and installed files. Validation
and contention failures preserve project outputs. Pending install state returns
`RECOVERY_FAILED`, `recovery_required`, and `recover_first` without recovery.
Import stages and syncs the new lock in the project directory, then replaces
tapid.lock with one rename. It never moves the old lock to a backup first.
A staging or replacement I/O failure reports `TRANSACTION_FAILED`, `unchanged`,
empty changed paths, and `after_correction`; the previous lock remains in place.
No rollback or install recovery is needed for these publication failures.
Successful replacement reports `committed`. Multi-output installation retains
its separate journal-backed rollback and recovery contract.
Import errors classify manifest, lock, registry configuration,
contention, and transaction failures at their operation boundaries. Workspace-member
edits detected between discovery and import publication return
`LOCK_MANIFEST_MISMATCH`; other workspace validation failures retain `INVALID_DATA`.

CI uses the install data and failure contract below, including progress suppression.
It requires a native verified-tree lock, preserves package.json and tapid.lock,
and replaces managed node_modules. Imported locks use install with --frozen.

## Install and dependency preview results

Ordinary install and mutating lifecycle results contain `data.package_count` and `data.replayed`. They exclude graphs, artifact URLs, raw metadata, credentials, source error chains, and uncontrolled diagnostic messages.

`tapid --json install --dry-run`, `add --dry-run`, `remove --dry-run`, and `update --dry-run` instead return a resolved graph preview under the version 1 envelope. The operation retains its canonical command name. Successful previews use `outcome: "success"`, `changes.state: "unchanged"`, empty affected-file lists, null retry advice, and exit 0. This payload is an observation, not an executable plan or a prepared installation.

| Preview data field | Meaning |
| --- | --- |
| `preview_kind` | `"resolved_graph"` |
| `artifacts_prepared` | Always `false` |
| `target` | Current OS/architecture/libc inspection target, with `unspecified` when libc is unavailable |
| `declarations` | Declaration edits with `section`, `name`, `before`, and `after`; absent requirements are null |
| `packages` | Package records and graph changes described below |
| `root_changes` | Changed graph root bindings with `name`, `before`, and `after`; absent bindings are null, present bindings are package keys |
| `effects` | Network permission, metadata lookups, and side effects described below |
| `limitations` | Presentation strings describing unavailable evidence and preview limits |

Declarations follow dependency-section order: `dependencies`, `devDependencies`, `optionalDependencies`, then `peerDependencies`, with names sorted within each section. Packages sort by canonical package key; root changes sort by binding name. Empty change lists are arrays, not null.

Each package entry contains `key`, `source`, `name`, `version`, `integrity`, `dependencies`, `change`, `before`, `after`, and `reasons`. A package record contains the first six fields. `dependencies` maps local dependency names to selected package keys. `integrity` contains available integrity metadata or null; it does not attest artifact bytes. `before` and `after` contain the corresponding record or null when absent. The top-level record describes the after state when present, otherwise the before state. Canonical keys distinguish source, name, version, peer context, and platform context.

`change` is `added`, `removed`, `changed`, or `unchanged`. A version, peer-context, or platform-context change produces removal of the old identity and addition of the new one. `changed` means a retained identity has different integrity metadata or dependency targets. Unchanged packages remain in the array. Inspect `root_changes` too: promoting an existing transitive package to a direct root can change a binding without changing a package record.

Each reason contains `graph` of `before` or `after`, `relation` of `direct` for a graph root or `dependency` for a parent edge, `parent` as the parent package key or null for roots, `dependency` as the incoming name, and `requirement` as an available requirement or null. These are selection edges in the indicated graph, not API compatibility claims. Existing lockfile transitive edges do not preserve their original dependency kind or declared range; unavailable ranges remain null rather than presenting reconstructed exact pins as declared requirements.

`effects` contains `network_allowed`, `metadata_source`, `metadata_lookups`, `artifact_requests`, `cache_reads`, `cache_writes`, `project_writes`, `recovery`, and `scripts`. `network_allowed` describes permission to fetch registry metadata, not proof that a request occurred. `metadata_source` is `registry`, `fixture`, or `lockfile`; `metadata_lookups` counts package metadata retrievals, not individual HTTP requests. Successful metadata is reused by source and actual package name within one graph operation; reuse by another consumer does not increment the count. A matching online lock can produce zero lookups even when network access is allowed. Fixtures and offline/frozen inspection forbid registry access. `artifact_requests` is always 0, and the remaining effect flags are always false. Cache fields refer to the package store, not reading the project's lockfile.

Previews do not create an activation lock, recover transactions, access the package store, fetch artifacts, or run scripts. Pending recovery fails without attempting recovery. `install --dry-run --offline` and `--frozen` inspect a matching validated lock without establishing that its artifacts are cached. Previews requiring copied file/Git resolution or imported npm locks fail explicitly. Artifact availability, archive validity, lifecycle outputs, and later installation success remain unverified. A later command reads current inputs and may resolve different metadata.

Preview metadata uses the same scalar sanitization and byte limits as other version 1 data. The combined envelope and preview truncation pointers are sorted and deduplicated in `truncated_fields`. Handled failures use the standard failure envelope with `data: null`, unchanged project state, a typed error, and exit 1.

## Outdated results

Outdated data contains `entries`, `total_entries`, and `truncated`. Each entry contains `identity`, `kind`, `declared`, `locked`, `newest_compatible`, `newest_available`, and `error`. Unknown versions are null. The error is null or an object with a typed code. Entries sort by identity and dependency kind. Metadata failure in any entry makes the overall outcome `partial`, even if that entry falls beyond the output limit. Partial outdated reports retain exit 0; callers must inspect `outcome` and entry errors.

Each entry also contains `compatible_impact` and `available_impact`. Both contain `version_change`, `lockfile`, and `manifest`. Version change is `unchanged`, `major`, `minor`, `patch`, `prerelease`, `downgrade`, `unlocked`, or `unknown`, comparing the locked version with the selected metadata version. Major/minor/patch take precedence over prerelease differences on an upgrade. These are SemVer comparisons, not API compatibility claims. Lockfile and manifest impact are `changed`, `unchanged`, or `unknown`. Lockfile impact compares the direct version pin if the shown version is selected; it does not predict transitive changes or resolution success. Manifest impact indicates whether the shown version requires changing the declared range, preserving npm alias and workspace range semantics. Peer declarations do not select direct lockfile pins, so their lockfile impact is `unchanged` even when registry metadata is unavailable. A provider installed through another dependency section does not change this rule; that section reports its own pin impact. Missing metadata otherwise yields unknown impact. `outdated --offline` without a fixture returns `REGISTRY_METADATA_UNAVAILABLE` for registry dependencies and a partial result, with locked versions retained and unchecked versions null. Local workspace versions remain available. Pending transactions return failure with `RECOVERY_FAILED`, `recovery_required`, and `recover_first`, without attempting recovery. An unchanged pin does not establish an unchanged whole lockfile after `update`. Named updates preserve unrelated compatible pins; selected requirements and peer constraints can still require transitive changes. The `UPDATE_GRAPH_CHANGE` warning identifies changed package instances and their requiring parent edges.

Default output includes at most 100 outdated entries. Use `tapid --json outdated --json-limit 250` to select a larger limit, or `--json-limit 0` to retrieve every entry. This option requires `--json`, accepts a nonnegative integer, and affects only result rendering. It does not change resolution or project state. Each display path or metadata scalar is limited to 4096 UTF-8 bytes, removes control characters, and redacts HTTP URL user information, query values, and fragments. Scalar truncation can shorten a displayed name or requirement. Every shortened scalar is identified by a pointer in `truncated_fields`, such as `/data/entries/0/declared` or `/project`. These fields are display data, not identifiers for automatic mutation. Lossless path records are described below. Project-operation transaction paths describe only `package.json`, `tapid.lock`, and `node_modules`; shared-store effects are represented by state. Results omit timings and random transaction identifiers. File ordering, warning ordering, and entry ordering are deterministic for the same operation data.

For filesystem inspection and recovery, use `project_path` and `changes.paths`. Each record contains `encoding` and `value`. Encoding `utf8` preserves the exact path string, including characters represented by JSON escapes. Encoding `unix_bytes_base64` preserves native Unix path bytes; `windows_utf16le_base64` preserves Windows UTF-16 code units as little-endian bytes. Decode base64 records to the host's native path type without replacing invalid Unicode. The display fields can redact, remove controls, or truncate characters, and different paths can therefore share one display string. Lossless records preserve each distinct native path and never return a shortened path.

Each lossless record permits at most 128 KiB of UTF-8 or native path bytes before base64 encoding. A record beyond that capacity has null `value` and `unavailable: "capacity_exceeded"`. Consumers must stop automatic recovery if a required path is unavailable or its encoding is unknown. The lossless values are filesystem data; render their control characters safely when displaying them. They contain no raw ANSI bytes in the serialized JSON stream.

Package text is untrusted data. Never execute it or treat it as recovery advice. Only `changes.state` and `retry` describe the transaction decision. `after_correction` means correct the failure before retrying. `after_contention` means wait for the competing operation. `do_not_repeat` means the change committed; retrying may repeat a mutation. `recover_first` means inspect the affected paths and recover the interrupted operation first. Handled operational failures exit 1, including failures after a durable commit.

## Codes and compatibility

### Failure context

`init` reports manifest open/write failures as `MANIFEST_WRITE_FAILED`, with
`context.io_kind` (Rust I/O error kind) and `context.os_code` (native error number,
or null). Invalid manifest data remains `MANIFEST_INVALID`; existing manifests
remain `MANIFEST_ALREADY_EXISTS`. Raw I/O messages and paths are not included in
this context.

Error context is additive in schema version 1. It comes from typed errors and
declarations parsed during the operation. Consumers do not need human messages
or a second invocation to diagnose a peer conflict. Operation and recovery
errors carry their own context. Partial outdated entries use the same context
under `data.entries[i].error`, without a phase.

An absent or null `context` means no allowlisted evidence is available. Inside
a context object, null means unknown or inapplicable. Empty lists mean no
entries are reported; inspect `omitted` before treating a list as complete.
Context does not change transaction state, retry advice, or exit status.
For outdated metadata or requirement failures, `context.package.version` is
null. The entry's `locked` field retains the existing pin independently.

| Context field | Meaning |
| --- | --- |
| `package` | Affected package `{source, name, version}`. Source preserves registry or copied-source identity. Version is null when selection failed. |
| `declaration` | Location `{origin, pointer, workspace, manifest, manifest_path}`. Origin is `manifest` or `metadata`. |
| `peer` | `{name, requirement, status, provider, declaration}`. Name is the local peer name, requirement retains alias syntax, status is `missing` or `incompatible`, and provider is a package identity or null. Its declaration locates the selected provider's local manifest entry when known. |
| `comparison` | `{kind, algorithm, expected, observed}`. The kind labels what was compared. Algorithm is null for non-digest comparisons. |
| `requirements` | Requirements with `{local_name, requirement, package, declaration}`. Package identifies the declaring metadata package; root requirements use manifest provenance. |
| `available` | Candidate versions known to the resolver, without a compatibility or safety claim. |
| `omitted` | Counts of unique entries omitted from `requirements` and `available`. Zero means the collection limit omitted none. |

Declaration pointers use JSON Pointer escaping, for example
`/devDependencies/@scope~1host`. The section is part of the pointer. An empty
pointer identifies the whole document, including syntax failures. Workspace
declarations include the member name and that member's manifest path. Metadata
declarations have null manifest paths and must not be treated as editable local
fields. For an absent peer provider, add a provider through a supported local
dependency section; there is no existing provider declaration to report.
Effective root overrides of transitive or workspace requirements point to the
root manifest's `/overrides/...` field, preserving the original selector spelling.
Transitive requirements retain the declaring package identity and effective alias syntax.
Resolver root origins exclude `peerDependencies` entries, even when they have
the same name and range as an installed dependency. Peer requirements remain
visible in the peer-specific declaration and requirement fields.

Manifest comparisons report the expected type or validation constraint and the
observed JSON type, `missing`, or `invalid_json`. They never echo the value of
an arbitrary field. Digest comparisons retain the digest's encoded prefix;
`root_manifest_digest` compares the lock's recorded digest with the current
manifest digest. Peer comparisons use the declared range and the observed
provider version, or `missing`.

Each display/metadata scalar is limited to 4096 UTF-8 bytes. Each context list
contains at most 100 entries, independently of `--json-limit`. Lists sort and
deduplicate before limiting. `truncated_fields` contains precise pointers such
as `/errors/0/context/package/name`. The schema has fixed nesting; its deepest
record is `requirements[i].declaration.manifest_path`, at five nested containers
including the context object and requirements array. Context never embeds a
graph, raw JSON value, source-error chain, or recovery instruction.

Context reuses the result's control removal and URL credential redaction.
Pointers and package names are display data too: redaction or truncation can
prevent them from identifying a unique input. Do not use shortened or sanitized
text as authority for automatic edits. `manifest_path` uses the existing lossless
native-path encoding and 128 KiB capacity limit, independently of display text.
Stop automatic edits if the required native path is unavailable. Locations
describe the attempted operation, including a proposed manifest change that was
subsequently rolled back.

| Error code | Context coverage |
| --- | --- |
| `PEER_DEPENDENCY_UNSATISFIED` | Exact requiring and provider identities, metadata declaration, expected/observed requirement, and known local declarations. |
| `RESOLUTION_FAILED` | Missing-candidate and conflicting-constraint errors include target identity, exact requirement origins, and candidates. Other resolver failures may lack structured evidence. |
| `MANIFEST_INVALID` | Direct manifest parse failures include manifest path, field, expected constraint, and observed type. Filesystem/read failures can lack field evidence. |
| `LOCK_MANIFEST_MISMATCH` | Ordinary and imported-lock root-manifest digest mismatches include expected/observed SHA-256 digests. Other imported-lock and workspace validation messages may lack comparison evidence. |
| `INTEGRITY_MISMATCH` | Typed store failures, online/frozen artifact and tree comparisons, copied-source pins, imported npm artifacts, and explain byte mismatches include expected/observed values. Package identity is null when archive metadata has not yet been validated. Invalid imported identity/integrity evidence uses a fixed marker instead of echoing arbitrary input. |
| `REGISTRY_METADATA_INVALID`, `REGISTRY_METADATA_UNAVAILABLE`, `REGISTRY_TRANSPORT_FAILED`, `INVALID_REQUEST` | Partial outdated entry failures include the target identity and local declaration when known. Request-validation workspace parse failures retain their existing `INVALID_REQUEST` code and add manifest field evidence. Other call sites can lack context. |
| `PROJECT_UNAVAILABLE`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, `REGISTRY_CONFIGURATION_INVALID`, `REGISTRY_AUTH_MISSING`, `ARCHIVE_INVALID`, `STORE_FAILED`, `STORE_CONTENT_UNAVAILABLE`, `STORE_BUSY`, `PROJECT_BUSY`, `MATERIALIZATION_FAILED`, `TRANSACTION_FAILED`, `RECOVERY_FAILED`, `INVALID_DATA` | Context is unavailable unless the caller retains allowlisted package or declaration evidence. No source message is exposed as a substitute. |
| `CACHE_BUSY`, `CACHE_PATH_INVALID`, `CACHE_MAINTENANCE_FAILED` | Store location remains in cache data when available; no dependency/manifest context applies. |
| `ARGUMENT_INVALID`, `JSON_UNSUPPORTED_COMMAND` | No dependency/manifest context; command-line values are not echoed. |

Workspace discovery retains its caller's existing category, including
`PROJECT_UNAVAILABLE` and `INVALID_DATA`, while adding typed manifest evidence.

A missing-provider failure can be diagnosed from this single result:

```json
{
  "schema_version": 1, "operation": "install", "outcome": "failure",
  "project": "/project", "project_path": {"encoding": "utf8", "value": "/project"},
  "changes": {"state": "unchanged", "files": [], "paths": []},
  "truncated_fields": [], "warnings": [], "retry": "after_correction", "data": null,
  "errors": [{"code": "PEER_DEPENDENCY_UNSATISFIED", "phase": "operation", "context": {
    "package": {"source": "https://registry.npmjs.org", "name": "plugin", "version": "1.0.0"},
    "declaration": {"origin": "metadata", "pointer": "/peerDependencies/react", "workspace": null, "manifest": null, "manifest_path": null},
    "peer": {"name": "react", "requirement": "^18", "status": "missing", "provider": null, "declaration": null},
    "comparison": {"kind": "peer_requirement", "algorithm": null, "expected": "^18", "observed": "missing"},
    "requirements": [{"local_name": "plugin", "requirement": "1.0.0", "package": null, "declaration": {"origin": "manifest", "pointer": "/dependencies/plugin", "workspace": null, "manifest": "/project/package.json", "manifest_path": {"encoding": "utf8", "value": "/project/package.json"}}}],
    "available": [], "omitted": {"requirements": 0, "available": 0}
  }}]
}
```

An incompatible-provider result identifies the existing provider declaration:

```json
{
  "schema_version": 1, "operation": "install", "outcome": "failure",
  "project": "/project", "project_path": {"encoding": "utf8", "value": "/project"},
  "changes": {"state": "unchanged", "files": [], "paths": []},
  "truncated_fields": [], "warnings": [], "retry": "after_correction", "data": null,
  "errors": [{"code": "PEER_DEPENDENCY_UNSATISFIED", "phase": "operation", "context": {
    "package": {"source": "https://registry.npmjs.org", "name": "plugin", "version": "1.0.0"},
    "declaration": {"origin": "metadata", "pointer": "/peerDependencies/react", "workspace": null, "manifest": null, "manifest_path": null},
    "peer": {"name": "react", "requirement": "^18", "status": "incompatible", "provider": {"source": "https://registry.npmjs.org", "name": "react", "version": "19.0.0"}, "declaration": {"origin": "manifest", "pointer": "/dependencies/react", "workspace": null, "manifest": "/project/package.json", "manifest_path": {"encoding": "utf8", "value": "/project/package.json"}}},
    "comparison": {"kind": "peer_requirement", "algorithm": null, "expected": "^18", "observed": "19.0.0"},
    "requirements": [{"local_name": "plugin", "requirement": "1.0.0", "package": null, "declaration": {"origin": "manifest", "pointer": "/dependencies/plugin", "workspace": null, "manifest": "/project/package.json", "manifest_path": {"encoding": "utf8", "value": "/project/package.json"}}}],
    "available": [], "omitted": {"requirements": 0, "available": 0}
  }}]
}
```

Operational codes are `INVALID_REQUEST`, `PROJECT_UNAVAILABLE`, `PROJECT_CHANGED`, `MANIFEST_INVALID`, `MANIFEST_ALREADY_EXISTS`, `MANIFEST_WRITE_FAILED`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, `LOCK_MANIFEST_MISMATCH`, `REGISTRY_CONFIGURATION_INVALID`, `REGISTRY_AUTH_MISSING`, `REGISTRY_METADATA_INVALID`, `REGISTRY_METADATA_UNAVAILABLE`, `REGISTRY_TRANSPORT_FAILED`, `RESOLUTION_FAILED`, `PEER_DEPENDENCY_UNSATISFIED`, `INTEGRITY_MISMATCH`, `ARCHIVE_INVALID`, `STORE_FAILED`, `STORE_CONTENT_UNAVAILABLE`, `STORE_BUSY`, `PROJECT_BUSY`, `MATERIALIZATION_FAILED`, `TRANSACTION_FAILED`, `RECOVERY_FAILED`, and `INVALID_DATA`. Protocol codes are `ARGUMENT_INVALID` and `JSON_UNSUPPORTED_COMMAND`. Warning codes are `UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED` and `PREVIOUS_TRANSACTION_RECOVERED`.

`PROJECT_CHANGED` means the starting manifest changed after dependency planning or while preparing a preview. Lifecycle commands compare the expected semantic manifest with the current manifest under the project lock before writing an override. This failure uses unchanged state and `retry: "after_correction"`; rerun against current inputs. It does not claim protection against uncooperative writers changing files throughout a transaction.

Within schema version 1, existing field types, code meanings, transaction states, and exit semantics remain stable. Additive fields, new error or warning codes, and new command coverage are compatible. Consumers must ignore unknown fields and handle unknown codes conservatively. Removing or renaming fields or changing their meaning requires a new schema version. Schema versioning is independent of Tapid's package version.

## Run child output and receipts

`tapid run SCRIPT --result-file PATH` writes a separate, newline-terminated JSON result while preserving child stdout, stderr, and exit codes. It suppresses the usual stderr receipt and conflicts with `--receipt-json`. Global `--json` still rejects `run` before launch to preserve the package-result stdout guarantee. Legacy `--receipt-json` appends a receipt to arbitrary child stderr and cannot be used as a trusted result channel.

Choose a fresh filename in an existing directory controlled by the caller, outside the canonical project root. Relative paths resolve against Tapid's invocation directory. Tapid resolves the parent directory before creating the file, rejects destinations inside the project even through directory symlinks, and exclusively creates the destination. Existing files, final-component symlinks, directories, and missing parents fail before launch without overwriting anything. Unix files have mode `0600`; Windows files inherit the caller directory's ACL. Keep the directory protected from other host writers. This channel protects against the contained child, not other processes with the caller's authority. Tapid's file handle is not inherited by the script. Project write permissions cannot extend outside the project; there is no unrestricted fallback.

The file starts empty. Tapid holds the same open file through execution, serializes the complete result in memory, then writes it with a final newline and synchronizes the file. It does not rename, reopen, or delete the destination. Read only after Tapid exits, require one complete newline-terminated JSON object, and validate `schema_version` and `operation`. Empty, truncated, missing, malformed, or unknown-version results are transport failures, never successful runs. OS termination or an I/O failure can leave an empty or partial file. Tapid reports delivery errors on stderr, attempts to truncate failed writes, and preserves an already completed child's exit code. Before child completion, failures exit 1. An invalid destination or argument-parsing failure cannot deliver a result. Use a new filename for every invocation and do not infer successful execution from an existing file or the process exit code alone.

The run envelope is separate from the package-operation envelope:

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1` |
| `operation` | `run` |
| `outcome` | `prelaunch_failure`, `execution_failure`, or `completed` |
| `error` | null on completion, otherwise an object with stable `code` and nullable checked `completion` evidence |
| `policy_inputs` | Selected policy and its provenance once available, otherwise null |
| `receipt` | Existing schema-version-1 checked enforcement and completion receipt, or null when no receipt was accepted |

`completed` includes nonzero child exits and limit or cancellation terminations. Inspect `receipt.termination` and the CLI exit status. `execution_failure` means the runner returned an error without an accepted receipt. `error.completion`, when present, preserves the runner's checked cleanup evidence after process creation. Its absence does not establish whether a child started or whether descendants stopped. Policy selection alone never establishes enforcement. Before launch, codes are `PROJECT_UNAVAILABLE`, `WORKSPACE_INVALID`, `MANIFEST_INVALID`, `SCRIPT_MISSING`, `CONFIG_UNAVAILABLE`, `CONFIG_INVALID`, `PROFILE_MISSING`, `PERMISSIONS_INVALID`, `REGISTRY_CONFIGURATION_INVALID`, `ENVIRONMENT_INVALID`, and `PREPARATION_FAILED`. Runner failures reuse the receipt diagnostic categories, including `unsupported-containment`, `policy-violation`, `spawn`, and `internal`. Treat unknown codes conservatively.

The nested receipt retains requested, declared, observed, and enforced dimension maps, per-dimension evidence and limitations, effective filesystem grants, configured limits, termination, and completion evidence. Unsupported containment remains a failure. The result channel adds no descendant ownership, detached-descendant cleanup, or resource enforcement guarantees beyond the checked backend evidence.

Legacy stderr receipts add `policy_inputs` when invocation permission flags are used. Result-file envelopes include the same metadata at the top level once policy selection succeeds, including profile-only runs. `source` is `script-profile`, `configured-defaults` or `invocation-baseline`; `configuration` contains the configured ceiling or null; `invocation` contains the supplied permission lists/booleans; `effective` describes the selected project policy and preserved assurance/limits. Environment entries are names only. Omitted lists inherit configured grants, so an empty invocation list does not itself mean the effective grant list is empty. Backend runtime additions remain in `effective_filesystem`, and checked enforcement remains in the established evidence. Profile-only stderr receipts retain their existing shape. Consumers must accept additive fields; this metadata does not make untrusted child streams a trusted machine-result channel.

## Project status

`tapid --json status` uses the version 1 envelope and always reports unchanged
project files, including when recovery is pending. `data` remains present on
inspection failures. `data.assessment` is `no_local_blockers` or `blocked`;
`outcome` is respectively `success` or `failure`, and the exit code is 0 or 1.
These describe local preflight only. They do not establish that installation,
lock replay, or script execution will succeed.

Status data contains `manifest_lock_agreement`, `workspace`, `installation`,
`recovery`, `recovery_guidance`, `registries`, `blockers`, `truncated`, `snapshot`,
`verification_depth`, `checks_not_performed`, and `capabilities`. Manifest/lock
agreement is `match`, `mismatch`, or `unknown`. Installation states are `absent`,
`managed`, or `unmanaged_or_invalid`. Recovery is `none_detected`, `pending`, or
`unknown`. Root digest and replay checks run whenever the manifest and lock parse, even
when registry configuration or workspace discovery fails. Root binding checks
require valid registry configuration; workspace checks also require discovery.
A confirmed mismatch remains conclusive when other checks cannot run.
Imported npm schema-8 locks use their own parser and root/workspace receipt
validators. Their registry routing, platform selection, and ancestor installation
layout checks are listed as unperformed.
The shared store is not inspected. Blockers contain stable `code`
and bounded `location` fields, using JSON pointers for dependency declarations.
A selected member does not limit full-workspace checks. Registry data includes
origins, credential variable names, and `credentials: "not_read"`. Each configured
route also identifies its configuration `layer` (`project` for project configuration)
and `source` (the declaring filename with terminal control characters escaped, or
null for configuration parsed from an in-memory document). Provenance is inspection
metadata and is excluded from preview's semantic registry routing identity.
Root peer dependencies remain declarations rather than installable specs; member
peer declarations still undergo compatibility checks. Recovery blocker locations
identify the primary or backup journal whose metadata was inspected.

`snapshot.consistency` is always `not_atomic`. Its `files` identify metadata
reads by digest, explicit absence/read failure, or directory/journal metadata.
Member manifest identities use the exact bounded bytes parsed during discovery,
so a later persistent replacement cannot become the baseline for stale parsed data.
`changes_detected` and each `changed_on_recheck` flag describe the second pass;
false does not prove that no concurrent change occurred. Workspace discovery
and other readers may observe different moments. Node_modules contents and
workspace links are not verified. Runner/platform assurance is not assessed.
The report lists these limits in `checks_not_performed` and exposes current
supported lock/spec forms in the experimental `capabilities` summary. Use
`tapid --json status --help` for arguments and defaults from Clap definitions.

The report includes at most 128 blockers, 128 workspace members, and 128
declarations from each dependency section. Discovery stops before parsing members
if it exceeds 128 candidate manifests or 4,096 expansion steps. Each pattern,
component, directory entry, literal path expansion, and candidate manifest probe
consumes a step; overlapping patterns count before deduplication. Exceeding either
limit reports `WORKSPACE_DISCOVERY_LIMIT` and marks the inspection truncated.
Workspace discovery and planning bound root/member manifest reads to 32 MiB
before parsing or hashing. File digests are bounded to 32 MiB, registry configuration to 64 KiB,
and marker reads to 1 KiB. A truncated inspection exits 1. Display locations are
sanitized and bounded to 4 KiB; they are descriptive and must not be used for
automatic recovery. Use the envelope's lossless `project_path` to identify the
project. Status never advises deleting recovery data or bypassing verification.

Status blocker codes are `MANIFEST_INVALID_OR_MISSING`, `WORKSPACE_INVALID`, `WORKSPACE_DISCOVERY_LIMIT`,
`WORKSPACE_SELECTION_INVALID`, `UNSUPPORTED_SPEC`, `UNSUPPORTED_OVERRIDE`,
`MANIFEST_COMPATIBILITY_BLOCKER`, `WORKSPACE_COMPATIBILITY_BLOCKER`,
`REGISTRY_CONFIGURATION_INVALID`, `LOCKFILE_MISSING`, `LOCKFILE_UNREADABLE`,
`LOCKFILE_INVALID`, `LOCK_MANIFEST_MISMATCH`, `LOCK_WORKSPACE_MISMATCH`,
`LOCK_ROOT_BINDINGS_INVALID`, `LOCK_REPLAY_UNSUPPORTED`, `INSTALLATION_NOT_MANAGED`,
`RECOVERY_REQUIRED`, `RECOVERY_STATE_INVALID`, and `SNAPSHOT_CHANGED`.
`INSPECTION_TRUNCATED` appears in the envelope's errors when entry or display
limits prevent a complete report. A lock can agree with manifests and still
have a replay blocker, such as a legacy schema requiring regeneration or
locally computed registry integrity. Status reports those separately.

## Standalone tools

Tool commands use operation `tool`. Their `changes.files` and `changes.paths` report affected or recovery paths under the tools root: `catalog.json`, `pending.json`, `committed`, `bin`, and `environments`. The `data.tools` array is sorted by npm package name and contains `name`, `version`, `requirement`, `environment`, `lockfile`, and a sorted `commands` array of `name`/`target` mappings. Targets are relative to the recorded environment. `data.bin_dir` identifies the PATH directory. Text follows the same bounded/sanitized presentation rules as other results.

Mutations report committed state only after the global command decision. Retained in-use environments or cleanup failures produce `committed_cleanup_pending`; inconsistent pending publication produces recovery failure instead of overwriting unknown files. Listing may settle a previous transaction or clean inactive owned environments. Installed package scripts are never run by these operations. Execution of an installed command is a normal user process and has no sandbox receipt.

## Historical candidate exclusions

Successful dependency operations that resolve registry metadata, including resolved
dry runs, include nonempty `data.candidate_exclusions`. Ordinary human success
output stays quiet. Replay and lock-only previews have no newly observed exclusions
and omit this field. Terminal missing-candidate, conflicting-requirement, and
all-excluded metadata errors include `errors[].context.candidate_exclusions` for
only the affected exact registry origin and package. Human errors summarize the
same evidence. Excluded versions remain absent from `context.available`.

The summary contains `total_packages`, `total_candidates`, `omitted_packages`,
`omitted_candidates`, `truncated`, and `packages`. Each package group contains
`source`, `name`, `total_candidates`, `omitted_candidates`, `truncated`,
`truncated_fields`, and `candidates`. Each candidate contains `version`,
`reason_code`, `truncated_fields`, and, when applicable, `dependency_category`.
The category is `dependency`, `peer_dependency`, `optional_dependency`, or
`optional_peer`. Stable reason codes are:

- `unsupported_dependency_name`
- `empty_dependency_requirement`
- `unsupported_dependency_requirement`
- `platform_mismatch`
- `undeclared_optional_peer`
- `unsupported_jsr_alias`

One primary reason follows normalization's first failure: platform compatibility,
required dependencies, peers, optional peer markers, optional dependencies, then
JSR alias restrictions. Dependency maps use canonical name order. Parsing and
candidate filtering remain fail closed. Rejected names, requirements, parser
explanations, and metadata URLs are never included in exclusion evidence.

Counts describe distinct observed rejected registry/package/version identities,
not all registry history or versions that would otherwise satisfy the graph.
Summary omissions include candidates in omitted groups; group omissions count
only unsampled candidates within that group. Repeated records do not inflate
counts. A truncated sample is not exhaustive history.

Collection retains at most 16 package groups, eight candidates per group, and
100 candidates overall. Terminal evidence independently retains up to eight
candidates for the affected package, even if global sampling would omit it.
Selection and ordering use full canonical source/name/version strings before
sanitizing or shortening display fields; version ordering here is lexical, not
SemVer precedence. Display fields are limited to 256 UTF-8 bytes. Local
`truncated_fields` lists identify shortened identity fields; the envelope also
records their full JSON pointers. `truncated` covers omissions and shortened
fields. Human exclusion summaries use at most 2,048 UTF-8 bytes and report how
many candidates were omitted from that rendering, which may exceed the structured
sample's omissions. Collection scans the normalization cache at return points;
these bounds do not limit the existing metadata cache itself.

## Dependency lifecycle observations

Schema version 1 adds envelope fields on success and failure:

- `warning_details`: ordered typed warning occurrences. Lifecycle warnings contain
  `code`, `package` identity, and `hook` for skipped hooks. Legacy `warnings` remains
  the sorted, deduplicated array of codes. `warning_details_omitted` counts omitted
  occurrences; `warning_details_complete` is false after omission or scalar truncation.
- `lifecycle`: null outside installation processing, otherwise the observations
  established by that operation. Failure still has `data: null`.

`lifecycle.coverage` is `not_performed`, `incomplete`, or `complete`. Its optional
`reason` explains `before_discovery`, `imported_lock`, `processing_stopped`, or
`discovery_failed`. `selected_packages` is null until a validated selection is
available. `inspected_packages` counts attempted discovery, including failures.
Complete coverage means all selected packages were inspected; it says nothing
about hook success or application readiness.

`packages` is sorted by the original lock instance key. Each entry has `identity`,
`discovery` (`succeeded`, `failed`, or `not_performed`), `hooks`, and
`omitted_hooks`. Successful discovery with an empty hook array establishes no
hooks were discovered. Failed or unperformed discovery does not. Identity fields
are `key`, `name`, `version`, `source`, `archive_digest`, `peer_context`, and
`platform_context`; distinct registry, peer, and platform instances stay distinct.

Hooks are sorted by hook name. They contain no raw command or environment values:

| Field | Values |
| --- | --- |
| `authorization` | `not_evaluated`, `not_approved`, `exact_policy`, `invocation_override`, `rejected` |
| `disposition` | `not_performed`, `skipped`, `executed`, `reused`, `blocked` |
| `reason` | null or a stable reason from an existing check |

Reasons include `default_denial`, `processing_stopped`, `approval_rejected`,
`required_hook_unapproved`, `missing_declared_integrity`,
`authenticated_output_missing_or_mismatched`, `tool_verification_failed`,
`containment_unavailable`, and `execution_or_output_failed`. Missing and mismatched
cached output intentionally share one classification. Unchecked prerequisites
remain unknown. Invocation authorization is not exact checked-in approval or
cache eligibility.

`hooks_executed`, `generated_outputs_reused`, and `hooks_skipped` count observed
outcomes independently of detail limits. Execution is recorded after successful
execution and output validation. Reuse requires authenticated acceptance and
required staging. These facts survive later failures or rollback; only
`changes.state` establishes whether dependency files were committed. Neither a
successful install nor any lifecycle observation establishes build, test, or
runtime usability.

Each structured collection retains at most 1,024 records while processing;
`omitted_packages`, per-package `omitted_hooks`, and `warning_details_omitted`
report exact omissions. Counters and installation checks continue after a limit.
`details_complete` is independent of inspection coverage and becomes false after
omission or scalar truncation. Scalars use the existing sanitized 4,096-byte
limit and JSON-pointer `truncated_fields`. Display identities may be shortened;
never use shortened values as equality keys or approval evidence. Reports exclude
raw scripts, child output, attestations, keys, and environment values.

For example, these selected fields describe one default-denied hook after files
were committed (other identity and report fields are omitted here):

```json
{
  "outcome": "success",
  "changes": {"state": "committed"},
  "warnings": ["DEPENDENCY_LIFECYCLE_HOOK_SKIPPED"],
  "lifecycle": {
    "coverage": "complete",
    "hooks_executed": 0,
    "generated_outputs_reused": 0,
    "hooks_skipped": 1,
    "packages": [{
      "discovery": "succeeded",
      "hooks": [{"hook": "install", "authorization": "not_approved", "disposition": "skipped", "reason": "default_denial"}]
    }]
  }
}
```

A frozen replay with an exact approval but unavailable authenticated output instead
has `outcome: "failure"`, `data: null`, incomplete coverage, and a hook with
`authorization: "exact_policy"`, `disposition: "blocked"`, and
`reason: "authenticated_output_missing_or_mismatched"`. Earlier observations stay
in the report, while `changes.state` describes preservation or rollback.
