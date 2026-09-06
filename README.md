# Tapid

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid)](https://crates.io/crates/tapid)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid)](https://crates.io/crates/tapid)
[![Docs.rs](https://docs.rs/tapid/badge.svg)](https://docs.rs/tapid)
[![License](https://img.shields.io/crates/l/tapid)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Tapid is a JavaScript and TypeScript package manager written in Rust. It provides deterministic dependency installation, verified package storage, Node-compatible `node_modules` materialization, lockfile replay, and explicit root-script execution. The current implementation targets a small, explicit npm-compatible subset. Development releases are available from GitHub Releases; production support is not yet available.

## Install Tapid

**macOS and Linux**

```bash
curl -fsSL https://tapid.dev/install.sh | bash
```

**Windows PowerShell**

```powershell
iwr -useb https://tapid.dev/install.ps1 | iex
```

These commands install the latest published Tapid release from the immutable GitHub release assets published by `LimeTip/tapid` and verify the selected archive against its `SHA256SUMS` entry. See [installation details](#installation-details) for release selection, contributor source builds, alternate repositories, and uninstall instructions.

## Quick start

The shortest path from an empty directory to installing a package is:

```bash
mkdir my-app
cd my-app
tapid init
tapid i is-char
```

To run a development script, add a `dev` entry to `package.json`, check in its permissions in `tapid.toml`, then run:

```bash
tapid run dev
```

`tapid i <package>` is an alias for `tapid install <package>`. The package form adds the dependency to `package.json`, resolves it from the configured registry, writes `tapid.lock`, and materializes `node_modules`. A package version can be supplied as `<package>@<version>`.

ADR 0005 makes containment default-on and fail-closed for root scripts. The policy parser and platform-neutral execution contracts are implemented, while CLI wiring and Linux/Windows runtime enforcement remain pending integrated verification. macOS 26 is unsupported because it lacks a public unprivileged primitive for race-free descendant ownership. This describes the accepted target contract, not a containment guarantee provided by the current binary.

For example, a Next.js development server needs project writes and network access but does not need ambient credentials:

```toml
[run.defaults]
read = ["."]
write = []
network = false
environment = []
subprocess = true
timeout_seconds = 300
max_output_bytes = 8388608
max_processes = 32
max_memory_bytes = 1073741824

[run.scripts.dev]
write = ["."]
network = true
environment = ["NODE_ENV"]
timeout_seconds = 28800
max_output_bytes = 67108864
max_processes = 64
max_memory_bytes = 2147483648
```

`write = ["."]` permits Next.js to create `.next`, `next-env.d.ts`, and any other project-local generated files; narrow it after observing the project's actual writes. `network = true` is the selected schema's portable boolean grant: it lets the server bind locally but also permits outbound connections, so it is not a loopback-only rule. `environment` names variables that may be copied from the caller when present; it does not import the rest of the caller's environment. Pass the bind address explicitly without exposing `HOST`, cloud credentials, proxy settings, or agent sockets:

```bash
tapid run dev -- --hostname 127.0.0.1 --port 3000
```

## Current consumer workflow

The consumer path supports validated fixture replay and bounded live npm metadata and artifact retrieval. It exercises deterministic transitive resolution, exact multi-version dependency edges, verified archives, canonical `tapid.lock` generation, managed `node_modules`, offline and frozen replay, the pre-ADR root-script path, argument forwarding, and lifecycle suppression. It does not verify ADR 0005 containment.

For a clean checkout, build Tapid and create the readable consumer fixture through the same helper used by CI:

```text
cargo build -p tapid
node tests/fixtures/create_consumer_project.js
```

The helper writes `TAPID_FIXTURE_PROJECT` to the `GITHUB_ENV` file supplied by CI. For a local smoke test, set that variable yourself and run the generated project path:

```bash
export GITHUB_ENV="$(mktemp)"
node tests/fixtures/create_consumer_project.js
. "$GITHUB_ENV"
export TAPID_FIXTURE=1
target/debug/tapid install --offline --frozen --project-dir "$TAPID_FIXTURE_PROJECT"
target/debug/tapid run --project-dir "$TAPID_FIXTURE_PROJECT" test -- forwarded 0
```

The non-fixture online path requests abbreviated npm install metadata and requires registry-declared SHA-512 integrity by default. Unsupported npm range syntax and malformed historical metadata are filtered or rejected fail-closed according to their scope. Live JSR installation remains unverified. Do not treat fixture replay or one successful npm project as evidence of complete npm compatibility.

The accepted invocation remains `tapid run <SCRIPT> -- <ARGS...>`. Values after the first `--` are forwarded in order to the selected script; the separator itself is not forwarded, and Tapid must not reinterpret forwarded values as Tapid options. Under ADR 0005 the future wired path reads the script's merged `[run.defaults]` and `[run.scripts.<name>]` policy, constructs a minimal environment with a controlled `PATH`, and starts only after the selected backend establishes every requested restriction. Until that wiring and platform runtime probes are integrated, no containment claim is made for the executable in this checkout.

Use a project directory explicitly when running outside the project directory:

```text
tapid install --project-dir ./example
tapid run --project-dir ./example test -- --runInBand
```

## Installation details

The public installers at `tapid.dev` install the latest published release:

```bash
curl -fsSL https://tapid.dev/install.sh | bash
```

```powershell
iwr -useb https://tapid.dev/install.ps1 | iex
```

For contributor development, install a specific source ref instead:

```bash
curl -fsSL https://tapid.dev/install.sh | bash -s -- --source-ref main
```

Select a specific published release explicitly, for example:

```bash
curl -fsSL https://tapid.dev/install.sh | bash -s -- --version v0.0.8
```

On Windows, download the public installer when you need to review it or pass options such as `-SourceRef`:

```powershell
$installer = Join-Path $env:TEMP "tapid-install.ps1"
Invoke-WebRequest https://tapid.dev/install.ps1 -OutFile $installer
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer -SourceRef main
Remove-Item $installer
```

Both installers add their user-local install directory to PATH without requiring administrator privileges. Unix shells are detected from `$SHELL`; zsh, bash, fish, and a POSIX profile fallback are supported. The default Unix directory, `~/.local/bin`, is configured automatically. For a custom `--install-dir`, the installer prints the directory that must be added manually. PowerShell updates the user-level Windows PATH. Open a new terminal, or follow the command printed by the installer, before using `tapid` in an existing terminal.

Remove only the Tapid CLI binary on Unix:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/uninstall.sh | sh
```

Windows uninstall:

```powershell
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\uninstall.ps1
```

The installers use the canonical `LimeTip/tapid` repository by default. Its release installation uses immutable GitHub release assets over HTTPS, verifies the archive against `SHA256SUMS`, validates that the archive contains only the expected regular executable, and stages the destination before replacement. Alternate repositories are explicit through `--repo` or `TAPID_REPO`; the installers do not establish whether an alternate repository provides equivalent release immutability. The uninstall scripts never remove project-local `.tapid-store`, `tapid.lock`, or `node_modules` data. Source installation remains the explicit development path. The checksum and archive share the same GitHub trust boundary, so this is integrity checking rather than independent release authentication. `tapid upgrade` is intentionally unavailable; rerun the installer to upgrade.

Installed package `bin` metadata produces executable entries in `node_modules/.bin`. Unix uses symlinks. Windows uses `.cmd` and PowerShell wrappers. Bin targets must be regular files inside the verified package tree; traversal, absolute paths, symlinks, collisions, and unsupported platforms are rejected.

## Offline and frozen replay

Both modes require an existing lockfile and all referenced verified trees:

```text
tapid install --offline --project-dir ./example
tapid install --frozen --project-dir ./example
```

Offline and frozen replay do not resolve metadata or fetch archives. The lockfile manifest digest, package identities, tree digests, markers, and managed output are validated before atomic activation. `--store-dir PATH` selects another verified store root.

## Supported subset and limitations

- npm package metadata with semver versions, package dependencies, and HTTPS tarball URLs is supported.
- Exact, bare major and minor, caret, tilde, and selected whitespace-separated comparison requirements are supported. Full npm range syntax, aliases, tags, git, file, workspace, and peer-resolution compatibility are not complete.
- Lifecycle scripts from dependencies never run during install. There is no approval workflow yet.
- `add`, `remove`, `update`, `prune`, workspaces, full npm lockfile compatibility, and private-registry authentication are not implemented.
- JSR support is experimental. Live JSR installation is not verified. A JSR artifact is accepted only when metadata supplies an HTTPS npm tarball URL and a valid SHA-512 SRI value. Tapid does not derive or trust integrity from transport bytes.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform is not evidence for another.
- ADR 0005 accepts an OS-backed, default-on root-script containment contract, but the configuration parser, platform backends, CLI wiring, and integrated runtime evidence are pending. Tapid does not yet claim a verified sandbox on macOS, Linux, or Windows. Package-level malware scanning, package provenance verification, and independently authenticated client release metadata also remain unavailable.

## Development

Node.js 22.6.0 or later is required for the TypeScript commands.

```text
node --experimental-strip-types tools/check_architecture.ts
node --experimental-strip-types --test tools/check_architecture_test.ts tools/release/release_test.ts tools/release/publish_test.ts
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --manifest-path tests/integration/Cargo.toml --tests
cargo diff --check
```

The workspace is under active development. Do not treat the current binary or registry behavior as a production package-management guarantee. Do not push, publish, or release from a documentation-only checkout.

## Project direction

Longer-term work includes broader npm compatibility, native package and registry protocols, explicit private-registry routing, evidence-aware policy, provenance, audit attestations, and safer execution. Those are product goals, not current capabilities.

## License

Tapid is developed by LimeTip Company and is licensed under the MIT License. See `LICENSE` for the complete text.
