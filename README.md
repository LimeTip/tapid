# Tapid

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)

Tapid is an early Rust package manager and package runner for JavaScript and TypeScript projects. The current consumer workflow targets a small, explicit npm-compatible subset. It is not production-ready and has not been published as a supported release.

## Current consumer workflow

From a project containing `package.json`:

```text
tapid install
tapid run init
tapid run dev
tapid run build
tapid run test
```

`tapid install` resolves npm metadata and downloads npm tarballs over the configured HTTPS transport. It verifies npm SHA-512 integrity when metadata supplies it, validates archive entries, stores verified trees under a project-local `.tapid-store` by default, writes `tapid.lock`, and atomically activates `node_modules`. Dependency lifecycle scripts are disabled during install.

Use a project directory explicitly when running outside the project directory:

```text
tapid install --project-dir ./example
tapid run --project-dir ./example test -- --runInBand
```

`tapid run <script>` reads a root `package.json` script, runs it in the project directory, prepends the managed `node_modules/.bin` directory to `PATH`, forwards arguments after `--`, and returns the child exit status. Root scripts execute arbitrary project code. This is compatibility-oriented process execution, not a sandbox.

## Installing the CLI

Tapid is still pre-release and does not publish a stable binary yet. For contributor development, install a specific source ref:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh \\
  | sh -s -- --source-ref main
```

Once stable GitHub releases exist, the same installer will default to the latest stable release:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh | sh
```

Select a specific release explicitly:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/install.sh \\
  | sh -s -- --version v0.1.0
```

Upgrade to the latest stable release:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/upgrade.sh | sh
```

Remove only the Tapid CLI binary:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/uninstall.sh | sh
```

The uninstall script never removes project-local `.tapid-store`, `tapid.lock`, or `node_modules` data. Release installation currently verifies HTTPS-delivered SHA-256 checksums; independently signed release metadata is a planned follow-up. Source installation is explicitly a development path until platform release assets are published.

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
