<h1 align="center">
  <img src=".github/assets/tapid-banner.png" alt="Tapid by LimeTip" width="85%">
</h1>

<p align="center">
  A JavaScript and TypeScript package manager written in Rust,<br>
  with verified downloads, reproducible installs, and dependency lifecycle scripts disabled during installation.
</p>

<p align="center">
  <a href="https://github.com/LimeTip/tapid/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/LimeTip/tapid/ci.yml?branch=main&amp;style=flat&amp;label=CI&amp;labelColor=202020&amp;color=a3e635" alt="CI status"></a>
  <a href="https://github.com/LimeTip/tapid/releases"><img src="https://img.shields.io/github/v/release/LimeTip/tapid?style=flat&amp;label=release&amp;labelColor=202020&amp;color=a3e635" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-a3e635?style=flat&amp;labelColor=202020" alt="License: Apache 2.0"></a>
</p>

<p align="center">
  <a href="https://tapid.dev">Website</a> ·
  <a href="crates/tapid-cli/README.md">CLI guide</a> ·
  <a href="docs/compatibility.md">Compatibility</a> ·
  <a href="#contributing">Contribute</a>
</p>

## Install Tapid

macOS and Linux:

```bash
curl -fsSL https://tapid.dev/install.sh | bash
```

Windows PowerShell:

```powershell
iwr -useb https://tapid.dev/install.ps1 | iex
```

These install the latest published development release. Open a new terminal, or follow the PATH instructions printed by the installer, before using `tapid`.

For specific versions, source builds, verification, upgrades, and uninstall instructions, see the [installation details](#installation-details). For release readiness, see [Development status](#development-status).

## Quick start

Create a project, install a package, then replay the install without network access:

```bash
mkdir demo
cd demo
tapid init
tapid i is-char
tapid install --offline --frozen
```

`tapid i` adds the dependency to `package.json`, writes `tapid.lock`, and creates `node_modules`. The replay command uses the lockfile and locally stored verified package trees. Use your project's Node.js runtime and tooling to execute code.

See the [CLI guide](crates/tapid-cli/README.md) for adding, removing, updating, and inspecting dependencies.

## Development status

Tapid is under active development. Releases are available for evaluation and compatibility testing; production support is not yet available. The current target is a bounded subset of the Node.js/npm ecosystem. Deno and Bun compatibility remains a future goal.

Download verification checks package bytes against registry-declared integrity. It does not authenticate publishers or detect malicious or vulnerable packages. Experimental `tapid run` is a separate Node.js-only script launcher with platform-specific containment limits.

See the [supported subset and limitations](#supported-subset-and-limitations) and [production adoption gate](docs/production-adoption.md) before evaluating Tapid for a production project.

## What Tapid manages

Tapid focuses on safer dependency selection and installation: it uses explicit package identities and integrity checks, and makes installs deterministic and recoverable. Vulnerability intelligence, publisher/provenance signals, and human audit attestations are product goals—not capabilities to assume are implemented today. Check [Supported subset and limitations](#supported-subset-and-limitations) for current guarantees.

Tapid does not replace Node.js, Deno, Bun, or another JavaScript runtime. The installed `node_modules` layout is intended for Node.js-compatible projects. Runtime-specific compatibility with other runtimes must be validated rather than assumed.

A `tapid.lock` records the root manifest digest, exact selected package identities, registry-declared artifact integrity, unpacked tree digests, and dependency edges. For example, a package entry is shaped like this (digest values shortened for readability):

```json
{
  "lockfile_version": 7,
  "root_manifest_digest": "sha256-…",
  "resolver_version": "0",
  "linker_version": "0",
  "roots": ["https://registry.npmjs.org|is-char@1.0.0|peer=-|platform=-"],
  "packages": {
    "https://registry.npmjs.org|is-char@1.0.0|peer=-|platform=-": {
      "registry": "https://registry.npmjs.org",
      "name": "is-char",
      "version": "1.0.0",
      "artifact_integrity": "sha512-…",
      "registry_integrity_declared": true,
      "unpacked_digest": "sha256-…",
      "tree_digest": "sha256-…",
      "dependencies": {}
    }
  }
}
```

The lockfile pins what was selected and supports verified replay; registry integrity verifies downloaded bytes against registry metadata, not publisher identity or package safety.

## Current package-management implementation

The consumer workflow exercises deterministic dependency resolution, npm metadata and artifact retrieval, exact multi-version dependency edges, verified archives, canonical `tapid.lock` generation, managed `node_modules`, offline/frozen replay, and suppression of dependency lifecycle scripts. This is a bounded npm-compatible subset, not full npm or pnpm compatibility.

### Synthetic news-site compatibility fixture

`examples/news-site-consumer` is a public, synthetic server-rendered Next.js/React/TypeScript application for evaluating package-manager compatibility on a representative news-site workload. The route at `/acceptance` returns the unique marker `TAPID_NEWS_SITE_ACCEPTANCE_V1`. Its npm-generated `package-lock.json` (lockfile v3) is the reference install. Direct dependencies and every floating transitive dependency are pinned to that reference through exact manifest versions and flat `overrides`; Tapid's native online resolver does not import the npm lock. This prevents later registry publications (including `caniuse-lite`) from changing just the Tapid side. Update those pins and the npm reference together deliberately, not during CI. `tests/news_site_fixture_test.ts` verifies the entire locked dependency/optional-dependency closure and proves that the strict comparator rejects version/edge drift. Install that reference in a separate directory with `npm ci`, then run a clean Tapid install and frozen/offline replay in the fixture. `scripts/compare-news-site-package-graphs.ts` compares reachable names/versions and dependency/peer edges, source origins, integrity, and platform-optional selections; it also reports physical-only packages even when unreachable. Tapid—not npm or a direct Node command—runs the fixture's `build`, `test`, and `start` scripts. Lockfile generation used Node.js v26.10.0 / npm 11.19.1; CI uses Ubuntu 24.04 / Node.js 22 and records its toolchain versions. The fixture contains no private code, customer information, credentials, or proprietary assets. From the repository root:

```bash
cargo build --locked --bin tapid
cd examples/news-site-consumer
npm_reference="$(mktemp -d)"
cp package.json package-lock.json "$npm_reference/"
npm ci --prefix "$npm_reference"
mkdir -p .next .tmp
tapid() { ../../target/debug/tapid "$@"; }
tapid install
tapid install --frozen
tapid install --offline --frozen
node --experimental-strip-types ../../scripts/compare-news-site-package-graphs.ts \
  --npm-root "$npm_reference" \
  --tapid-root "$PWD" \
  --json .tmp/package-graph.json \
  --text .tmp/package-graph.txt
export NEXT_TELEMETRY_DISABLED=1
export TMPDIR="$PWD/.tmp"
tapid run build
tapid run test
tapid run start
# In another terminal:
curl --fail http://127.0.0.1:3000/acceptance
```

The expected response is `TAPID_NEWS_SITE_ACCEPTANCE_V1`. Next.js production output is stored in the ignored `.next/` directory; `.tmp/` and `node_modules/` are also generated and ignored. The checked-in `tapid.toml` requests Restricted execution, with build output and temporary files limited to fixture-local paths and networking disabled by default. CI installs the npm baseline in a separate directory, performs Tapid online/frozen/offline-frozen install, fails on reachable graph, dependency/peer-edge, source, integrity, or platform-optional drift, and retains the deterministic graph report. Physical-only packages are reported even when unreachable. The app scripts are executed through Tapid after its managed install.

The package-management toolchain also includes:

- `init`, `install`/`i`, `add`, `remove`, and `update` for project manifests and dependencies.
- `ci` to install the exact graph in `tapid.lock`, downloading missing verified packages without resolving versions again.
- `outdated` to compare locked versions with registry metadata, and `prune` to remove unreachable managed packages.
- A content-addressed local store and lockfile replay for offline installs, with transactional activation of managed `node_modules`.
- Safe archive extraction and integrity checks, plus generated package `bin` shims. Dependency lifecycle scripts are suppressed during installation.

These controls improve repeatability and reject certain mismatches, but they do not currently detect vulnerable or malicious packages or authenticate publishers. See [Supported subset and limitations](#supported-subset-and-limitations) for exact behavior. Experimental root-script execution is separate and not the product focus; see [ADR 0005](docs/adr/0005-default-on-root-script-sandbox.md) for its status and limitations.

The non-fixture online path requests abbreviated npm install metadata and requires registry-declared SHA-512 integrity by default. Unsupported npm range syntax and malformed historical metadata are filtered or rejected fail-closed according to their scope. Live JSR installation remains unverified. Do not treat fixture replay or one successful npm project as evidence of complete npm compatibility.

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

The Unix installer manages only `~/.local/bin` through a versioned `tapid-path-managed-v1` block in an owned, writable Bash or POSIX startup file (`$HOME/.bash_profile`, `$HOME/.bashrc`, or `$HOME/.profile`). Repeated installs are idempotent; uninstall removes only that exact Tapid block and preserves unrelated PATH entries. Symlinked, non-regular, foreign, or unwritable startup files are rejected. For a custom `--install-dir`, the installer prints the directory that must be added manually. PowerShell updates the user-level Windows PATH. Open a new terminal, or follow the command printed by the installer, before using `tapid`. Other Unix shells remain outside this slice.

Remove the Tapid CLI binary and its managed PATH block on Unix:

```bash
curl -fsSL https://raw.githubusercontent.com/LimeTip/tapid/main/scripts/uninstall.sh | sh
```

Windows uninstall:

```powershell
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\uninstall.ps1
```

The next-release installers and upgrade command, expected for 0.0.11, select immutable HTTPS archives through `https://tapid.dev/releases/v1/latest.tsv`. They verify the release record's size and SHA-256, require exactly the expected regular executable in the archive, and stage the destination before replacement. GitHub remains the initial archive host; the owned discovery address permits a future provider change. The [release-record contract](docs/release-record-v1.md) and [release runbook](docs/release-distribution.md) describe the coordinated first rollout. This source documentation does not establish that the new routes or release are public.

Use `tapid upgrade --dry-run` to inspect the selected release. The new default has no GitHub discovery fallback. Invalid received metadata or mismatched downloads fail; unavailable discovery can use local recovery while reporting that the latest release could not be checked. Verified identical executable bytes produce an already-up-to-date result without replacement. `--release-url` or `TAPID_RELEASE_RECORD_URL` selects another release record; explicit `--endpoint` retains the historical signed protocol. These HTTPS and checksum paths do not provide independent release authentication.

Released 0.0.10 clients still use signed discovery followed by GitHub checksum fallback and can reach the next release that way. Explicit installer versions through 0.0.10 use the historical GitHub assets and `SHA256SUMS` unless a record override is supplied. Clients older than 0.0.10 require rerunning the installer. Alternate repositories remain explicit through `--repo` or `TAPID_REPO`; their release controls are the operator's responsibility. Source installation remains the development path. Uninstall scripts never remove project-local `.tapid-store`, `tapid.lock`, or `node_modules` data.

Installed package `bin` metadata produces executable entries in `node_modules/.bin`. Unix uses symlinks. Windows uses `.cmd` and PowerShell wrappers. Bin targets must be regular files inside the verified package tree; traversal, absolute paths, symlinks, collisions, and unsupported platforms are rejected.

## Install from the lockfile

```text
tapid ci
tapid ci --offline
```

`tapid ci` requires `tapid.lock` and matching root and workspace manifests. It preserves `package.json` and `tapid.lock`, installs the locked versions and edges, and atomically replaces managed `node_modules`. Missing store trees are downloaded from locked HTTPS artifact URLs, checked against locked SHA-512 integrity and SHA-256 tree digests, and published through the install transaction. Dependency lifecycle scripts do not run. `--offline` requires every verified tree in the store.

`ci` requires download URLs for every registry package, even with a warm cache or `--offline`. Locks missing download URLs need regeneration with `tapid update` using live registry metadata and review of the resulting changes. The explicit `--registry-fixture` option supplies local artifacts for tests and air-gapped development. Tapid uses its own lockfile and supported dependency semantics; this is not complete npm compatibility. An unmanaged `node_modules` is rejected rather than deleted.

## Offline and frozen replay

Both modes require an existing lockfile and all referenced verified trees:

```text
tapid install --offline --project-dir ./example
tapid install --frozen --project-dir ./example
```

Ordinary install reuses a matching lock. Changed manifests preserve compatible locked selections; `tapid update` requests a fresh graph. Frozen installation with a native lock keeps the exact graph and lock bytes, and can download missing pinned archives without resolving metadata. Imported npm locks preserve selections but may record verified tree receipts. Offline installation forbids downloads, including with `--frozen`. Native lock hydration requires a pinned HTTPS URL and registry-declared SHA-512 provenance, then verifies the archive and locked tree digest. Replay validates manifest digests, registry routes, platform contexts, tree markers, and managed output before atomic activation. `--store-dir PATH` selects another verified store root. See [locked installation decisions](docs/adr/0008-locked-install-selection-and-hydration.md).

## Supported subset and limitations

- npm package metadata with semver versions, package dependencies, and HTTPS tarball URLs is supported.
- npm aliases such as `"h3-v2": "npm:h3@2.0.1-rc.20"` preserve the local import name and the actual registry identity through install and frozen/offline replay. Alias targets accept supported semver ranges; dist-tags remain unsupported. See [alias behavior](docs/compatibility.md#npm-aliases).
- Range satisfaction is differentially tested against pinned `node-semver` 7.8.5 for exact and partial versions, `x`/`*` wildcards, comparators and intersections, caret/tilde (including zero-major bounds), hyphen ranges, `||` alternatives, prerelease eligibility, and ignored build metadata. This is not a claim of complete npm CLI or package-specifier compatibility; tags and git/file dependencies remain unsupported, and workspace declarations are limited to the documented forms.
- `add`, `remove`, and range-preserving `update` are available for the current package, with `--dev`, `--optional`, `--peer`, and explicit `--latest` mutation modes. `add --peer` records only a declaration; registry package peer requirements are validated against compatible direct project roots and recorded in peer contexts in lockfile/materialization identities. Missing or incompatible providers fail closed transactionally. Nested/ancestor peer-provider lookup and multiple contexts for one exact package instance remain unsupported. `outdated` is read-only during normal operation and reports lockfile versions and registry metadata for the selected manifest; if it finds a durable interrupted-transaction journal, it recovers project state before reporting. `prune` replays the validated workspace-root lockfile atomically, including with `--workspace`, to remove unreachable managed output; it does not mutate the selected member manifest. `add`, `remove`, `update`, and `outdated` use the `--project-dir` manifest by default, or a named member selected with `--workspace <name>` where applicable. npm-style workspaces support bounded discovery/globs, local package links, and root or selected-member operations with root-owned lockfile, store, and activation; scripts require explicit `tapid run`. Unsupported layouts and protocols fail closed before mutation. See the [compatibility matrix](docs/compatibility.md#compatibility-matrix).
- Lifecycle mutations are all-or-nothing across the manifest, lockfile, verified store, and managed `node_modules` activation. Resolution, integrity, archive, peer, workspace, and materialization failures preserve the prior state; verified trees are not committed to the shared store until project activation succeeds. Durable recovery journals let the next lifecycle command, including `outdated`, restore the prior state after a crash before commit or finish cleanup after a committed operation.
- The live npm path requires registry-declared SHA-512 integrity by default and verifies downloaded bytes against that digest. This integrity check matches bytes to registry metadata; it does not authenticate the publisher, prove the user intended that package, or establish the archive's package identity independently of the metadata. The explicit `--allow-unverified-registry-artifacts` compatibility exception permits missing integrity and is online-only.
- Vulnerability intelligence, package malware scanning, publisher/provenance verification, and human audit attestations are not implemented. A verified archive is not necessarily safe or vulnerability-free.
- Lifecycle scripts from dependencies never run during install. There is no approval workflow yet.
- JSR support is experimental. Live JSR installation is not verified. A JSR artifact is accepted only when metadata supplies an HTTPS npm tarball URL and a valid SHA-512 SRI value. Tapid does not derive or trust integrity from transport bytes.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform is not evidence for another.
- ADR 0005 default-on, fail-closed CLI wiring and configuration parsing are integrated. macOS 26 Restricted execution is experimental and uses deprecated/private native Seatbelt APIs; Linux Restricted uses Landlock and seccomp and has targeted Ubuntu 24.04.5 x86_64 local-VM and hosted CI validation. ManagedTree, configured resource-limit profiles, Windows native containment, and the broader Linux Restricted probe matrix remain unsupported or pending. Package-level malware scanning, package provenance verification, and independently authenticated client release metadata also remain unavailable.

## Contributing

Help us make Tapid work with more real projects. Useful contributions include:

- Trying it in a disposable copy of a Node.js project and reporting compatibility failures with a minimal reproduction, platform, and Tapid version.
- Improving installation instructions and examples when a step is unclear.
- Adding isolated package fixtures and regression tests for unsupported or failing cases.
- Working on Rust capabilities such as resolution, manifests, archives, and installed layouts.

Start with the [contributing guide](CONTRIBUTING.md) and [open issues](https://github.com/LimeTip/tapid/issues). For a substantial change, describe the problem and proposed approach before implementation. Report security vulnerabilities through [SECURITY.md](SECURITY.md).

## Development

Use Git, Rust with rustfmt and Clippy, and Node.js 22.7.0 or later. Run Cargo through the development wrapper to reuse build artifacts across worktrees. See the [testing guide](docs/testing.md) for focused checks and the full local lane.

Rust owns Tapid behavior and security verification. TypeScript owns developer
scripts, release orchestration, documentation checks, and website/consumer
fixtures, with tests run by Node. Python and uv are not project prerequisites.

```text
node --experimental-strip-types tools/check_architecture.ts
node --experimental-strip-types --test tools/check_architecture_test.ts tests/*_test.ts tools/release/*_test.ts
node --experimental-strip-types scripts/dev.ts fmt --all --check
node --experimental-strip-types scripts/dev.ts clippy --workspace --all-targets --all-features --locked -- -D warnings
node --experimental-strip-types scripts/dev.ts test --workspace --all-features --locked
node --experimental-strip-types scripts/dev.ts test --manifest-path tests/integration/Cargo.toml --tests --locked
git diff --check
```

The workspace is under active development. Do not treat the current binary or registry behavior as a production package-management guarantee. Do not push, publish, or release from a documentation-only checkout.

## Project direction

Longer-term work includes broader npm compatibility, native package and registry protocols, explicit private-registry routing, evidence-aware policy, provenance, audit attestations, and safer execution. Those are product goals, not current capabilities.

## License

The Tapid CLI and its supporting crates in this repository are developed by LimeTip AB and licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the complete text.

Copyright 2026 LimeTip AB.

Run `tapid license` to print the complete license and copyright attribution embedded in the executable. This command works offline and does not require a project.

Existing npm projects can use `tapid import-package-lock <path>` to preserve supported npm v3 selections without resolution. Import is offline; the first frozen install verifies pinned tarballs. See the [migration and rollback guide](docs/npm-lockfile-import.md). Tapid manages packages and lockfiles; Node.js, workerd, Wrangler, and deployment tools keep their existing roles.
