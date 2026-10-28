<h1 align="center">
  <img src=".github/assets/tapid-banner.png" alt="Tapid by LimeTip" width="85%">
</h1>

<p align="center">
  A JavaScript and TypeScript package manager written in Rust,<br>
  with verified downloads, reproducible installs, and dependency lifecycle scripts denied by default.
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

These install the latest published development release. On Unix, run the activation command printed by the installer before using `tapid`; persistent PATH changes require `--configure-path`. On Windows, persistent user PATH changes require `-ConfigurePath`; run the printed activation command for the current PowerShell session.

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

## Evaluate an existing project

Tapid releases are available for development and compatibility evaluation; production support has not been declared. It manages packages and lockfiles, not application execution: keep Node.js (or your existing runtime), build/test tools, and deployment workflow unchanged during evaluation. Start with the [supported compatibility subset](docs/compatibility.md), then follow the [existing-project npm lock migration and rollback guide](docs/npm-lockfile-import.md) in a disposable checkout. Keep the npm lock and known-good install path until your application passes its own checks; dependency lifecycle scripts are disabled by default. A successful import or fixture run is not a production-support claim.

Standalone commands can use `tapid tool install <package>` without a project. Each tool has an isolated lock and dependency environment, with managed commands exposed from the reported bin directory. See the [tool workflow](crates/tapid-cli/README.md#standalone-cli-tools) for PATH setup, runtime requirements, and update/removal behavior.

## Development status

Tapid is under active development. Releases are available for evaluation and compatibility testing; production support is not yet available. The current target is a bounded subset of the Node.js/npm ecosystem. Deno and Bun compatibility remains a future goal.

Download verification checks package bytes against registry-declared integrity. It does not authenticate publishers or detect malicious or vulnerable packages. Experimental `tapid run` is a separate Node.js-only script launcher with platform-specific containment limits.

See the [supported subset and limitations](#supported-subset-and-limitations) and [production adoption gate](docs/production-adoption.md) before evaluating Tapid for a production project.

The [backlog reconciliation and delivery sequence](docs/backlog-reconciliation.md) records historical dispositions, implementation evidence, and remaining standalone adoption work.

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

The consumer workflow exercises deterministic dependency resolution, npm metadata and artifact retrieval, exact multi-version dependency edges, verified archives, canonical `tapid.lock` generation, managed `node_modules`, and offline/frozen replay. Dependency lifecycle scripts are denied by default; exact checked-in approvals can permit supported hooks. This is a bounded npm-compatible subset, not full npm or pnpm compatibility.

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
- `outdated` to compare locked versions with registry metadata, `prune` to remove unreachable managed packages, and `why <PACKAGE>` to trace why a locked package is reachable (including `--json` output).
- A content-addressed local store and lockfile replay for offline installs, with transactional activation of managed `node_modules`.
- Safe archive extraction and integrity checks, plus generated package `bin` shims. Dependency lifecycle scripts are denied by default; [exact checked-in approvals](docs/dependency-lifecycle.md) can produce verified derived trees through Linux ManagedTree.

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

The Unix installer accepts `--shell auto|bash|zsh|fish|posix`; explicit selection overrides the `$SHELL` preference used by `auto`. Persistent startup-file changes require `--configure-path`. Without it, startup files remain unchanged and the installer prints a literal activation command. Unknown or unset auto preferences also leave startup files unchanged.

Use `--shell-mode login|interactive|both` to select startup behavior. Bash, Zsh, and fish default to `both`; POSIX supports only `login`. Bash login selection uses the first existing `.bash_profile`, `.bash_login`, or `.profile`, creating `.profile` if none exists; an unsafe higher-precedence file is rejected. Interactive Bash uses `.bashrc`. Zsh uses `.zprofile` for login and `.zshrc` for interactive shells, under an absolute exported `ZDOTDIR` when supplied, otherwise HOME. Empty or relative `ZDOTDIR` is rejected; the installer does not execute startup files to discover dynamic overrides.

Fish manages `$XDG_CONFIG_HOME/fish/config.fish`, defaulting to `$HOME/.config/fish/config.fish`. A supplied XDG directory must be absolute and non-empty. Persistence requires a native fish supporting `fish_add_path --path --prepend`; the installer probes capabilities without loading user configuration before downloading. Login and interactive guards restrict activation to the selected mode; `both` covers their union. No universal variables are modified. Changing an existing fish receipt’s mode requires uninstalling its owned block first. Fish itself represents empty PATH segments as `.` and supplies defaults for unset PATH; activation preserves that shell-provided representation.

Default and absolute custom install directories support spaces and shell metacharacters; colon and line-break characters are rejected before installation. New managed blocks prepend the literal directory only if absent, preserving unrelated PATH segments, including empty segments. Reading both startup files does not add the directory twice. The printed activation command updates the current shell directly without sourcing the entire startup file. An installer subprocess cannot activate its parent shell.

Newly written blocks have a private `.tapid-path-v1` receipt beside the executable. Two-profile installs preflight both targets and stage two numbered records before writing either block. Each record tracks pending/completed progress and exact before/after bytes; retries reconcile pending snapshots and preserve conflicting edits with an error. An unmarked equivalent export stays user-owned and is recorded as pre-existing. Uninstall uses recorded locations even after shell changes or a missing binary, removes only exact owned blocks, and preserves unrelated bytes. Edited blocks cause an error and retain ownership metadata for retry. Receipt readers reject unknown fields, symlinks, inconsistent snapshots, scalar fields over 64 KiB, and snapshots over 8 MiB. Startup files must be owned, writable regular files with no symlink ancestors.

Legacy one-profile receipts expand automatically to Bash/Zsh `both` mode when their recorded target is one of the selected files. The original payload, ownership and snapshots are preserved. A private `.tapid-path-previous-v1` recovery receipt bridges publication; install retries and uninstall reconcile either side of the rename before changing profiles. Conflicting target selections and unreceipted blocks in a two-profile install are refused. Legacy uninstall searches bounded default locations and validates the exact old payload. PowerShell requires `-ConfigurePath` for persistence and never silently changes process PATH. It compares fully qualified paths without rewriting existing tokens, preserving empty, relative, malformed, and environment-variable entries and their order. Literal paths compare without case, slash-direction, or trailing-separator differences; unresolved variables remain opaque. Windows receipts record raw before/after values, value presence, and registry kind. Windows directories containing `%` require manual activation because persisted environment expansion can change their meaning. Repeated installs reconcile pending writes; uninstall restores only an unchanged owned snapshot, including after a missing executable. Concurrent edits cause refusal and retain recovery metadata rather than guessing which token to remove. The full native platform matrix still requires Windows CI evidence under issue #37.

For Windows persistence, run `& $installer -ConfigurePath` after downloading the installer. New processes inherit their parent’s environment; a terminal launched from a stale parent may still need a fresh sign-in session. Read-back verifies stored configuration, not native resolution.

For example, opt in to Bash login configuration with `sh install.sh --configure-path --shell bash --shell-mode login` after downloading the installer. Local macOS probes cover automatic POSIX login startup and Bash, Zsh, and fish login and interactive startup with a custom directory; persistence read-back alone does not prove resolution on other platforms or with arbitrary user startup overrides.

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

Automation clients can use the compact [agent guide v1](docs/agent-guide.md) for command selection, JSON results, and recovery decisions.

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

Ordinary install reuses a matching lock. Changed manifests preserve compatible locked selections; `tapid update` without names requests a fresh graph; named updates preserve unrelated compatible pins. Frozen installation with a native lock keeps the exact graph and lock bytes, and can download missing pinned archives without resolving metadata. Imported npm locks preserve selections but may record verified tree receipts. Offline installation forbids downloads, including with `--frozen`. Native lock hydration requires a pinned HTTPS URL and registry-declared SHA-512 provenance, then verifies the archive and locked tree digest. Replay validates manifest digests, registry routes, platform contexts, tree markers, and managed output before atomic activation. `--store-dir PATH` selects another verified store root. See [locked installation decisions](docs/adr/0008-locked-install-selection-and-hydration.md).

## Inspect and clear the package cache

Use `tapid cache info` to see published package counts and logical byte sizes. `tapid clean` or `tapid cache clean` previews removal; add `--yes` to clear the recognized published cache without prompting. These commands work offline, support `--store-dir <PATH>` and global `--json`, and leave project files, staging, lifecycle keys, recovery state, and unrecognized entries intact.

Cleaning refuses competing replay or publication operations. Installed projects keep their files, but future offline or frozen replay may need the cache repopulated. See the [cache command contract](crates/tapid-cli/README.md#inspect-and-clear-the-package-cache) for scope, safety checks, and failure behavior.

## Supported subset and limitations

- npm package metadata with semver versions, package dependencies, and HTTPS tarball URLs is supported.
- npm aliases such as `"h3-v2": "npm:h3@2.0.1-rc.20"` preserve the local import name and the actual registry identity through install and frozen/offline replay. Alias targets accept supported semver ranges; dist-tags remain unsupported. See [alias behavior](docs/compatibility.md#npm-aliases).
- Range satisfaction is differentially tested against pinned `node-semver` 7.8.5 for exact and partial versions, `x`/`*` wildcards, comparators and intersections, caret/tilde (including zero-major bounds), hyphen ranges, `||` alternatives, prerelease eligibility, and ignored build metadata. This is not a claim of complete npm CLI or package-specifier compatibility; tags and git/file dependencies remain unsupported, and workspace declarations are limited to the documented forms.
- `add`, `remove`, and range-preserving `update` are available for the current package, with `--dev`, `--optional`, `--peer`, and explicit `--latest` mutation modes. `add --peer` records only a declaration; registry package peer requirements are validated against compatible direct project roots and recorded in peer contexts in lockfile/materialization identities. Missing or incompatible providers fail closed transactionally. Nested/ancestor peer-provider lookup and multiple contexts for one exact package instance remain unsupported. `outdated` reports direct lockfile selections, compatible and available versions, SemVer changes, and direct pin and declared range impact. It is read-only and stops with recovery guidance if an interrupted transaction is pending. Registry access is explicit in help; `--offline` reports unchecked registry versions as unknown. `prune` replays the validated workspace-root lockfile atomically, including with `--workspace`, to remove unreachable managed output; it does not mutate the selected member manifest. `add`, `remove`, `update`, and `outdated` use the `--project-dir` manifest by default, or a named member selected with `--workspace <name>` where applicable. npm-style workspaces support bounded discovery/globs, local package links, and root or selected-member operations with root-owned lockfile, store, and activation; scripts require explicit `tapid run`. Unsupported layouts and protocols fail closed before mutation. See the [compatibility matrix](docs/compatibility.md#compatibility-matrix).
- Lifecycle mutations are all-or-nothing across the manifest, lockfile, verified store, and managed `node_modules` activation. Resolution, integrity, archive, peer, workspace, and materialization failures preserve the prior state; verified trees are not committed to the shared store until project activation succeeds. Durable recovery journals let the next mutating lifecycle command restore the prior state after a crash before commit or finish cleanup after a committed operation.
- The live npm path requires registry-declared SHA-512 integrity by default and verifies downloaded bytes against that digest. This integrity check matches bytes to registry metadata; it does not authenticate the publisher, prove the user intended that package, or establish the archive's package identity independently of the metadata. The explicit `--allow-unverified-registry-artifacts` compatibility exception permits missing integrity and is online-only.
- Vulnerability intelligence, package malware scanning, publisher/provenance verification, and human audit attestations are not implemented. A verified archive is not necessarily safe or vulnerability-free.
- Dependency lifecycle scripts are denied by default. Exact checked-in approvals can run supported hooks through Linux ManagedTree; see [policy and platform requirements](docs/dependency-lifecycle.md).
- JSR support is experimental. Live JSR installation is not verified. A JSR artifact is accepted only when metadata supplies an HTTPS npm tarball URL and a valid SHA-512 SRI value. Tapid does not derive or trust integrity from transport bytes.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu, macOS, and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform is not evidence for another.
- ADR 0005 default-on, fail-closed CLI wiring and configuration parsing are integrated. macOS 26 Restricted execution is experimental and uses deprecated/private native Seatbelt APIs; Linux Restricted uses Landlock and seccomp and has targeted Ubuntu 24.04.5 x86_64 local-VM and hosted CI validation. Linux ManagedTree and resource-limit profiles require delegated cgroup v2 and private namespaces. macOS ManagedTree, Windows native containment, and the broader platform probe matrix remain unsupported or pending. Package-level malware scanning, package provenance verification, and independently authenticated client release metadata also remain unavailable.

## Contributing

Help us make Tapid work with more real projects. Useful contributions include:

- Trying it in a disposable copy of a Node.js project and reporting compatibility failures with a minimal reproduction, platform, and Tapid version.
- Improving installation instructions and examples when a step is unclear.
- Adding isolated package fixtures and regression tests for unsupported or failing cases.
- Working on Rust capabilities such as resolution, manifests, archives, and installed layouts.

Start with the [contributing guide](CONTRIBUTING.md) and [open issues](https://github.com/LimeTip/tapid/issues). For a substantial change, describe the problem and proposed approach before implementation. Report security vulnerabilities through [SECURITY.md](SECURITY.md).

## Development

Use Git, Rust with rustfmt and Clippy, and a supported Node.js major (26 preferred, 24, or 22). Run Cargo through the development wrapper to reuse build artifacts across worktrees. See the [testing guide](docs/testing.md) for focused checks and the full local lane.

Rust owns Tapid behavior and security verification. TypeScript owns developer
scripts, release orchestration, documentation checks, and website/consumer
fixtures, with tests run by Node. Python and uv are not project prerequisites.

Tapid supports every released Node.js major in Current, Active LTS, or Maintenance LTS status according to the [official Node.js release lifecycle](https://nodejs.org/en/about/previous-releases). End-of-life versions and prereleases are outside the support policy. The current supported majors are 26 (preferred), 24, and 22. Tapid itself is a Rust binary and does not require Node.js for installation commands.

Consumer CI checks use the latest release of every supported major on Linux, macOS, and Windows, with `check-latest: true` to avoid stale runner versions. Update the CI major-version matrix when Node.js releases a new Current version or a supported major reaches end of life.

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
