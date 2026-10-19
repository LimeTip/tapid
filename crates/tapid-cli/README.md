# tapid

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid)](https://crates.io/crates/tapid)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid)](https://crates.io/crates/tapid)
[![Docs.rs](https://docs.rs/tapid/badge.svg)](https://docs.rs/tapid)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
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

Automation clients can use the compact [agent guide v1](../../docs/agent-guide.md) for command selection, JSON results, and recovery decisions.

## Commands

```text
tapid init [PATH]
tapid license
tapid explain <PACKAGE>@<VERSION> [--artifact-file <PATH>] [--json]
tapid manifest validate [PATH]
tapid lock verify
tapid import-package-lock <PATH>
tapid install [OPTIONS]
tapid ci [OPTIONS]
tapid add <PACKAGE>... [OPTIONS]
tapid remove <PACKAGE>... [OPTIONS]
tapid update [OPTIONS]
tapid outdated [OPTIONS]
tapid prune [OPTIONS]
tapid why <PACKAGE> [OPTIONS]
tapid upgrade [OPTIONS]
tapid tool install <PACKAGE> [--tools-dir <PATH>]
tapid tool list [--tools-dir <PATH>]
tapid tool update <PACKAGE> [--tools-dir <PATH>]
tapid tool remove <PACKAGE> [--tools-dir <PATH>]
tapid cache info [--store-dir <PATH>]
tapid cache clean [--dry-run | --yes] [--store-dir <PATH>]
tapid clean [--dry-run | --yes] [--store-dir <PATH>]
tapid run <SCRIPT> [PERMISSION FLAGS] [--node-runtime <PATH>] [--receipt-json | --result-file <PATH>] [-- <ARGS>...]
```

`tapid init` creates a private `package.json` without overwriting an existing file. Manifest and lock commands validate the selected files. Paths default to the current directory and `package.json` where applicable.

`tapid manifest validate` rejects symlinks and other non-regular files before
parsing. Root scripts perform the same check before loading run policy or
executing a script.

`tapid license` prints the complete Apache-2.0 license and LimeTip AB copyright attribution embedded in the executable. It works offline and does not require a project.

`tapid explain react@19.0.0` reads full npm metadata for an exact version using
the current directory's `tapid.toml` registry routing. It requires no manifest
and writes no project, lockfile, store, or installed files. Scoped names and
`npm:` prefixes are supported; ranges, tags, aliases, and JSR are unsupported.
Use `--project-dir` to select registry configuration, or `--registry-metadata`
to inspect a local full npm metadata JSON snapshot without network access.

The summary separates registry-reported SHA-512 integrity, signatures, and
attestation references from verified bytes. `--artifact-file package.tgz`
checks a local regular file against the reported digest without extracting it.
A mismatch exits 1; missing expected integrity remains unknown. Files are
bounded to 32 MiB for metadata and 512 MiB for artifacts. The command does not
download artifacts or attestations, verify publisher identity or provenance,
query vulnerability providers, scan for malware, or obtain human review. It
marks those checks and missing evidence explicitly. It records when Tapid fetched
live metadata, read a local snapshot, or checked local bytes. Registry-reported
publication and metadata modification times appear when present, after RFC 3339
validation, and remain unverified. Missing timestamps stay unavailable. These
times do not establish evidence freshness, which remains unknown.
A digest match does not establish package safety or
intended content. `--json` emits the versioned evidence summary, including on a
byte mismatch; see the [JSON protocol](../../docs/json-results.md).

`tapid i` is an alias for `tapid install`, including when adding a package. Use `tapid install --help` or `tapid help install` for installation help. Bare `help` and `install` package arguments are rejected before accessing the project to avoid accidental installs. To intentionally install a package with either name, use an explicit spec such as `help@1.0.0` or `npm:install`.

Dependency hooks remain denied without exact checked-in approval. Online installs offer separate invocation-only `--allow-unapproved-dependency-scripts` and `--unsafe-no-dependency-sandbox` overrides. The first retains required containment; the second explicitly permits host access but does not authorize unapproved hooks by itself. Override outputs never enter the authenticated build cache. See [dependency lifecycle scripts](../../docs/dependency-lifecycle.md) for limits, warnings, and platform requirements. These flags do not change `tapid run`.

## Standalone CLI tools

Install a tool without creating a project:

```text
tapid tool install typescript@^5
tapid tool list
tapid tool update typescript
tapid tool remove typescript
```

Each tool has its own generated private manifest, ordinary tapid.lock, and
dependency environment. Tools share the verified package cache. Only the
explicitly installed package's bin commands are exported; transitive commands
stay inside that tool's environment. npm names, scoped names, and supported
semver ranges are accepted. Dist-tags, npm aliases, and JSR tools are not
supported by this interface.

Update resolves within the requirement recorded at installation. To change that
requirement, run tool install again with a new spec. List reports the exact
version, requirement, command mappings, and environment/lock paths; --json uses
the existing versioned result protocol.

The default tool root is:
- Windows: %LOCALAPPDATA%/tapid/tools
- macOS: ~/Library/Application Support/tapid/tools
- Linux: $XDG_DATA_HOME/tapid/tools, or ~/.local/share/tapid/tools when XDG_DATA_HOME is unset or empty

Add the reported root's bin directory to PATH once using your shell's normal
configuration. Tapid does not edit shell profiles or the persistent environment.
For example, on Linux with the default location, the current shell can use
export PATH="$HOME/.local/share/tapid/tools/bin:$PATH". On Windows, add the
reported bin directory to your user Path and open a new terminal. --tools-dir
must be absolute and selects a separate installation. Install/update also accept
--store-dir and --registry-fixture for the same local fixture/cache seams as
project installation. Tool commands do not load the caller project's tapid.toml.

JavaScript entry points require an existing Node executable on PATH. Native
executables are also supported. Windows launchers select Node explicitly for
recognized Node shebangs; other script interpreters are rejected there. Unix
entry points use their interpreter/shebang or native executable. Command names
must contain only letters, numbers, dot, underscore, or hyphen. Launcher paths
must be UTF-8 and cannot contain double quotes or line breaks. Windows commands
are exposed through both .cmd and .ps1 files; shell execution policy still applies.

Tool commands run with your user permissions and retain the caller's working
directory. Dependency isolation does not sandbox execution, and does not change
tapid run's containment policy. Lifecycle scripts never run during tool
installation/update. Skipped hooks are reported; generated/native outputs may
therefore be missing. Additional runtime setup remains a separate command:
for example, after installing a compatible agent-browser package, explicitly
invoke agent-browser install to request its browser download. Tapid neither
performs that download implicitly nor claims the package is fully configured.

A new environment is verified before activation. Cross-tool command collisions,
Windows case/extension collisions, the reserved tapid command, the Unix node
runtime command (including case variants), and unowned
command files are rejected. Updates/removals verify recorded launcher bytes;
user replacements and unrecognized files/directories are preserved. A durable
journal selects the old catalog before commit and the new catalog after commit.
The next tool operation or invocation settles interrupted publication. List can
recover pending work and retire inactive owned environments.
Older Unix tools exporting `node` remain listable and removable, but execution
of that reserved command is rejected.

Environments record the OS, CPU, and libc used during installation. Moving a
tools directory to a different host platform requires update or reinstall before
execution. Older environments without this identity also require update or
reinstall; they remain listable and removable.
Tools can be migrated individually while other foreign environments remain
installed. Their recorded commands stay reserved against collisions, and execution
remains blocked until replacement. Foreign command names reserved on the new host
remain listable but do not receive launchers there.

Installed commands can start while an install or update stages its environment.
Catalog recovery, publication, and invocation lease acquisition share a short-held
lock. Invocations wait up to 30 seconds for that lock before reporting contention.

A running invocation leases its environment. Cleanup deferred by that lease or
an open Windows file is reported as committed_cleanup_pending in JSON and retried
by later tool operations. The lease covers the process waited for by the
launcher, not detached descendants: finish daemon sessions before replacing
their tool. Tool state and launchers must not be edited concurrently by other
host processes. Link/reparse checks reject static redirection; these checks are
not protection against a host process already allowed to replace managed paths.
Unknown or inconsistent recovery state fails closed and preserves files.

## Inspect and clear the package cache

`tapid cache` and `tapid cache info` report the package-store path, counts, and logical file bytes for published artifacts and marked package trees. They work offline without a project and do not create files or recover transactions. The default path is the same platform cache location used by installation. Use `--store-dir <PATH>` to inspect an explicit store. Human inspection and preview output labels sizes as logical bytes. Sizes exclude directory overhead, symlink targets, staging, and unrecognized entries. They measure file content lengths, not allocated disk space or predicted disk space reclaimed by cleaning. Hard links, compression, sparse files, and filesystem metadata can make those quantities differ.

`tapid clean` and `tapid cache clean` preview that scope by default. Pass `--yes` to delete the currently recognized published entries without prompting. `--dry-run` explicitly requests the preview and conflicts with `--yes`. All forms support global `--json` with schema version 1 results.

Cleaning preserves project `node_modules`, manifests, lockfiles, lifecycle authentication keys, staging, recovery journals, and unrecognized entries. Installed projects keep their materialized files. Future offline or frozen replay may fail until the required cache trees are populated again. Release-upgrade recovery data is outside this command's scope. This is explicit eviction of reusable package data, not automatic garbage collection based on which projects use it.

Maintenance uses the existing cross-process store lock and fails immediately when a competing publication or replay prevents safe access. Symlinked roots, cache directories, and lock files are rejected. Opened directories remain pinned during maintenance, and no-follow traversal prevents concurrent parent replacements from redirecting deletion. Symlink entries are preserved; links inside a recognized tree are never followed. A pending transaction must be recovered by installation before maintenance. A populated legacy store without a lock also requires an install before maintenance. A deletion or directory-sync failure stops cleanup and exits nonzero; some entries may already have been removed. Inspect the cache or retry after correcting the filesystem error. No background cleanup is scheduled. JSON reports these failures as `committed_cleanup_pending`; failures before eviction remain `unchanged`. This state describes a stopped, possibly partial cache eviction, not a queued cleanup task.

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

The fixture option is for local tests and air-gapped development. It is not a registry authentication or production mirror feature. The live npm path resolves supported transitive ranges, requires registry-declared SHA-512 integrity by default, selects compatible optional packages for the current OS/CPU/libc target, verifies extracted trees, writes schema 7 locks without derived outputs, schema 9 locks with approved lifecycle outputs, or schema 10 locks with copied file/Git sources, and stores trees in the platform cache outside the consumer project. `--allow-unverified-registry-artifacts` is an explicit online-only compatibility exception and emits a warning.

When that option is enabled, existing lock entries with locally computed integrity
are resolved and fetched again. Ordinary online installs still preserve compatible
registry-verified selections. Offline, frozen, and CI replay reject locally computed
integrity even when the artifact is cached.

`tapid why <PACKAGE>` reads the selected project's `package.json` and `tapid.lock` and lists deterministic dependency paths from direct roots to each reachable locked instance of that package. Use `--project-dir PATH` to select a project and `--workspace NAME` for a workspace member. It is strictly read-only and offline: it does not contact registries, recover transactions, inspect or modify the store, or materialize `node_modules`. Direct install roots retain their manifest kind (`dependencies`, `devDependencies`, or `optionalDependencies`); recorded package peer-context relationships are labeled `peer`, while transitive lockfile dependency edges are labeled `dependency` because the current schema does not preserve their original kind. The consuming project's own `peerDependencies` are declarations, not install roots. Registry identity, version, and workspace source are shown to disambiguate instances. A confirmed absent match prints an explicit message and exits with status `1`; a bounded search is reported as partial instead.

`tapid --json why PACKAGE` emits the version 1 JSON envelope with `schema_version: 1`, `operation: "why"`, `outcome`, `project`, `changes`, `warnings`, `errors`, `retry`, and a command-specific `data` object. The `data` object contains `package`, `paths`, `truncated`, `not_found`, and `graph_warnings`. Results are deterministic and bounded to 50 paths, 128 dependency levels, and 10,000 graph visits; `truncated: true` means the search is partial, so `not_found` is false and the command exits successfully even when no path was found. Transitive edge-kind limitations are surfaced in `data.graph_warnings`.

## Install and lifecycle outcomes

`outdated` compares each direct dependency's declared range and locked version with registry metadata. It reports the newest compatible version and newest available version, with SemVer change labels and direct lockfile pin impact for each. `available-manifest=changed` means the available version requires a different declared range. Peer declarations do not select direct lockfile pins, so their lockfile impact is unchanged even when metadata is unavailable. Missing metadata leaves other impact fields unknown. The command never recovers interrupted transactions or changes the manifest, lockfile, store, or node_modules. A pending transaction stops inspection with recovery guidance. Live registry access is the default. Use `--offline` to forbid registry access; without a fixture, registry versions and impact are unknown, while local workspace versions remain available.

Lockfile impact describes selecting the shown direct version. An unchanged direct pin does not establish that the whole lockfile would stay unchanged after `update`. Named updates preserve unrelated compatible pins, but selected requirements and peer constraints can require transitive changes. Transitive changes and resolution failures require full resolution. Major, minor, patch, and prerelease labels compare version numbers; they do not establish API compatibility. Registry "available" means the highest published SemVer version, including prereleases, rather than npm's `latest` tag.

`update react` refreshes the selected direct dependency while preserving unrelated valid locked roots and transitive versions. Selected requirements, peer constraints, and removal of unreachable packages can still change the transitive graph; update warnings identify affected package instances and their requiring parent edges. Omitting names refreshes the whole graph. Without a native lock, resolution starts from registry metadata.

`update` preserves declared ranges unless a save policy or `--latest` is explicit. Without these flags, it leaves the original `package.json` bytes unchanged. `--save-exact`, `--save-caret`, and `--save-tilde` save the resolved selected version as `19.1.0`, `^19.1.0`, or `~19.1.0`. The flags conflict with one another and also work without `--latest`, within the current range. `update --latest react` allows versions beyond the current range and defaults to saving a caret requirement. For example, `update --latest --save-exact react` saves an exact version. Each selected declaration retains its section and npm alias target. Workspace references and copied-source declarations retain their source syntax. A name declared in multiple sections is updated in every section where it appears. Other declarations stay unchanged.

`install`, `add`, `remove`, and `update` accept `--dry-run`. They report declaration edits and a resolved dependency graph, including source-qualified package identities, versions, integrity metadata, dependency edges, and the direct roots or parent edges that select each changed package. A version, peer-context, or platform-context change appears as removal of the old identity and addition of the new one. Named update previews preserve unrelated valid pins and report the resolved save requirements, including the default caret policy for `--latest`. Omitting package names refreshes the whole graph.

A dry run never writes project files, creates an activation lock, recovers transactions, reads or writes the package store, downloads package artifacts, or runs lifecycle scripts. Pending recovery stops the preview. Live resolution may request registry metadata; `--registry-fixture` resolves entirely from local fixture metadata. `install --dry-run --offline` and `--frozen` inspect a matching validated lock without checking whether its artifacts are cached. Output states the network permission, package metadata lookup count, and cache/artifact effects explicitly. A resolved graph does not attest artifact availability, archive validity, generated lifecycle outputs, or the success of a later install. Copied file/Git sources and imported npm locks currently require an ordinary install rather than a resolved preview.

`tapid --json add PACKAGE --dry-run` uses the version 1 command envelope with unchanged project state. `data.preview_kind` is `resolved_graph`; `data.declarations` lists section/name/before/after edits, `data.packages` lists package changes and reasons, `data.root_changes` lists root binding changes, and `data.effects` describes this preview's effects. This is an inspection result, not a saved executable plan. A later command reads current inputs and may resolve different metadata.

Lifecycle edits validate the manifest they were planned against after acquiring the project lock. An intervening semantic manifest edit fails with `PROJECT_CHANGED` before writing the override. This protects the planning-to-lock interval for cooperating commands; it does not protect against an uncooperative writer changing files during the transaction.

Install reads lifecycle policy and classifies the current lockfile after acquiring the project lock and completing recovery. If recovery restores a different lock format, replay and CI restrictions apply to that recovered lock.

`install`, `ci`, `add`, `remove`, `update`, `prune`, and `outdated` use typed application results. Failures retain an error category and print a `diagnostic:` code on stderr, such as `LOCKFILE_MISSING`, `LOCK_MANIFEST_MISMATCH`, `REGISTRY_AUTH_MISSING`, `RESOLUTION_FAILED`, or `INTEGRITY_MISMATCH`. Operational failures still exit with code `1`.

Results carry the effective project directory, affected project outputs, policy and recovery warnings, and retry advice. Dependency mutations distinguish unchanged state, successful rollback, committed changes, committed changes with cleanup pending, and recovery required. Output paths describe `package.json`, `tapid.lock`, and `node_modules`; shared-store effects are covered by the transaction state. A rollback clears those paths. Failed recovery retains the paths that need inspection. An `outdated` result is unchanged unless a pending transaction requires recovery. In that case it reports `recovery_required` and the paths to inspect without attempting recovery.

Human `install` output reports resolving, verification, store replay, and linking progress on terminal stderr, at most once per second within a phase plus phase transitions and completion. Redirected stderr and `--json` suppress progress. Successful installs report elapsed time, changed project paths, and added, changed, reused, and removed registry lock selections when a valid comparison is available. Counts compare exact lock records, including version and peer/platform context. A version replacement counts as an addition and removal; reused selections do not imply cache hits or skipped filesystem work. Missing prior locks count as empty. Invalid native locks fail installation. Imported locks omit the comparison because their records are not directly comparable to verified-tree lock records. Failures identify the operation phase, retain the diagnostic and safe error context, describe unchanged or rolled-back project files, and give known next steps without suggesting a repeat after commit.

A nonzero exit after commit does not mean the dependency change failed. Tapid reports that the change committed and warns against repeating the operation. Cleanup failures preserve the durable commit decision. If rollback cannot finish, Tapid reports recovery required and retains its journal for the next recovery attempt. Contention errors advise waiting for the competing operation. Diagnostic messages are limited to 4 KiB each, and HTTP URL user information, query values, and fragments are redacted. Use global `--json` for versioned machine results from these commands. Use `outdated --json --json-limit 0` to include every direct dependency. See the [JSON protocol](../../docs/json-results.md) for fields, truncation markers, lossless recovery paths, partial results, parsing errors, and command coverage.

## npm workspaces

Declare members with root `workspaces`, for example `["apps/*", "packages/*"]`.
Literal paths and `*` as a whole directory component are supported; unsupported
glob syntax fails before mutation. Ordinary semver dependencies on member names
link locally without registry fallback. `workspace:*`, `workspace:^`, and
`workspace:~` are also accepted as compatibility syntax.

Run these commands from the workspace root:

```text
tapid install
tapid install --workspace news
tapid add @example/ui@^1.0.0 --workspace news
tapid update --workspace news
tapid remove @example/ui --workspace news
tapid prune --workspace news
tapid install --offline --workspace news
tapid install --frozen --workspace news
tapid run dev --workspace news
```

Selection uses the exact member package name. Without `--workspace`, mutations
and scripts select the root. Install and prune activate the full graph using the
root lock and root `node_modules`; member scripts run from their directory using
root policy. Pass `--project-dir <root>` when running elsewhere. See the
[workspace contract](https://github.com/LimeTip/tapid/blob/main/docs/compatibility.md#npm-workspaces)
for discovery, replay, selection, and compatibility limits.

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

Do not put tokens in `package.json`, `tapid.toml`, command-line arguments, or `tapid.lock`. Offline and warm replay use the registry identities already pinned in the lockfile without credentials or registry requests. Frozen hydration uses credentials only for the configured origin of a missing pinned artifact. Changed registry routing fails explicitly. Registry credentials are excluded from root-script environments even if a run policy tries to allowlist the corresponding variable. Private-registry support is under development and is not a production-support claim.

## Legacy registry identities

Locks containing noncanonical persisted registry origins (such as uppercase hosts
or explicit `:443`) fail closed before activation/store mutation in offline and
frozen modes. Preserve a separate verified backup of `tapid.lock`, then deliberately
remove the incompatible original lock, then run online `tapid install` and review changed versions, artifacts and edges. The
online path replaces the lock after re-resolution, not identity migration. See
[compatibility and recovery](https://github.com/LimeTip/tapid/blob/main/docs/compatibility.md#persisted-registry-identity-compatibility).

## Install locked dependencies in CI

```text
tapid ci --project-dir ./example
tapid ci --offline --store-dir /absolute/path/to/verified-store
```

`ci` requires an ordinary verified-tree `tapid.lock` and matching root and workspace manifests. Imported npm schema 8 locks use `tapid install --frozen`; `ci` rejects them before mutation because their verification receipts can change during installation. It installs exact locked versions and dependency edges without version resolution or changes to `package.json` and `tapid.lock`. Existing verified store trees are reused; missing trees are downloaded from locked HTTPS URLs and checked against locked SHA-512 integrity and SHA-256 tree digests. Private registry downloads use the configured route and exact-origin credentials. Every registry package must have a locked artifact URL, including with a warm cache or `--offline`. Locks missing download URLs require regeneration with `tapid update` using live registry metadata and review of the resulting changes. Explicit `--registry-fixture` installs can supply local artifacts without locked URLs for tests and air-gapped development. This exception requires a readable fixture containing every URL-less locked registry, name, and version, even with a warm cache or `--offline`.

Installation uses atomic managed `node_modules` replacement and coordinated store publication. Validation or activation failure preserves the previous install when rollback succeeds. An unmanaged `node_modules` is rejected. Dependency lifecycle scripts do not run. `--offline` disables downloads and requires all trees in the store. Package arguments and the unverified-artifact exception are unavailable on `ci`.

## Offline and frozen

```text
tapid install --offline --project-dir ./example
tapid install --frozen --project-dir ./example
tapid install --offline --frozen --store-dir ./verified-store
```

Both flags require `tapid.lock` and matching root and workspace manifests. Replay validates exact package identities, configured registry routes, supported target contexts, tree digests, and regular `.tapid-tree` markers before staging. Activation replaces managed `node_modules` atomically.

With a native lock, `--frozen` can download a missing tree from its pinned HTTPS archive URL. It verifies registry-declared SHA-512 integrity, archive structure, and the locked canonical tree digest. It does not resolve metadata or rewrite the lock. Imported npm locks preserve selections but may record verified tree receipts. Missing URLs or provenance, corrupt stored trees, changed routing, incompatible platforms, and failed verification stop installation. `--offline` forbids downloads, including with `--frozen`.

Ordinary installation also replays a matching lock. Changed manifests retain compatible locked roots and transitive selections while resolving necessary changes. `tapid update` without names explicitly refreshes the graph; named updates preserve unrelated compatible pins. Unsupported or noncanonical locks require a verified backup and deliberate removal before generating a replacement. These are Tapid's supported lock semantics, not the complete npm frozen-lockfile policy.

## Experimental Node.js root-script runner and `.bin` handling

The invocation permission names are the same on Windows, macOS and Linux:

```text
tapid run test --allow-read . --allow-subprocess
tapid run build --allow-read . --allow-write .next --allow-write .tmp --allow-env CI --allow-subprocess
```

`--allow-read <PATH>`, `--allow-write <PATH>` and `--allow-env <NAME>` may be repeated. Filesystem paths use portable project-relative syntax with forward slashes; they are relative to the policy project root, including when `--workspace` selects a member. Environment arguments are names, not `NAME=VALUE` assignments. `PATH` stays controlled by runtime/tool resolution, and registry credential names remain forbidden. `--allow-subprocess` allows descendants under the same containment policy. `--allow-network` (alias `--allow-net`) requests the existing unrestricted network permission; it does not restrict networking by host, port or loopback address. Arguments after `--` remain script arguments and never grant permissions.

Supplying any of these permission flags enables invocation-only policy selection without requiring a script profile or creating/editing `tapid.toml`. With no configured policy, the baseline is Required/Restricted assurance with no project read, write, environment, network or subprocess grants. Backend runtime grants remain necessary and are disclosed separately. Node scripts normally need `--allow-read . --allow-subprocess` because the launcher uses a platform shell. Restricted does not promise complete descendant cleanup or ManagedTree resource guarantees.

An exact script profile, or explicitly configured `[run.defaults]` when there is no exact profile, remains a ceiling. A supplied read/write/environment list replaces that dimension with the selected narrower grants; omitted dimensions keep the configured grants. Flags cannot exceed configured filesystem scope, enable denied network/subprocess authority, weaken assurance or remove resource ceilings. To authorize broader access, deliberately change the configuration. Empty explicit defaults still preserve the legacy ManagedTree default. With no invocation permission flags, exact-profile requirements and existing configuration behavior are unchanged.

Names and policy selection are portable; enforcement availability still depends on the backend. For example, Windows rejects networking without the selected Node compatibility component and missing/unsafe write targets before execution. Prepare writable directories such as `.next` and `.tmp` before a Windows run. No unsupported request falls back to unrestricted execution.

Invocation-mode receipts add `policy_inputs` with the configuration source, invocation grants and effective project policy. These contain environment names, never values. `effective_filesystem` and established backend evidence describe resolved/runtime authority; input provenance alone is not enforcement evidence. The existing receipt channel and child exit/output behavior remain unchanged.

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

Paths are project-relative. Grant only the access the script needs: `network = false` requests network denial, while `network = true` allows unrestricted networking. Consult the platform scope for acceptance limits, including the Windows local-bind limitation. The backend also constructs a limited child environment and closes unrelated inherited descriptors.

Restricted is an authority boundary, **not** full process-tree management or a promise that arbitrary script code is safe. Tapid does not guarantee cleanup or termination of detached descendants. Configured tree-wide timeout, output, process-count, and memory limits require Linux ManagedTree. A requested restriction the backend cannot enforce causes the run to fail before the target starts; Tapid does not silently run it without containment. Linux Restricted uses Landlock and seccomp and requires kernel support; it has targeted Ubuntu 24.04.5 x86_64 validation. macOS Restricted is experimental and uses deprecated/private Seatbelt APIs.

Without invocation permission flags, the command requires checked-in `tapid.toml` and an exact `[run.scripts.<name>]` profile; `[run.defaults]` is merged only into that explicitly selected profile. `assurance = "restricted"` explicitly requests ADR 0005 **Restricted** execution. Omitting `assurance` in configuration retains the legacy-safe **ManagedTree** contract, which additionally requires race-free descendant ownership, complete cleanup/kill, and configured tree-wide timeout, output, process, and memory semantics. Unsupported required dimensions fail before the shell starts.

On Windows, explicit Node network grants use a private standard-user socket broker without administrator setup or firewall exemptions. Allowlist `SystemRoot` for Winsock provider initialization. The component adapts application socket imports and normal later addon loads; dynamically resolved APIs retain kernel containment without promised networking compatibility. Windows still has incomplete network-denial acceptance because the no-network AppContainer can bind locally. See [Windows scope](../../docs/windows-tapid-run-scope.md) for the tested configurations and remaining acceptance.

The command constructs a minimal environment rather than preserving inherited variables: `PATH` is reserved and cannot be allowlisted, while other declared names are retrieved individually from the caller only when present. Windows environment-name matching is case-insensitive and case-equivalent allowlist duplicates are rejected. The current `network` field is boolean: `true` grants unrestricted networking. `--hostname`, `--port`, and other forwarded application arguments do not constrain authority; declared listen/connect scopes remain future work.

`--node-runtime` is optional. Without it, Tapid examines at most 256 absolute entries from the invoking host's `PATH`, ignores empty or relative entries that could resolve through an untrusted working directory, and selects the first canonical, regular executable named `node` (`node.exe` on Windows). With it, Tapid validates that exact path instead. Host `PATH` is used only for this trusted preflight lookup and is never copied or appended to the child environment. On macOS the child receives a new `PATH` ordered as a private directory containing only a byte-verified `node` snapshot on a distinct inode, canonical project `node_modules/.bin`, then the runtime's canonical directory. Project commands win over ordinary runtime tools, while project-controlled `node` cannot shadow the verified executable. A selected runtime under project write authority is rejected. The runner creates a fresh unpredictable owner-only directory outside project write authority for each attempt and never reuses retained directories. It copies from a held runtime object, verifies exact bytes and executable metadata, and makes Seatbelt deny hard links and writes to the private snapshot while project writes remain allowed. It retains the snapshot whenever subprocess-enabled targets may have executed, including post-launch errors, because Restricted cleanup cannot prove descendants have exited. It removes the directory on pre-spawn and proven failed-exec paths. Successful `subprocess=false` executions remove it only after root exit, with Seatbelt denying process-fork. Private device/inode, mode, size, and link count are checked immediately before launch. Host writes and races after the final check remain residual risks. Other platforms retain their request construction but cannot execute a native sandbox. An absent project `.bin` is omitted from the search path and runtime grants. If present, it must be a real directory strictly beneath the canonical project root; unsafe `node_modules` parents, pre-existing symlinks, and canonical escapes are rejected. The macOS backend requires a relocatable Node binary, such as the official Node distribution; builds depending on executable-relative external libraries are not supported by the private snapshot. On Unix the runtime must have an executable bit.

On Unix, package scripts use `/bin/sh -c <script> tapid-script <forwarded-args...>` with `"$@"` boundaries and preserve native argument bytes. On Windows, the request uses `cmd.exe /D /S /C`, npm-compatible escaping for forwarded arguments, and a verbatim adapter boundary; CR/LF arguments are rejected. Configuration input is bounded before parsing. Unknown configuration, invalid project-relative paths, invalid environment names, reserved `PATH`, invalid runtimes, unsupported combinations, and unavailable guarantees fail before the shell starts. Project configuration cannot turn containment off. A noisy `--no-sandbox` outcome is planned only for trusted interactive projects, with no enforcement receipt, no silent fallback, and unattended rejection unless separately authorized; this CLI does not implement it.

The CLI uses `tapid-runner::ExecutionRequest` and checked execution exclusively for normal root scripts. Native backends own live child stdout/stderr streaming; the CLI does not replay captured bytes after completion. The checked launch receipt matches the exact requested enforcement and identifies each dimension's request, mechanism, assurance level, enforcement state, scope, and limitation; `CanonicalPath` is not `NativeObject`, and limit terminations remain distinct. Completion evidence describes lifecycle and cleanup results only and must not re-confirm launch-only authority. For Restricted, it must distinguish no cleanup guarantee from best-effort cleanup actually attempted or observed. Native macOS Restricted execution is evidence-gated. Linux ManagedTree can enforce all four configured resource limits when delegated cgroup v2 and private namespace prerequisites are available. Unsupported configurations fail closed before target code and issue no receipt. The Windows verbatim command boundary and all native backend behavior still require execution on their target operating systems.

On successful contained execution, stderr receives a receipt after streamed child output. Human output starts with `sandbox receipt:` and pretty-printed JSON. `--receipt-json` emits the same data as one compact JSON line, preceded by a newline even when child stderr did not end with one. Child stdout and stderr are streamed once and are not embedded or replayed in the receipt. Child output can forge receipt text, so stderr receipts are unsuitable for trusted machine results.

Use `--result-file PATH` for a separate schema-version-1 run result on both prelaunch failure and child completion. It suppresses the stderr receipt, conflicts with `--receipt-json`, and preserves child streams and exit codes. The destination must be a new file in an existing caller-controlled directory outside the canonical project root. Tapid rejects existing files and symlinks without overwriting them. Wait for Tapid to exit and require one complete newline-terminated JSON object; missing or partial results are transport failures. The result contains policy inputs when available and the existing checked receipt on completion. It adds no containment or cleanup guarantees. See the [result protocol](../../docs/json-results.md#run-child-output-and-receipts) for destination trust, failure codes, and interrupted-write handling.

Receipt schema version 1 contains `assurance`, `backend` with name/version/deprecation, `requested`, `declared`, `observed`, and `enforced` boolean dimension maps; `declared_evidence`, `observed_evidence`, and `established_evidence` arrays with dimension/scope/mechanism/limitations; `effective_filesystem`; `configured_limits`; `termination`; and `completion` with confirmed dimensions, evidence, and cleanup confidence. Each filesystem entry contains display `path`, lossless `native_path` with `encoding` and integer `units`, `access`, `kind`, `source`, and `binding`. Unix encoding is `unix-bytes`; Windows uses `windows-utf16`. Enum values use their Rust names. Project and runtime entries remain separate via `ProjectPolicy` and `BackendRuntime`. Limits use their configuration names and `null` for unconfigured values. No macOS configured limit is silently accepted. See [runner semantics](../tapid-runner/README.md) for exact directory data, global metadata, explicit devices, sampled probes, and cleanup limitations.

Install derives executable shims from verified package `bin` metadata. Unix uses symlinks. Windows writes `.cmd` and PowerShell wrappers. The planner rejects malformed metadata, absolute or traversal targets, symlink and special-file targets, collisions, and unsupported platforms. Root scripts remain arbitrary code and can use every explicitly granted capability.

## Lifecycle policy and limitations

- Dependency lifecycle scripts are denied by default. Exact checked-in approvals can build private derived trees using Linux ManagedTree; see [dependency lifecycle scripts](../../docs/dependency-lifecycle.md).
- Root scripts run only after the explicit `tapid run` command.
- Root-script execution is wired to fail-closed preflight. macOS 26 has an experimental Restricted backend using deprecated/private native Seatbelt APIs; Linux Restricted uses Landlock/seccomp and has targeted Ubuntu 24.04.5 x86_64 local-VM and hosted CI validation. Linux ManagedTree requires explicit cgroup delegation and private namespaces. macOS ManagedTree, Windows native containment, and the broader platform probe matrix remain unsupported or pending.
- Full npm CLI/package-specifier compatibility is not implemented. Copied project-root file tarballs and HTTPS Git pins are supported; directory links, SSH Git, preparation hooks, nested/ancestor peer lookup, automatic peer placement, and complete optional-dependency semantics remain unsupported. Tapid supports the documented bounded npm-style workspace subset; see the [compatibility matrix](https://github.com/LimeTip/tapid/blob/main/docs/compatibility.md#compatibility-matrix). Range satisfaction is differential-tested against pinned node-semver 7.8.5 for the documented grammar in `crates/tapid-resolver/README.md`.
- `add`, `remove`, `update`, `outdated`, and `prune` are implemented (see [Commands](#commands)); dependency script approval is available through exact checked-in lifecycle policies, while package publishing is outside this slice. Private-registry authentication is available as a development feature, not a production-support claim.
- JSR installation remains fail-closed unless metadata provides both an HTTPS npm tarball URL and a valid SHA-512 SRI value. Live JSR integrity behavior is unsupported and unverified.
- CI runs workspace and nested integration tests on Ubuntu, macOS, and Windows. Dedicated consumer validation runs on Ubuntu and Windows. The published v0.0.8 installers were also exercised through public installation and binary-execution smoke tests on all three operating systems. A local run on one platform does not prove behavior on another.

The macOS runner requires the binary's early private-launcher initializer. `sandbox-exec` launches that same executable under a parameter-bound profile. A fixed-size nonce/version READY/GO protocol, kqueue NOTE_EXEC, and CLOEXEC status EOF establish launch; target exit codes and stderr never do. Receipt `executable_resolution` reports exact Unix byte arrays for PATH and its ordered entries, `caller_path_inherited = false`, the distinct byte-verified private Node snapshot identity and validation timing, and cleanup observation. This reserves bare `node` and env-shebang resolution, not explicit paths to project executables.

For retained bindings, `executable_resolution.reserved_node.cleanup_observed` is `false`, and `limitations` explicitly describes retention. This field reports removal of the private snapshot, independently of best-effort process-group cleanup in `completion`. Retained directories and snapshots consume temporary storage until OS cleanup or host removal after every descendant exits. Tapid does not schedule deletion or reuse them. OS or host removal while descendants survive ends reserved-node protection. Host writes or races after final validation remain outside Restricted containment; retention provides no ManagedTree ownership or cleanup guarantee.

Npm dist-tags such as `latest` and `next` resolve to exact versions online, including tagged prereleases. Matching installs and frozen/offline replay retain the locked version after a tag moves; `update` refreshes the tag target. See [dist-tag behavior](../../docs/compatibility.md#registry-dist-tags).

Project-root `file:vendor/tool.tgz` dependencies copy contained regular tarballs;
`git+https://example.test/tool.git#main` dependencies pin a peeled commit and
archive digest. Copied sources use schema 10 and the existing verified-tree and
bin materialization paths. Matching and frozen installs retain the accepted pin,
`update` refreshes Git references, and replay rejects changed file bytes.
See [supported forms and limits](../../docs/compatibility.md#copied-file-tarballs-and-pinned-git-dependencies).

Npm aliases are supported in manifest dependencies and package arguments such as `tapid add 'h3-v2@npm:h3@2.0.1-rc.20'`. Scoped targets and supported semver ranges retain their actual registry identity and local import names during install and frozen/offline replay. See [alias behavior and limits](../../docs/compatibility.md#npm-aliases).

Existing npm projects can use `tapid import-package-lock <path>` to preserve supported npm v3 selections without resolution. Import is offline; the first frozen install verifies pinned tarballs. See the [migration and rollback guide](../../docs/npm-lockfile-import.md). Tapid manages packages and lockfiles; Node.js, workerd, Wrangler, and deployment tools keep their existing roles.

Windows contained runs with network access disabled require one administrator setup: `tapid sandbox install`. Later runs remain unattended as a standard user. Use `tapid sandbox status` to inspect the guard, or `tapid sandbox uninstall` from an administrator terminal when no executions or retained denial filters remain. The LocalSystem service is a persistent, exact-AppContainer-SID deny boundary; explicit network grants retain the existing standard-user broker. See [Windows execution scope](../../docs/windows-tapid-run-scope.md) for native acceptance status and limitations.
