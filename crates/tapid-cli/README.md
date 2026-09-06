# tapid

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid)](https://crates.io/crates/tapid)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid)](https://crates.io/crates/tapid)
[![Docs.rs](https://docs.rs/tapid/badge.svg)](https://docs.rs/tapid)
[![License](https://img.shields.io/crates/l/tapid)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

The `tapid` command-line client for the Tapid JavaScript and TypeScript package manager, written in Rust. It provides deterministic installation and lockfile replay, verified package storage, Node-compatible linking, and explicit root-script execution.

## Install Tapid

**macOS and Linux**

```bash
curl -fsSL https://tapid.dev/install.sh | bash
```

**Windows PowerShell**

```powershell
iwr -useb https://tapid.dev/install.ps1 | iex
```

The installers select the latest published release from the immutable GitHub release assets published by `LimeTip/tapid`, verify the platform archive against its `SHA256SUMS` entry, and install Tapid without administrator privileges. Alternate repositories must provide their own equivalent release controls. See the repository [installation details](https://github.com/LimeTip/tapid#installation-details) for version selection, contributor source builds, and uninstall instructions.

## Commands

```text
tapid init [PATH]
tapid manifest validate [PATH]
tapid lock verify
tapid install [OPTIONS]
tapid run <SCRIPT> [--node-runtime <PATH>] [--receipt-json] [-- <ARGS>...]
```

`tapid init` creates a private `package.json` without overwriting an existing file. Manifest and lock commands validate the selected files. Paths default to the current directory and `package.json` where applicable.

## Install packages

The supported package installation paths are the live npm path, validated lockfile replay, and the local registry fixture:

```text
tapid install --project-dir ./example
tapid install --offline --frozen --project-dir ./example
tapid install --registry-fixture ./fixture.json --project-dir ./example
```

The fixture option is for local tests and air-gapped development. It is not a registry authentication or production mirror feature. The live npm path resolves supported transitive ranges, requires registry-declared SHA-512 integrity by default, selects compatible optional packages for the current OS/CPU/libc target, verifies extracted trees, writes schema 6 locks, and stores trees in the platform cache outside the consumer project. `--allow-unverified-registry-artifacts` is an explicit online-only compatibility exception and emits a warning.

## Offline and frozen

```text
tapid install --offline --project-dir ./example
tapid install --frozen --project-dir ./example
tapid install --offline --frozen --store-dir ./verified-store
```

Both flags require `tapid.lock` and all referenced verified trees. Replay validates the root manifest digest, exact package identities, tree digests, regular `.tapid-tree` markers, and available store content before staging. It performs no network resolution or archive download. Activation replaces managed `node_modules` atomically; failed validation or staging does not intentionally activate partial output.

`--frozen` currently selects the same no-network replay path as `--offline`. It does not yet implement the complete npm frozen-lockfile policy.

## Run and `.bin`

```text
tapid run init
tapid run dev -- --hostname 127.0.0.1 --port 3001
tapid run --project-dir ./example test -- --runInBand
tapid run dev --node-runtime /absolute/path/to/node -- --host 127.0.0.1
```

Values after the first `--` are forwarded in order to the selected script; the separator is not forwarded and those values are not parsed as Tapid options. Missing scripts fail with exit code `1`. Clap parsing errors use exit code `2`.

The command requires checked-in `tapid.toml` and an exact `[run.scripts.<name>]` profile; `[run.defaults]` is merged only into that explicitly selected profile. In the configuration schema, `assurance = "restricted"` explicitly requests ADR 0005 **Restricted** execution: requested filesystem/network authority, explicit environment/PATH and descriptor hygiene, and descendant propagation must be established before spawn. Restricted provides no cleanup guarantee, although a backend may report best-effort cleanup it actually attempted or observed. Omitting `assurance` preserves the legacy-safe **ManagedTree** contract, which additionally requires race-free descendant ownership, complete cleanup/kill, and configured tree-wide timeout, output, process, and memory semantics. The schema and experimental macOS Restricted backend are implemented; unsupported required dimensions fail before the shell starts.

The command constructs a minimal environment rather than preserving inherited variables: `PATH` is reserved and cannot be allowlisted, while other declared names are retrieved individually from the caller only when present. Windows environment-name matching is case-insensitive and case-equivalent allowlist duplicates are rejected. The current `network` field is boolean: `true` grants unrestricted networking. `--hostname`, `--port`, and other forwarded application arguments do not constrain authority; declared listen/connect scopes remain future work.

`--node-runtime` is optional. Without it, Tapid examines at most 256 absolute entries from the invoking host's `PATH`, ignores empty or relative entries that could resolve through an untrusted working directory, and selects the first canonical, regular executable named `node` (`node.exe` on Windows). With it, Tapid validates that exact path instead. Host `PATH` is used only for this trusted preflight lookup and is never copied or appended to the child environment. On macOS the child receives a new `PATH` ordered as a private directory containing only a byte-verified `node` snapshot on a distinct inode, canonical project `node_modules/.bin`, then the runtime's canonical directory. Project commands win over ordinary runtime tools, while project-controlled `node` cannot shadow the verified executable. A selected runtime under project write authority is rejected. The runner creates a fresh unpredictable owner-only directory outside project write authority for each attempt and never reuses retained directories. It copies from a held runtime object, verifies exact bytes and executable metadata, and makes Seatbelt deny hard links and writes to the private snapshot while project writes remain allowed. It retains the snapshot whenever subprocess-enabled targets may have executed, including post-launch errors, because Restricted cleanup cannot prove descendants have exited. It removes the directory on pre-spawn and proven failed-exec paths. Successful `subprocess=false` executions remove it only after root exit, with Seatbelt denying process-fork. Private device/inode, mode, size, and link count are checked immediately before launch. Host writes and races after the final check remain residual risks. Other platforms retain their request construction but cannot execute a native sandbox. The project `.bin` must be a real directory strictly beneath the canonical project root; pre-existing symlinks and canonical escapes are rejected. On Unix the runtime must have an executable bit.

On Unix, package scripts use `/bin/sh -c <script> tapid-script <forwarded-args...>` with `"$@"` boundaries and preserve native argument bytes. On Windows, the request uses `cmd.exe /D /S /C`, npm-compatible escaping for forwarded arguments, and a verbatim adapter boundary; CR/LF arguments are rejected. Configuration input is bounded before parsing. Unknown configuration, invalid project-relative paths, invalid environment names, reserved `PATH`, invalid runtimes, unsupported combinations, and unavailable guarantees fail before the shell starts. Project configuration cannot turn containment off. A noisy `--no-sandbox` outcome is planned only for trusted interactive projects, with no enforcement receipt, no silent fallback, and unattended rejection unless separately authorized; this CLI does not implement it.

The CLI uses `tapid-runner::ExecutionRequest` and checked execution exclusively for normal root scripts. Native backends own live child stdout/stderr streaming; the CLI does not replay captured bytes after completion. The checked launch receipt matches the exact requested enforcement and identifies each dimension's request, mechanism, assurance level, enforcement state, scope, and limitation; `CanonicalPath` is not `NativeObject`, and limit terminations remain distinct. Completion evidence describes lifecycle and cleanup results only and must not re-confirm launch-only authority. For Restricted, it must distinguish no cleanup guarantee from best-effort cleanup actually attempted or observed. Native macOS Restricted execution is evidence-gated. ManagedTree, all configured resource limits, and non-macOS native execution fail closed as `unsupported-containment` before target spawn and issue no receipt. The Windows verbatim command boundary and all native backend behavior still require execution on their target operating systems.

On successful contained execution, stderr receives a receipt after streamed child output. Human output starts with `sandbox receipt:` and pretty-printed JSON. `--receipt-json` emits the same data as one compact JSON line, preceded by a newline even when child stderr did not end with one. Child stdout and stderr are streamed once and are not embedded or replayed in the receipt. On success the final stderr line in JSON mode is the receipt; launch failures emit an error and no receipt. Child output is untrusted and may resemble receipt text, so consumers must also check the CLI completion and final record.

Receipt schema version 1 contains `assurance`, `backend` with name/version/deprecation, `requested`, `declared`, `observed`, and `enforced` boolean dimension maps; `declared_evidence`, `observed_evidence`, and `established_evidence` arrays with dimension/scope/mechanism/limitations; `effective_filesystem`; `configured_limits`; `termination`; and `completion` with confirmed dimensions, evidence, and cleanup confidence. Each filesystem entry contains display `path`, lossless `native_path` with `encoding` and integer `units`, `access`, `kind`, `source`, and `binding`. Unix encoding is `unix-bytes`; Windows uses `windows-utf16`. Enum values use their Rust names. Project and runtime entries remain separate via `ProjectPolicy` and `BackendRuntime`. Limits use their configuration names and `null` for unconfigured values. No macOS configured limit is silently accepted. See [runner semantics](../tapid-runner/README.md) for exact directory data, global metadata, explicit devices, sampled probes, and cleanup limitations.

Install derives executable shims from verified package `bin` metadata. Unix uses symlinks. Windows writes `.cmd` and PowerShell wrappers. The planner rejects malformed metadata, absolute or traversal targets, symlink and special-file targets, collisions, and unsupported platforms. Root scripts remain arbitrary code and can use every explicitly granted capability.

## Lifecycle policy and limitations

- Dependency lifecycle scripts are disabled during every install path.
- Root scripts run only after the explicit `tapid run` command.
- Root-script execution is wired to fail-closed preflight. macOS 26 has an experimental Restricted backend using deprecated/private native Seatbelt APIs; ManagedTree, resource-limit profiles, and Linux/Windows native backends remain unavailable.
- Full npm semver, aliases, tags, git/file/workspace specs, peer semantics, workspaces, and complete optional-dependency and lockfile compatibility are not implemented.
- `add`, `remove`, `update`, `prune`, script approval, private-registry authentication, and package publishing are outside this slice.
- JSR installation remains fail-closed unless metadata provides both an HTTPS npm tarball URL and a valid SHA-512 SRI value. Live JSR integrity behavior is unsupported and unverified.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform does not prove behavior on another.

The macOS runner requires the binary's early private-launcher initializer. `sandbox-exec` launches that same executable under a parameter-bound profile. A fixed-size nonce/version READY/GO protocol, kqueue NOTE_EXEC, and CLOEXEC status EOF establish launch; target exit codes and stderr never do. Receipt `executable_resolution` reports exact Unix byte arrays for PATH and its ordered entries, `caller_path_inherited = false`, the distinct byte-verified private Node snapshot identity and validation timing, and cleanup observation. This reserves bare `node` and env-shebang resolution, not explicit paths to project executables.

For retained bindings, `executable_resolution.reserved_node.cleanup_observed` is `false`, and `limitations` explicitly describes retention. This field reports removal of the private snapshot, independently of best-effort process-group cleanup in `completion`. Retained directories and snapshots consume temporary storage until OS cleanup or host removal after every descendant exits. Tapid does not schedule deletion or reuse them. OS or host removal while descendants survive ends reserved-node protection. Host writes or races after final validation remain outside Restricted containment; retention provides no ManagedTree ownership or cleanup guarantee.
