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
tapid run <SCRIPT> [-- <ARGS>...]
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
tapid run dev -- --host 127.0.0.1
tapid run --project-dir ./example test -- --runInBand
```

The accepted ADR 0005 command remains `tapid run <SCRIPT> -- <ARGS...>`. Values after the first `--` are forwarded in order to the selected script; the separator is not forwarded and those values are not parsed as Tapid options. Missing scripts fail with exit code `1`. Clap parsing errors use exit code `2`.

The target command reads checked-in `tapid.toml`, merges `[run.defaults]` with `[run.scripts.<name>]`, and starts only if a platform backend can enforce all requested filesystem, network, environment, subprocess, and resource restrictions. It constructs a minimal environment rather than preserving inherited variables: a controlled `PATH` includes the managed `.bin` directory and required runtime locations, and only environment names explicitly listed in policy may be copied from the caller when present. Unknown configuration, invalid project-relative paths, invalid environment names, unsupported combinations, and unavailable guarantees fail before the shell starts. Project configuration cannot turn containment off.

This is the accepted target contract. The configuration parser, platform backends, enforcement receipts, and CLI wiring are pending integrated verification, so the current binary must not be described as contained. The initial macOS backend depends on deprecated, behaviorally probed `sandbox-exec`/Seatbelt and becomes unavailable if that probe fails. Linux and Windows support is unclaimed until their backends pass positive and negative runtime probes on the exact integrated commit.

Install derives executable shims from verified package `bin` metadata. Unix uses symlinks. Windows writes `.cmd` and PowerShell wrappers. The planner rejects malformed metadata, absolute or traversal targets, symlink and special-file targets, collisions, and unsupported platforms. Root scripts remain arbitrary code and can use every explicitly granted capability.

## Lifecycle policy and limitations

- Dependency lifecycle scripts are disabled during every install path.
- Root scripts run only after the explicit `tapid run` command.
- Root-script containment is an accepted but not yet integrated capability; existing root-script tests do not verify ADR 0005.
- Full npm semver, aliases, tags, git/file/workspace specs, peer semantics, workspaces, and complete optional-dependency and lockfile compatibility are not implemented.
- `add`, `remove`, `update`, `prune`, script approval, private-registry authentication, and package publishing are outside this slice.
- JSR installation remains fail-closed unless metadata provides both an HTTPS npm tarball URL and a valid SHA-512 SRI value. Live JSR integrity behavior is unsupported and unverified.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform does not prove behavior on another.
