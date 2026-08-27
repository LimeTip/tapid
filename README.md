# Tapid

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)

Tapid is a JavaScript and TypeScript package manager written in Rust. It provides deterministic dependency installation, verified package storage, Node-compatible `node_modules` materialization, lockfile replay, and explicit root-script execution. The current implementation targets a small, explicit npm-compatible subset. Version 0.0.5 is published for development use, but signed platform binaries and production support are not yet available.

## Install Tapid

Source installation is the supported developer path today.

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh | bash
```

The default command builds Tapid from the `main` source branch locally. This is the development installation path until signed platform release assets are published.

**Windows PowerShell**

```powershell
$installer = Join-Path $env:TEMP "tapid-install.ps1"
Invoke-WebRequest https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.ps1 -OutFile $installer
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer -SourceRef main
Remove-Item $installer
```

See [installation details](#installation-details) for release selection, alternate repositories, and uninstall instructions.

## Quick start

The shortest path from an empty directory to a running development script is:

```bash
mkdir my-app
cd my-app
tapid init
tapid i is-char
tapid run dev
```

`tapid i <package>` is an alias for `tapid install <package>`. The package form adds the dependency to `package.json`, resolves it from the configured registry, writes `tapid.lock`, and materializes `node_modules`. A package version can be supplied as `<package>@<version>`.

`tapid run dev` runs the root `dev` script from `package.json`. It is a compatibility-oriented process runner, not a sandbox.

## Current consumer workflow

The currently supported consumer path is the checked-in fixture and validated lockfile replay. It exercises deterministic installation, managed `node_modules`, root-script execution, argument forwarding, and lifecycle suppression without relying on a live registry.

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

The non-fixture online path is deliberately fail-closed for transitive projects until the registry-client metadata contract exposes verified per-version dependency maps and required SHA-512 integrity for all supported registries. Do not treat a successful fixture replay as evidence that general npm installation is production-ready.

`tapid run <script>` reads a root `package.json` script, runs it in the project directory, prepends the managed `node_modules/.bin` directory to `PATH`, forwards arguments after `--`, and returns the child exit status. Root scripts execute arbitrary project code. This is compatibility-oriented process execution, not a sandbox.

Use a project directory explicitly when running outside the project directory:

```text
tapid install --project-dir ./example
tapid run --project-dir ./example test -- --runInBand
```

## Installation details

Tapid does not yet publish platform release binaries. For contributor development, install a specific source ref:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh \
  | sh -s -- --source-ref main
```

When signed platform binaries and release metadata are available, the same installer will support a stable release path. Until then, the source-ref workflow above is the only supported installation path.

Future stable-release example:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh | sh
```

Select a specific release explicitly:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh \
  | sh -s -- --version v0.1.0
```

On Windows, download and review the PowerShell installer before running it:

```powershell
$installer = Join-Path $env:TEMP "tapid-install.ps1"
Invoke-WebRequest https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.ps1 -OutFile $installer
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer -SourceRef main
Remove-Item $installer
```

Remove only the Tapid CLI binary on Unix:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/uninstall.sh | sh
```

Windows uninstall:

```powershell
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\uninstall.ps1
```

The installers use the canonical `LimeTip/tapid` repository by default. Alternate repositories are explicit through `--repo` or `TAPID_REPO`. The uninstall scripts never remove project-local `.tapid-store`, `tapid.lock`, or `node_modules` data. Release installation currently verifies HTTPS-delivered SHA-256 checksums; independently signed release metadata, platform binaries, and self-upgrade support are planned follow-ups. Source installation is the supported developer path until those distribution artifacts exist.

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
- Exact, caret, tilde, and selected whitespace-separated comparison requirements are supported. Full npm range syntax, aliases, tags, git, file, workspace, and peer-resolution compatibility are not complete.
- Lifecycle scripts from dependencies never run during install. There is no approval workflow yet.
- `add`, `remove`, `update`, `prune`, workspaces, full npm lockfile compatibility, and private-registry authentication are not implemented.
- JSR support is experimental. Live JSR installation is not verified. A JSR artifact is accepted only when metadata supplies an HTTPS npm tarball URL and a valid SHA-512 SRI value. Tapid does not derive or trust integrity from transport bytes.
- Linux and Windows consumer checks are configured in GitHub Actions. A local macOS run is not evidence for those platforms, and the repository does not claim CI execution until a workflow run is available.
- Tapid does not provide malware scanning, cryptographic signing, provenance verification, an OS sandbox, or process capability enforcement.

## Development

```text
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
