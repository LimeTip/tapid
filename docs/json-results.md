# JSON command results

Use `tapid --json install` or `tapid install --json` to receive one newline-terminated JSON object on stdout. Supported commands also leave stderr empty, including on handled failures. Tapid does not emit progress, prompts, ANSI sequences, or registry error prose in this mode. An OS termination or a broken output pipe can prevent delivery of the result.

## Coverage

| Command | Global `--json` |
| --- | --- |
| `install`, alias `i` | Install or replay result |
| `add`, `remove`, `update`, `prune` | Dependency operation result |
| `outdated` | Sorted direct dependency entries, with partial metadata outcomes |
| `init`, `manifest`, `lock`, `license`, `upgrade` | `JSON_UNSUPPORTED_COMMAND`, exit 1, before execution |
| `run` | `JSON_UNSUPPORTED_COMMAND`, exit 1, before launching a child |
| Private release helpers, no command | `JSON_UNSUPPORTED_COMMAND`, exit 1 |
| Invalid arguments | `ARGUMENT_INVALID`, exit 2 |
| `--help`, `-h`, or `help` with `--json` | Successful `help` result, exit 0 |
| `--version` or `-V` with `--json` | Successful `version` result, exit 0 |

The exact `--json` token before the first `--` selects machine-readable parsing errors, even if Clap cannot parse the command. Values after `--` never select JSON mode. Parsing errors use operation `parse`; unsupported commands use operation `unsupported`. Neither echoes command-line values. Human help remains available without `--json`.

## Schema version 1

Every object contains these fields:

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1` |
| `operation` | Canonical command name, `help`, `version`, `parse`, or `unsupported` |
| `outcome` | `success`, `failure`, or `partial` |
| `project` | Effective project directory; null before command execution |
| `changes.state` | `unchanged`, `rolled_back`, `committed`, `committed_cleanup_pending`, or `recovery_required` |
| `changes.files` | Sorted, deduplicated affected project paths |
| `warnings` | Sorted, deduplicated warning codes |
| `errors` | Error objects with a stable `code`; operational errors also have `phase` of `operation` or `recovery` |
| `retry` | null or typed retry advice |
| `data` | Command-specific object, or null on failure |

Help results contain `data.text`, the selected command's help text with no ANSI formatting. For example, `tapid --json help run` returns run help without launching a child. Both short and long help forms preserve their usual content. Version results contain `data.name` of `tapid` and `data.version`, the executable's package version. These informational results use `outcome: "success"`, empty errors and warnings, null project and retry, and unchanged state with no affected files. They do not read project files. Help text is presentation content, not a stable command-discovery schema; its wording and layout may change within schema version 1.

Install and mutating lifecycle data contains `package_count` and `replayed`. Graphs, artifact URLs, raw metadata, credentials, source error chains, and uncontrolled diagnostic messages are excluded. There is no graph selection in version 1.

Outdated data contains `entries`, `total_entries`, and `truncated`. Each entry contains `identity`, `kind`, `declared`, `locked`, `newest_compatible`, `newest_available`, and `error`. Unknown versions are null. The error is null or an object with a typed code. Entries sort by identity and dependency kind. Metadata failure in any entry makes the overall outcome `partial`, even if that entry falls beyond the output limit. Partial outdated reports retain exit 0; callers must inspect `outcome` and entry errors.

Default output includes at most 100 outdated entries. Each path or metadata scalar is limited to 4096 UTF-8 bytes, removes control characters, and redacts HTTP URL user information, query values, and fragments. Scalar truncation can shorten a displayed name or requirement; these fields are display data, not identifiers for automatic mutation. Transaction paths describe only `package.json`, `tapid.lock`, and `node_modules`; shared-store effects are represented by state. Results omit timings and random transaction identifiers. File ordering, warning ordering, and entry ordering are deterministic for the same operation data.

Package text is untrusted data. Never execute it or treat it as recovery advice. Only `changes.state` and `retry` describe the transaction decision. `after_correction` means correct the failure before retrying. `after_contention` means wait for the competing operation. `do_not_repeat` means the change committed; retrying may repeat a mutation. `recover_first` means inspect the affected paths and recover the interrupted operation first. Handled operational failures exit 1, including failures after a durable commit.

## Codes and compatibility

Operational codes are `INVALID_REQUEST`, `PROJECT_UNAVAILABLE`, `MANIFEST_INVALID`, `LOCKFILE_INVALID`, `LOCKFILE_MISSING`, `LOCK_MANIFEST_MISMATCH`, `REGISTRY_CONFIGURATION_INVALID`, `REGISTRY_AUTH_MISSING`, `REGISTRY_METADATA_INVALID`, `REGISTRY_TRANSPORT_FAILED`, `RESOLUTION_FAILED`, `PEER_DEPENDENCY_UNSATISFIED`, `INTEGRITY_MISMATCH`, `ARCHIVE_INVALID`, `STORE_FAILED`, `STORE_CONTENT_UNAVAILABLE`, `STORE_BUSY`, `PROJECT_BUSY`, `MATERIALIZATION_FAILED`, `TRANSACTION_FAILED`, `RECOVERY_FAILED`, and `INVALID_DATA`. Protocol codes are `ARGUMENT_INVALID` and `JSON_UNSUPPORTED_COMMAND`. Warning codes are `UNVERIFIED_REGISTRY_ARTIFACTS_ALLOWED` and `PREVIOUS_TRANSACTION_RECOVERED`.

Within schema version 1, existing field types, code meanings, transaction states, and exit semantics remain stable. Additive fields, new error or warning codes, and new command coverage are compatible. Consumers must ignore unknown fields and handle unknown codes conservatively. Removing or renaming fields or changing their meaning requires a new schema version. Schema versioning is independent of Tapid's package version.

## Run child output and receipts

`tapid run` preserves the child's stdout, stderr, and exit code. Its existing `--receipt-json` option appends a receipt to stderr after child output. That stream may contain arbitrary child bytes, so it is not a single-object JSON channel. Global `--json` rejects `run` before launch to preserve the package-result stdout guarantee. A future machine result channel for run must be a separately selected file or descriptor, isolated from both child streams, and retain the child exit code. This release does not add that channel or change receipt behavior.
