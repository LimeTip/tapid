# JSON command results

Use `tapid --json install` or `tapid install --json` to receive one newline-terminated JSON object on stdout. Supported commands also leave stderr empty, including on handled failures. Tapid does not emit progress, prompts, ANSI sequences, or registry error prose in this mode. An OS termination or a broken output pipe can prevent delivery of the result.

## Coverage

| Command | Global `--json` |
| --- | --- |
| `install`, alias `i` | Install or replay result |
| `add`, `remove`, `update`, `prune` | Dependency operation result |
| `outdated` | Sorted direct dependency entries, with partial metadata outcomes |
| `explain` | Exact package evidence under `data`; registry metadata or a local snapshot, with optional artifact byte verification |
| `why` | Dependency explanation paths under `data.paths`; a confirmed empty match returns `data.not_found: true` and exit 1, while a bounded empty search is partial and exits 0 |
| `cache`, `cache info`, `cache clean`, `clean` | Package cache inspection, preview, or cleanup result |
| `ci`, `init`, `manifest`, `lock`, `license`, `upgrade` | `JSON_UNSUPPORTED_COMMAND`, exit 1, before execution |
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
| `changes.files` | Sorted, deduplicated display paths for affected project outputs |
| `changes.paths` | Sorted, deduplicated lossless native path records for affected project outputs |
| `truncated_fields` | Sorted JSON pointers to display or metadata scalars shortened by the byte limit |
| `warnings` | Sorted, deduplicated warning codes |
| `errors` | Error objects with a stable `code`; operational errors also have `phase` of `operation` or `recovery`, and bounded typed `context` when available |
| `retry` | null or typed retry advice |
| `data` | Command-specific object, or null on failure |

Help results contain `data.text`, the selected command's help text with no ANSI formatting. For example, `tapid --json help run` returns run help without launching a child. Both short and long help forms preserve their usual content. Version results contain `data.name` of `tapid` and `data.version`, the executable's package version. These informational results use `outcome: "success"`, empty errors and warnings, null project and retry, and unchanged state with no affected files. They do not read project files. Help text is presentation content, not a stable command-discovery schema; its wording and layout may change within schema version 1.

`explain` successful `data` contains `package`, `version`, `registry`, `source`, `metadata_fetched_at`, `metadata_read_at`, `source_timestamp`, `source_timestamp_status`, `published_at`, `freshness`, `artifact`, `registry_integrity`, `byte_integrity`, `registry_signatures`, `provenance`, `publisher_identity`, `vulnerabilities`, `malware_analysis`, `human_review`, and `limitations`; shortened display values are listed in the envelope's `truncated_fields`. The nested fields are `registry_integrity.status/value`, `byte_integrity.status/actual/scope/checked_at`, `registry_signatures.status/count`, `provenance.status/reference/fetched`, `publisher_identity.status`, `vulnerabilities.status/reason`, `malware_analysis.status`, and `human_review.status`. Reported claims are not treated as verified evidence, and freshness is `unknown`. The serializer does not emit `data.not_found`: an operational error, including unavailable or invalid requested metadata, uses the standard `failure` envelope with `data: null`, a typed `errors` entry, and exit status 1. With `--artifact-file`, a byte mismatch is a failure result with populated evidence data and `INTEGRITY_MISMATCH`; a match remains `success`. `--registry-metadata` records a local snapshot source rather than contacting the registry.

`why` data contains `package`, deterministic `paths`, `truncated`, `not_found`, and `graph_warnings`. Each path contains `steps` with package identity, incoming dependency name and kind, and source kind (`registry`, `workspace`, or `copied`). A confirmed missing match is an unchanged successful operation envelope with `data.not_found: true`, but the command exits 1 to preserve its CLI no-match contract. If traversal reaches a safety bound before finding a path, the envelope outcome is `partial`, `data.truncated` is true, `data.not_found` is false, and the command exits 0. Operational errors use the standard `failure` envelope and typed `errors` entry.

## Package cache results

`cache`, `cache info`, `cache clean`, and `clean` use the version 1 envelope. The operation is `cache` for its subcommands and `clean` for the shortcut. `project` is null and `changes.files` and `changes.paths` are empty because maintenance does not change project outputs. `data.scope` is `published_package_data`; `data.store` is bounded display text and `data.store_path` uses the existing lossless native-path encoding. `data.action` is `info`, `preview`, or `clean`. The `data.summary` object contains `artifacts` and `trees`, each with `entries` and logical file `bytes`, plus `preserved_entries` for unrecognized entries inside those namespaces. Staging and upgrade-recovery caches are excluded. The version 1 `bytes` field names are unchanged: they count file content lengths, not allocated disk space or predicted reclaimed disk space. Human output labels the same values as logical bytes.

Inspection and previews report `changes.state: "unchanged"`. Successful eviction exits 0 and reports `committed` when entries were removed, or `unchanged` for an empty deletion set. `CACHE_BUSY` exits 1 with `unchanged` state and `retry: "after_contention"`. Other failures before eviction exit 1 with `unchanged` state and `CACHE_MAINTENANCE_FAILED`. A deletion or directory-sync failure exits 1 and reports `committed_cleanup_pending` because some cached data may have been removed; this state concerns cache eviction, not a project transaction. Cleanup has stopped, and no background cleanup is scheduled. Inspect the cache or retry after correcting the filesystem error. The state does not promise that removal or synchronization will finish automatically. Failed results have `data.summary: null`. Cache-location failures, including explicit roots that are files or symlinks, use `CACHE_PATH_INVALID`. All cache operational errors include `phase: "operation"`. An empty explicit `--store-dir` is rejected during argument parsing as `ARGUMENT_INVALID`, exit 2.

Install and mutating lifecycle data contains `package_count` and `replayed`. Graphs, artifact URLs, raw metadata, credentials, source error chains, and uncontrolled diagnostic messages are excluded. There is no graph selection in version 1.

Outdated data contains `entries`, `total_entries`, and `truncated`. Each entry contains `identity`, `kind`, `declared`, `locked`, `newest_compatible`, `newest_available`, and `error`. Unknown versions are null. The error is null or an object with a typed code. Entries sort by identity and dependency kind. Metadata failure in any entry makes the overall outcome `partial`, even if that entry falls beyond the output limit. Partial outdated reports retain exit 0; callers must inspect `outcome` and entry errors.

Each entry also contains `compatible_impact` and `available_impact`. Both contain `version_change`, `lockfile`, and `manifest`. Version change is `unchanged`, `major`, `minor`, `patch`, `prerelease`, `downgrade`, `unlocked`, or `unknown`, comparing the locked version with the selected metadata version. Major/minor/patch take precedence over prerelease differences on an upgrade. These are SemVer comparisons, not API compatibility claims. Lockfile and manifest impact are `changed`, `unchanged`, or `unknown`. Lockfile impact compares the direct version pin if the shown version is selected; it does not predict transitive changes or resolution success. Manifest impact indicates whether the shown version requires changing the declared range, preserving npm alias and workspace range semantics. Peer declarations do not select direct lockfile pins, so their lockfile impact is `unchanged` even when registry metadata is unavailable. A provider installed through another dependency section does not change this rule; that section reports its own pin impact. Missing metadata otherwise yields unknown impact. `outdated --offline` without a fixture returns `REGISTRY_METADATA_UNAVAILABLE` for registry dependencies and a partial result, with locked versions retained and unchecked versions null. Local workspace versions remain available. Pending transactions return failure with `RECOVERY_FAILED`, `recovery_required`, and `recover_first`, without attempting recovery. An unchanged pin does not establish an unchanged whole lockfile after `update`, which re-resolves the full graph.

Default output includes at most 100 outdated entries. Use `tapid --json outdated --json-limit 250` to select a larger limit, or `--json-limit 0` to retrieve every entry. This option requires `--json`, accepts a nonnegative integer, and affects only result rendering. It does not change resolution or project state. Each display path or metadata scalar is limited to 4096 UTF-8 bytes, removes control characters, and redacts HTTP URL user information, query values, and fragments. Scalar truncation can shorten a displayed name or requirement. Every shortened scalar is identified by a pointer in `truncated_fields`, such as `/data/entries/0/declared` or `/project`. These fields are display data, not identifiers for automatic mutation. Lossless path records are described below. Transaction paths describe only `package.json`, `tapid.lock`, and `node_modules`; shared-store effects are represented by state. Results omit timings and random transaction identifiers. File ordering, warning ordering, and entry ordering are deterministic for the same operation data.

For filesystem inspection and recovery, use `project_path` and `changes.paths`. Each record contains `encoding` and `value`. Encoding `utf8` preserves the exact path string, including characters represented by JSON escapes. Encoding `unix_bytes_base64` preserves native Unix path bytes; `windows_utf16le_base64` preserves Windows UTF-16 code units as little-endian bytes. Decode base64 records to the host's native path type without replacing invalid Unicode. The display fields can redact, remove controls, or truncate characters, and different paths can therefore share one display string. Lossless records preserve each distinct native path and never return a shortened path.

Each lossless record permits at most 128 KiB of UTF-8 or native path bytes before base64 encoding. A record beyond that capacity has null `value` and `unavailable: "capacity_exceeded"`. Consumers must stop automatic recovery if a required path is unavailable or its encoding is unknown. The lossless values are filesystem data; render their control characters safely when displaying them. They contain no raw ANSI bytes in the serialized JSON stream.

Package text is untrusted data. Never execute it or treat it as recovery advice. Only `changes.state` and `retry` describe the transaction decision. `after_correction` means correct the failure before retrying. `after_contention` means wait for the competing operation. `do_not_repeat` means the change committed; retrying may repeat a mutation. `recover_first` means inspect the affected paths and recover the interrupted operation first. Handled operational failures exit 1, including failures after a durable commit.

## Codes and compatibility

### Failure context

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

Operational codes are `INVALID_REQUEST`, `PROJECT_UNAVAILABLE`, `MANIFEST_INVALID`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, `LOCK_MANIFEST_MISMATCH`, `REGISTRY_CONFIGURATION_INVALID`, `REGISTRY_AUTH_MISSING`, `REGISTRY_METADATA_INVALID`, `REGISTRY_METADATA_UNAVAILABLE`, `REGISTRY_TRANSPORT_FAILED`, `RESOLUTION_FAILED`, `PEER_DEPENDENCY_UNSATISFIED`, `INTEGRITY_MISMATCH`, `ARCHIVE_INVALID`, `STORE_FAILED`, `STORE_CONTENT_UNAVAILABLE`, `STORE_BUSY`, `PROJECT_BUSY`, `MATERIALIZATION_FAILED`, `TRANSACTION_FAILED`, `RECOVERY_FAILED`, and `INVALID_DATA`. Protocol codes are `ARGUMENT_INVALID` and `JSON_UNSUPPORTED_COMMAND`. Warning codes are `UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED` and `PREVIOUS_TRANSACTION_RECOVERED`.

Within schema version 1, existing field types, code meanings, transaction states, and exit semantics remain stable. Additive fields, new error or warning codes, and new command coverage are compatible. Consumers must ignore unknown fields and handle unknown codes conservatively. Removing or renaming fields or changing their meaning requires a new schema version. Schema versioning is independent of Tapid's package version.

## Run child output and receipts

`tapid run` preserves the child's stdout, stderr, and exit code. Its existing `--receipt-json` option appends a receipt to stderr after child output. That stream may contain arbitrary child bytes, so it is not a single-object JSON channel. Global `--json` rejects `run` before launch to preserve the package-result stdout guarantee. A future machine result channel for run must be a separately selected file or descriptor, isolated from both child streams, and retain the child exit code. This release does not add that channel.

When invocation permission flags are used, the schema-version-1 receipt adds `policy_inputs`: `source` is `script-profile`, `configured-defaults` or `invocation-baseline`; `configuration` contains the configured ceiling or null; `invocation` contains the supplied permission lists/booleans; `effective` describes the selected project policy and preserved assurance/limits. Environment entries are names only. Omitted lists inherit configured grants, so an empty invocation list does not itself mean the effective grant list is empty. Backend runtime additions remain in `effective_filesystem`, and checked enforcement remains in the established evidence. Profile-only receipts retain their existing shape. Consumers must accept additive fields; this metadata does not make untrusted child streams a trusted machine-result channel.
