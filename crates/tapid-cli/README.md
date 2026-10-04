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

The next-release installers, expected for 0.0.11, select archives through `https://tapid.dev/releases/v1/latest.tsv`, check their recorded size and SHA-256, and install Tapid without administrator privileges. GitHub remains the initial archive host. Explicit versions through 0.0.10 use their historical GitHub archives and `SHA256SUMS` unless `TAPID_RELEASE_RECORD_URL` is supplied. The new public routes and installer copies require the coordinated release cutover; this source README does not establish their deployed state. See the repository [installation details](https://github.com/LimeTip/tapid#installation-details) for version selection, contributor source builds, and uninstall instructions.

## Commands

```text
tapid init [PATH]
tapid manifest validate [PATH]
tapid lock verify
tapid install [OPTIONS]
tapid upgrade [OPTIONS]
tapid run <SCRIPT> [--node-runtime <PATH>] [--receipt-json] [-- <ARGS>...]
```

`tapid init` creates a private `package.json` without overwriting an existing file. Manifest and lock commands validate the selected files. Paths default to the current directory and `package.json` where applicable.

`tapid i` is an alias for `tapid install`, including when adding a package. Use `tapid install --help` or `tapid help install` for installation help. Bare `help` and `install` package arguments are rejected before accessing the project to avoid accidental installs. To intentionally install a package with either name, use an explicit spec such as `help@1.0.0` or `npm:install`.

## Upgrade Tapid

Starting with 0.0.10, `tapid upgrade` discovers and installs the latest stable release. `tapid upgrade --dry-run` inspects the selected release without replacing the binary. Older clients can be upgraded by rerunning the public installer.

When the installed executable matches the verified release bytes, the command reports that Tapid is already up to date and leaves the executable unchanged. It still downloads and verifies the release before comparing. If network discovery fails and the command uses its recovery cache, it reports that the latest release could not be checked. Cached recovery is reported as a restore or an unchanged executable.

The next-release command, expected for 0.0.11, reads the same `tapid.dev` release record as the installers and verifies its `.sig` TrustEnvelope sidecar against the embedded production keyring before selecting an artifact. Use `--release-url` or `TAPID_RELEASE_RECORD_URL` to select another record address. It validates metadata, archive size, checksum, and archive contents before staging executable replacement. Invalid received metadata or mismatched downloads fail. Default discovery does not fall back to the GitHub API. An explicit injected keyring is reserved for controlled test seams; explicit `--endpoint` retains the historical signed-discovery protocol and is separate from `--release-url`.

## Install packages

The supported package installation paths are the live npm path, validated lockfile replay, and the local registry fixture:

```text
tapid install --project-dir ./example
tapid install --offline --frozen --project-dir ./example
tapid install --registry-fixture ./fixture.json --project-dir ./example
```

The fixture option is for local tests and air-gapped development. It is not a registry authentication or production mirror feature. The live npm path resolves supported transitive ranges, requires registry-declared SHA-512 integrity by default, selects compatible optional packages for the current OS/CPU/libc target, verifies extracted trees, writes schema 7 locks, and stores trees in the platform cache outside the consumer project. `--allow-unverified-registry-artifacts` is an explicit online-only compatibility exception and emits a warning.

## Private npm registry routing (development feature)

Registry routing is configured in the project-root `tapid.toml`. With no `[registries]` entries, plain npm package names continue to resolve from `https://registry.npmjs.org`. A matching scope overrides `default`; otherwise `default` applies, then the public npm registry is the fallback. `npm:` aliases use the same scope routing. `jsr:` packages retain their JSR identity and are not routed through npm settings.

```toml
[registries.default]
url = "https://npm-mirror.example"

[registries."@acme"]
url = "https://packages.acme.example"
token-env = "TAPID_ACME_NPM_TOKEN"
```

`url` must be a canonical HTTPS origin without a path, query, fragment, or embedded user information. `token-env` is the name of an environment variable, never the credential value. Tapid reads that variable only when resolving packages routed to that entry. Scope configuration takes precedence over the default entry, including its credential source; a scope without `token-env` does not inherit the default entry's token. If a selected private route requires a missing or empty token, installation fails closed without falling back to another registry. Without `token-env`, the selected origin is used without bearer authentication. Credentials are attached only to requests for the exact configured origin, and redirects to another origin are rejected.

The only supported credential provider is an environment variable selected by `token-env`. For local use, populate it through an operating-system secret manager or a protected shell environment; in CI, map the corresponding CI secret into the install job's environment. Do not put literal tokens in configuration, command arguments, scripts, or logs. Tapid does not implicitly read `.npmrc`, npm configuration variables, or home-directory credentials. Registry selection order is: exact package scope, then `[registries.default]`, then the public npm registry. Credential selection follows only the chosen entry and has no implicit cross-entry fallback. This initial feature does not implement credential helper or file providers.

Do not put tokens in `package.json`, `tapid.toml`, command-line arguments, or `tapid.lock`. Offline/frozen replay uses the registry identities already pinned in the lockfile and does not require credentials or contact a registry. Registry credentials are excluded from root-script environments even if a run policy tries to allowlist the corresponding variable. Private-registry support is under development and is not a production-support claim.

## Legacy registry identities

Locks containing noncanonical persisted registry origins (such as uppercase hosts
or explicit `:443`) fail closed before activation/store mutation in offline and
frozen modes. Preserve a separate verified backup of `tapid.lock`, then deliberately
run online `tapid install` and review changed versions, artifacts and edges. The
online path replaces the lock after re-resolution, not identity migration. See
[compatibility and recovery](https://github.com/LimeTip/tapid/blob/main/docs/compatibility.md#persisted-registry-identity-compatibility).

## Offline and frozen

```text
tapid install --offline --project-dir ./example
tapid install --frozen --project-dir ./example
tapid install --offline --frozen --store-dir ./verified-store
```

Both flags require `tapid.lock` and all referenced verified trees. Replay validates the root manifest digest, exact package identities, tree digests, regular `.tapid-tree` markers, and available store content before staging. It performs no network resolution or archive download. Activation replaces managed `node_modules` atomically; failed validation or staging does not intentionally activate partial output.

`--frozen` currently selects the same no-network replay path as `--offline`. It does not yet implement the complete npm frozen-lockfile policy.

## Experimental Node.js root-script runner and `.bin` handling

`tapid run` is a separate root-script launcher, not Tapid's package-management core and not a general JavaScript runtime selector. It currently launches Node.js scripts only. It cannot select Bun, Deno, or another runtime; use the runtime's own tooling for those projects. Its execution-containment feature is experimental and supported only on the documented platform/configuration combinations.

```text
tapid run init
tapid run dev -- --hostname 127.0.0.1 --port 3001
tapid run --project-dir ./example test -- --runInBand
tapid run dev --node-runtime /absolute/path/to/node -- --hostname 127.0.0.1 --port 3001
```

Values after the first `--` are forwarded in order to the selected script; the separator is not forwarded and those values are not parsed as Tapid options. Missing scripts fail with exit code `1`. Clap parsing errors use exit code `2`.

## Experimental root-script containment

`tapid run` still invokes Node.js; Tapid is not a JavaScript runtime. A script profile with `assurance = "restricted"` asks the platform backend to apply filesystem and network restrictions before the script starts and propagate those restrictions to child processes. For example:

```toml
[run.scripts.test]
assurance = "restricted"
read = ["."]
write = ["build"]
network = false
```

Paths are project-relative. Grant only the access the script needs: `network = false` denies network socket creation/traffic, while `network = true` allows unrestricted networking. The backend also constructs a limited child environment and closes unrelated inherited descriptors.

Restricted is an authority boundary, **not** full process-tree management or a promise that arbitrary script code is safe. Tapid does not guarantee cleanup or termination of detached descendants. Configured timeout, output, process-count, and memory limits are unsupported. A requested restriction the backend cannot enforce causes the run to fail before the target starts; Tapid does not silently run it without containment. Linux Restricted uses Landlock and seccomp and requires kernel support; it has targeted Ubuntu 24.04.5 x86_64 validation. macOS Restricted is experimental and uses deprecated/private Seatbelt APIs.

The command requires checked-in `tapid.toml` and an exact `[run.scripts.<name>]` profile; `[run.defaults]` is merged only into that explicitly selected profile. `assurance = "restricted"` explicitly requests ADR 0005 **Restricted** execution. Omitting `assurance` retains the legacy-safe **ManagedTree** contract, which additionally requires race-free descendant ownership, complete cleanup/kill, and configured tree-wide timeout, output, process, and memory semantics. Unsupported required dimensions fail before the shell starts.

The command constructs a minimal environment rather than preserving inherited variables: `PATH` is reserved and cannot be allowlisted, while other declared names are retrieved individually from the caller only when present. Windows environment-name matching is case-insensitive and case-equivalent allowlist duplicates are rejected. The current `network` field is boolean: `true` grants unrestricted networking. `--hostname`, `--port`, and other forwarded application arguments do not constrain authority; declared listen/connect scopes remain future work.

`--node-runtime` is optional. Without it, Tapid examines at most 256 absolute entries from the invoking host's `PATH`, ignores empty or relative entries that could resolve through an untrusted working directory, and selects the first canonical, regular executable named `node` (`node.exe` on Windows). With it, Tapid validates that exact path instead. Host `PATH` is used only for this trusted preflight lookup and is never copied or appended to the child environment. On macOS the child receives a new `PATH` ordered as a private directory containing only a byte-verified `node` snapshot on a distinct inode, canonical project `node_modules/.bin`, then the runtime's canonical directory. Project commands win over ordinary runtime tools, while project-controlled `node` cannot shadow the verified executable. A selected runtime under project write authority is rejected. The runner creates a fresh unpredictable owner-only directory outside project write authority for each attempt and never reuses retained directories. It copies from a held runtime object, verifies exact bytes and executable metadata, and makes Seatbelt deny hard links and writes to the private snapshot while project writes remain allowed. It retains the snapshot whenever subprocess-enabled targets may have executed, including post-launch errors, because Restricted cleanup cannot prove descendants have exited. It removes the directory on pre-spawn and proven failed-exec paths. Successful `subprocess=false` executions remove it only after root exit, with Seatbelt denying process-fork. Private device/inode, mode, size, and link count are checked immediately before launch. Host writes and races after the final check remain residual risks. Other platforms retain their request construction but cannot execute a native sandbox. An absent project `.bin` is omitted from the search path and runtime grants. If present, it must be a real directory strictly beneath the canonical project root; unsafe `node_modules` parents, pre-existing symlinks, and canonical escapes are rejected. The macOS backend requires a relocatable Node binary, such as the official Node distribution; builds depending on executable-relative external libraries are not supported by the private snapshot. On Unix the runtime must have an executable bit.

On Unix, package scripts use `/bin/sh -c <script> tapid-script <forwarded-args...>` with `"$@"` boundaries and preserve native argument bytes. On Windows, the request uses `cmd.exe /D /S /C`, npm-compatible escaping for forwarded arguments, and a verbatim adapter boundary; CR/LF arguments are rejected. Configuration input is bounded before parsing. Unknown configuration, invalid project-relative paths, invalid environment names, reserved `PATH`, invalid runtimes, unsupported combinations, and unavailable guarantees fail before the shell starts. Project configuration cannot turn containment off. A noisy `--no-sandbox` outcome is planned only for trusted interactive projects, with no enforcement receipt, no silent fallback, and unattended rejection unless separately authorized; this CLI does not implement it.

The CLI uses `tapid-runner::ExecutionRequest` and checked execution exclusively for normal root scripts. Native backends own live child stdout/stderr streaming; the CLI does not replay captured bytes after completion. The checked launch receipt matches the exact requested enforcement and identifies each dimension's request, mechanism, assurance level, enforcement state, scope, and limitation; `CanonicalPath` is not `NativeObject`, and limit terminations remain distinct. Completion evidence describes lifecycle and cleanup results only and must not re-confirm launch-only authority. For Restricted, it must distinguish no cleanup guarantee from best-effort cleanup actually attempted or observed. Native macOS Restricted execution is evidence-gated. ManagedTree, all configured resource limits, and non-macOS native execution fail closed as `unsupported-containment` before target spawn and issue no receipt. The Windows verbatim command boundary and all native backend behavior still require execution on their target operating systems.

On successful contained execution, stderr receives a receipt after streamed child output. Human output starts with `sandbox receipt:` and pretty-printed JSON. `--receipt-json` emits the same data as one compact JSON line, preceded by a newline even when child stderr did not end with one. Child stdout and stderr are streamed once and are not embedded or replayed in the receipt. On success the final stderr line in JSON mode is the receipt; launch failures emit an error and no receipt. Child output is untrusted and may resemble receipt text, so consumers must also check the CLI completion and final record.

Receipt schema version 1 contains `assurance`, `backend` with name/version/deprecation, `requested`, `declared`, `observed`, and `enforced` boolean dimension maps; `declared_evidence`, `observed_evidence`, and `established_evidence` arrays with dimension/scope/mechanism/limitations; `effective_filesystem`; `configured_limits`; `termination`; and `completion` with confirmed dimensions, evidence, and cleanup confidence. Each filesystem entry contains display `path`, lossless `native_path` with `encoding` and integer `units`, `access`, `kind`, `source`, and `binding`. Unix encoding is `unix-bytes`; Windows uses `windows-utf16`. Enum values use their Rust names. Project and runtime entries remain separate via `ProjectPolicy` and `BackendRuntime`. Limits use their configuration names and `null` for unconfigured values. No macOS configured limit is silently accepted. See [runner semantics](../tapid-runner/README.md) for exact directory data, global metadata, explicit devices, sampled probes, and cleanup limitations.

Install derives executable shims from verified package `bin` metadata. Unix uses symlinks. Windows writes `.cmd` and PowerShell wrappers. The planner rejects malformed metadata, absolute or traversal targets, symlink and special-file targets, collisions, and unsupported platforms. Root scripts remain arbitrary code and can use every explicitly granted capability.

## Lifecycle policy and limitations

- Dependency lifecycle scripts are disabled during every install path.
- Root scripts run only after the explicit `tapid run` command.
- Root-script execution is wired to fail-closed preflight. macOS 26 has an experimental Restricted backend using deprecated/private native Seatbelt APIs; Linux Restricted uses Landlock/seccomp and has targeted Ubuntu 24.04.5 x86_64 local-VM and hosted CI validation. ManagedTree, configured resource-limit profiles, Windows native containment, and the broader Linux Restricted probe matrix remain unsupported or pending.
- Full npm CLI/package-specifier compatibility is not implemented: tags, git/file/workspace specs, peer semantics, workspaces, and complete optional-dependency and lockfile behavior remain out of scope. Range satisfaction is differential-tested against pinned node-semver 7.8.5 for the documented grammar in `crates/tapid-resolver/README.md`.
- `add`, `remove`, `update`, `prune`, script approval, and package publishing are outside this slice. Private-registry authentication is available as a development feature, not a production-support claim.
- JSR installation remains fail-closed unless metadata provides both an HTTPS npm tarball URL and a valid SHA-512 SRI value. Live JSR integrity behavior is unsupported and unverified.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform does not prove behavior on another.

The macOS runner requires the binary's early private-launcher initializer. `sandbox-exec` launches that same executable under a parameter-bound profile. A fixed-size nonce/version READY/GO protocol, kqueue NOTE_EXEC, and CLOEXEC status EOF establish launch; target exit codes and stderr never do. Receipt `executable_resolution` reports exact Unix byte arrays for PATH and its ordered entries, `caller_path_inherited = false`, the distinct byte-verified private Node snapshot identity and validation timing, and cleanup observation. This reserves bare `node` and env-shebang resolution, not explicit paths to project executables.

For retained bindings, `executable_resolution.reserved_node.cleanup_observed` is `false`, and `limitations` explicitly describes retention. This field reports removal of the private snapshot, independently of best-effort process-group cleanup in `completion`. Retained directories and snapshots consume temporary storage until OS cleanup or host removal after every descendant exits. Tapid does not schedule deletion or reuse them. OS or host removal while descendants survive ends reserved-node protection. Host writes or races after final validation remain outside Restricted containment; retention provides no ManagedTree ownership or cleanup guarantee.

Npm aliases are supported in manifest dependencies and package arguments such as `tapid add 'h3-v2@npm:h3@2.0.1-rc.20'`. Scoped targets and supported semver ranges retain their actual registry identity and local import names during install and frozen/offline replay. See [alias behavior and limits](../../docs/compatibility.md#npm-aliases).
