# JSON command results

Use `tapid --json install` or `tapid install --json` to receive one newline-terminated JSON object on stdout. Supported commands also leave stderr empty, including on handled failures. Tapid does not emit progress, prompts, ANSI sequences, or registry error prose in this mode. An OS termination or a broken output pipe can prevent delivery of the result.

## Coverage

| Command | Global `--json` |
| --- | --- |
| `install`, alias `i` | Install or replay result |
| `add`, `remove`, `update`, `prune` | Dependency operation result |
| `outdated` | Sorted direct dependency entries, with partial metadata outcomes |
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
| `errors` | Error objects with a stable `code`; operational errors also have `phase` of `operation` or `recovery` |
| `retry` | null or typed retry advice |
| `data` | Command-specific object, or null on failure |

Help results contain `data.text`, the selected command's help text with no ANSI formatting. For example, `tapid --json help run` returns run help without launching a child. Both short and long help forms preserve their usual content. Version results contain `data.name` of `tapid` and `data.version`, the executable's package version. These informational results use `outcome: "success"`, empty errors and warnings, null project and retry, and unchanged state with no affected files. They do not read project files. Help text is presentation content, not a stable command-discovery schema; its wording and layout may change within schema version 1.

Install and mutating lifecycle data contains `package_count` and `replayed`. Graphs, artifact URLs, raw metadata, credentials, source error chains, and uncontrolled diagnostic messages are excluded. There is no graph selection in version 1.

Outdated data contains `entries`, `total_entries`, and `truncated`. Each entry contains `identity`, `kind`, `declared`, `locked`, `newest_compatible`, `newest_available`, and `error`. Unknown versions are null. The error is null or an object with a typed code. Entries sort by identity and dependency kind. Metadata failure in any entry makes the overall outcome `partial`, even if that entry falls beyond the output limit. Partial outdated reports retain exit 0; callers must inspect `outcome` and entry errors.

Each entry also contains `compatible_impact` and `available_impact`. Both contain `version_change`, `lockfile`, and `manifest`. Version change is `unchanged`, `major`, `minor`, `patch`, `prerelease`, `downgrade`, `unlocked`, or `unknown`, comparing the locked version with the selected metadata version. Major/minor/patch take precedence over prerelease differences on an upgrade. These are SemVer comparisons, not API compatibility claims. Lockfile and manifest impact are `changed`, `unchanged`, or `unknown`. Lockfile impact compares the direct version pin if the shown version is selected; it does not predict transitive changes or resolution success. Manifest impact indicates whether the shown version requires changing the declared range, preserving npm alias and workspace range semantics. Peer declarations do not select direct lockfile pins, so their lockfile impact is `unchanged` even when registry metadata is unavailable. A provider installed through another dependency section does not change this rule; that section reports its own pin impact. Missing metadata otherwise yields unknown impact. `outdated --offline` without a fixture returns `REGISTRY_METADATA_UNAVAILABLE` for registry dependencies and a partial result, with locked versions retained and unchecked versions null. Local workspace versions remain available. Pending transactions return failure with `RECOVERY_FAILED`, `recovery_required`, and `recover_first`, without attempting recovery. An unchanged pin does not establish an unchanged whole lockfile after `update`, which re-resolves the full graph.

Default output includes at most 100 outdated entries. Use `tapid --json outdated --json-limit 250` to select a larger limit, or `--json-limit 0` to retrieve every entry. This option requires `--json`, accepts a nonnegative integer, and affects only result rendering. It does not change resolution or project state. Each display path or metadata scalar is limited to 4096 UTF-8 bytes, removes control characters, and redacts HTTP URL user information, query values, and fragments. Scalar truncation can shorten a displayed name or requirement. Every shortened scalar is identified by a pointer in `truncated_fields`, such as `/data/entries/0/declared` or `/project`. These fields are display data, not identifiers for automatic mutation. Lossless path records are described below. Transaction paths describe only `package.json`, `tapid.lock`, and `node_modules`; shared-store effects are represented by state. Results omit timings and random transaction identifiers. File ordering, warning ordering, and entry ordering are deterministic for the same operation data.

For filesystem inspection and recovery, use `project_path` and `changes.paths`. Each record contains `encoding` and `value`. Encoding `utf8` preserves the exact path string, including characters represented by JSON escapes. Encoding `unix_bytes_base64` preserves native Unix path bytes; `windows_utf16le_base64` preserves Windows UTF-16 code units as little-endian bytes. Decode base64 records to the host's native path type without replacing invalid Unicode. The display fields can redact, remove controls, or truncate characters, and different paths can therefore share one display string. Lossless records preserve each distinct native path and never return a shortened path.

Each lossless record permits at most 128 KiB of UTF-8 or native path bytes before base64 encoding. A record beyond that capacity has null `value` and `unavailable: "capacity_exceeded"`. Consumers must stop automatic recovery if a required path is unavailable or its encoding is unknown. The lossless values are filesystem data; render their control characters safely when displaying them. They contain no raw ANSI bytes in the serialized JSON stream.

Package text is untrusted data. Never execute it or treat it as recovery advice. Only `changes.state` and `retry` describe the transaction decision. `after_correction` means correct the failure before retrying. `after_contention` means wait for the competing operation. `do_not_repeat` means the change committed; retrying may repeat a mutation. `recover_first` means inspect the affected paths and recover the interrupted operation first. Handled operational failures exit 1, including failures after a durable commit.

## Codes and compatibility

Operational codes are `INVALID_REQUEST`, `PROJECT_UNAVAILABLE`, `MANIFEST_INVALID`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, `LOCK_MANIFEST_MISMATCH`, `REGISTRY_CONFIGURATION_INVALID`, `REGISTRY_AUTH_MISSING`, `REGISTRY_METADATA_INVALID`, `REGISTRY_METADATA_UNAVAILABLE`, `REGISTRY_TRANSPORT_FAILED`, `RESOLUTION_FAILED`, `PEER_DEPENDENCY_UNSATISFIED`, `INTEGRITY_MISMATCH`, `ARCHIVE_INVALID`, `STORE_FAILED`, `STORE_CONTENT_UNAVAILABLE`, `STORE_BUSY`, `PROJECT_BUSY`, `MATERIALIZATION_FAILED`, `TRANSACTION_FAILED`, `RECOVERY_FAILED`, and `INVALID_DATA`. Protocol codes are `ARGUMENT_INVALID` and `JSON_UNSUPPORTED_COMMAND`. Warning codes are `UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED` and `PREVIOUS_TRANSACTION_RECOVERED`.

Within schema version 1, existing field types, code meanings, transaction states, and exit semantics remain stable. Additive fields, new error or warning codes, and new command coverage are compatible. Consumers must ignore unknown fields and handle unknown codes conservatively. Removing or renaming fields or changing their meaning requires a new schema version. Schema versioning is independent of Tapid's package version.

## Run child output and receipts

`tapid run` preserves the child's stdout, stderr, and exit code. Its existing `--receipt-json` option appends a receipt to stderr after child output. That stream may contain arbitrary child bytes, so it is not a single-object JSON channel. Global `--json` rejects `run` before launch to preserve the package-result stdout guarantee. A future machine result channel for run must be a separately selected file or descriptor, isolated from both child streams, and retain the child exit code. This release does not add that channel or change receipt behavior.
