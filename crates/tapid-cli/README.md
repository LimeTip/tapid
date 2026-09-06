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
tapid run <SCRIPT> [--node-runtime <PATH>] [-- <ARGS>...]
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

The command requires checked-in `tapid.toml` and an exact `[run.scripts.<name>]` profile; `[run.defaults]` is merged only into that explicitly selected profile. It starts only if `tapid-runner` can enforce every requested filesystem, network, environment, subprocess, and resource restriction. It constructs a minimal environment rather than preserving inherited variables: `PATH` is reserved and cannot be allowlisted, while other declared names are retrieved individually from the caller only when present. Windows environment-name matching is case-insensitive and case-equivalent allowlist duplicates are rejected.

`--node-runtime` is optional. Without it, Tapid examines at most 256 absolute entries from the invoking host's `PATH`, ignores empty or relative entries that could resolve through an untrusted working directory, and selects the first canonical, regular executable named `node` (`node.exe` on Windows). With it, Tapid validates that exact path instead. Host `PATH` is used only for this trusted preflight lookup and is never copied or appended to the child environment. The child receives a newly constructed `PATH` whose first entry is the selected runtime's canonical directory and whose second entry is the canonical project `node_modules/.bin`. The latter must be a real directory strictly beneath the canonical project root; pre-existing symlinks and canonical escapes are rejected. The runner retains the selected runtime's native filesystem identity and revalidates it before spawn. On Unix the runtime must have an executable bit.

On Unix, package scripts use `/bin/sh -c <script> tapid-script <forwarded-args...>` with `"$@"` boundaries and preserve native argument bytes. On Windows, the request uses `cmd.exe /D /S /C`, npm-compatible escaping for forwarded arguments, and a verbatim adapter boundary; CR/LF arguments are rejected. Configuration input is bounded before parsing. Unknown configuration, invalid project-relative paths, invalid environment names, reserved `PATH`, invalid runtimes, unsupported combinations, and unavailable guarantees fail before the shell starts. Project configuration cannot turn containment off, and this CLI exposes no unsandboxed override.

The CLI uses `tapid-runner::ExecutionRequest` and checked execution exclusively for normal root scripts. Native backends own live child stdout/stderr streaming; the CLI does not replay captured bytes after completion. A successful result prints a receipt identifying requested, declared, observed, and enforced dimensions, and limit terminations are reported distinctly. The current runner base has no native platform backend, so execution fails closed as `unsupported-containment` before spawn and issues no receipt. The Windows verbatim command boundary and native backend behavior still require execution on their target operating systems.

Install derives executable shims from verified package `bin` metadata. Unix uses symlinks. Windows writes `.cmd` and PowerShell wrappers. The planner rejects malformed metadata, absolute or traversal targets, symlink and special-file targets, collisions, and unsupported platforms. Root scripts remain arbitrary code and can use every explicitly granted capability.

## Lifecycle policy and limitations

- Dependency lifecycle scripts are disabled during every install path.
- Root scripts run only after the explicit `tapid run` command.
- Root-script execution is wired to the containment contract, but remains unavailable until a native backend passes its runtime probes and can issue a checked receipt.
- Full npm semver, aliases, tags, git/file/workspace specs, peer semantics, workspaces, and complete optional-dependency and lockfile compatibility are not implemented.
- `add`, `remove`, `update`, `prune`, script approval, private-registry authentication, and package publishing are outside this slice.
- JSR installation remains fail-closed unless metadata provides both an HTTPS npm tarball URL and a valid SHA-512 SRI value. Live JSR integrity behavior is unsupported and unverified.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform does not prove behavior on another.
