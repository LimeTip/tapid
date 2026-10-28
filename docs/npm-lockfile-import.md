# Importing an npm lockfile

`tapid import-package-lock <path>` imports npm lockfileVersion 3 into `tapid.lock` in the current project. This is a one-time migration. It reads `package.json` and the npm lock offline, preserves selected versions, tarball URLs, SHA-512 integrity, dependency and peer placements, and optional/platform constraints. It does not resolve dependencies, contact a registry, change `package.json`, touch the store, or replace `node_modules`.

Tapid manages packages and lockfiles. It is not a runtime and does not replace Node.js, workerd, Wrangler, or deployment tools. npm is not required to import a v3 lock or to install and replay the resulting Tapid lock.

## Evaluate an existing application reversibly

Use two isolated checkouts of the same immutable application revision: `npm-baseline` and `tapid-evaluation`. Record the source URL, commit, license, OS/architecture, Node/npm versions, Tapid source revision and binary SHA-256. Use the application's documented toolchain. Retain the original npm lock, all matching root/member manifests, and any previous Tapid lock outside both checkouts. Keep credentials and stores out of evidence reports. The [evaluation index and report template](evaluations/npm-migration/README.md) distinguish attempted checks from successful acceptance.

1. In `npm-baseline`, run `npm ci` with the application's normal lifecycle behavior. Run its documented build, tests and a bounded start/readiness check. Record missing scripts, services and prerequisites explicitly. A failed baseline makes application comparison inconclusive.
2. Start `tapid-evaluation` without npm-owned root or member `node_modules` trees. Select a fresh isolated store and an isolated home/configuration. Record `tapid --json status` before mutation. Status is local compatibility inspection; it does not establish store validity, lifecycle compatibility or application readiness.
3. Import and verify using the commands below. An unsupported diagnostic is a stopping condition for dependent checks. Retain its JSON pointer, package identity, field/reason, and before/after hashes of manifests and locks. Check that the store and installed trees are unchanged. Do not delete unsupported metadata or refresh npm selections to turn that attempt into success.
4. Install frozen, then replay offline with the same store. Preserve the imported lock before and after hydration. Receipt changes are verification evidence, not version drift.

```sh
# In tapid-evaluation; EVALUATION_STORE is an absolute, disposable store path.
tapid --json status
tapid import-package-lock ./package-lock.json
tapid lock verify
tapid install --frozen --store-dir "$EVALUATION_STORE"
tapid install --offline --frozen --store-dir "$EVALUATION_STORE"
```

From the Tapid source checkout, compare the installed graphs before application execution:

```sh
node --experimental-strip-types scripts/compare-news-site-package-graphs.ts \
  --npm-root "$NPM_BASELINE" --tapid-root "$TAPID_EVALUATION" \
  --json "$EVIDENCE_DIR/graph.json" --text "$EVIDENCE_DIR/graph.md"
```

Exit 0 means equality under the comparator's supported evidence model; exit 1 means differences or incomplete instance evidence; exit 2 means an input/execution error. Schema 2 reports contextual dependency/peer bindings, exact artifact URLs and integrities, aggregate differences and receipt counts separately. It handles cycles and layout-only hoisting differences. It currently rejects package links, including workspace links; native locks with multiple candidate records for an installed name/version report ambiguous association. These are comparator limits, not proof of importer incompatibility. Do not claim workspace parity or general native-maintenance stability from this helper yet. Optional/platform differences require review against explicit constraints; an arbitrary missing optional package is not a platform exemption.

Run the same application build, tests and bounded readiness check on the Tapid tree using its existing runtime/tools. Record the command, working directory, exit and readiness result independently of graph parity. **Imported replay never executes dependency hooks, even after an approval draft.** Hook-dependent applications may therefore fail despite equal selections. Successful import does not certify application behavior, later dependency mutations or deployment.

Before maintenance, explain the conversion risk: `add` and named `update` leave imported replay and invoke native resolution, whose peer and member/workspace support is narrower. Keep independent disposable copies of the successfully imported state for one exact add target and one named update target. Record expected direct changes, requiring edges for necessary transitive changes and all unrelated compatible selections. Save pre-operation manifests, locks and semantic evidence. The retained npm lock describes the old state after conversion; comparing against it is not sufficient proof of maintenance stability. Until explicit native before/after comparison is available, mark that result unavailable. Never automatically retry with fresh resolution or weaker verification. Replay successful native results frozen and offline.

Finally demonstrate rollback after actual Tapid activation: restore the retained matching root/member manifests and locks, remove or restore the appropriate root/member installed trees, run the retained `npm ci` path, and repeat a previously passing application check. Reinstalling npm after a rejected import proves baseline recovery only; it is not evidence of rollback from an activated Tapid tree. Keep the baseline available throughout.

## Migration

Keep a backup or a version-control copy of `package.json`, `package-lock.json`, and any existing `tapid.lock` before importing. Use a clean project with no interrupted Tapid transaction.

```sh
tapid import-package-lock ./package-lock.json
tapid lock verify
```

The npm root dependency sections must match `package.json`. If the npm lock is older than v3 or stale, refresh it with npm before importing, then review the resulting dependency changes separately. For example, `npm install --package-lock-only --lockfile-version=3` creates a v3 lock, but can also change selected versions. The importer will not perform this refresh for you.

For private registries, configure the matching origin in `tapid.toml` before importing. The importer treats the origin of each pinned tarball URL as its registry identity. It refuses a mismatch with Tapid routing. It never applies npm's implicit substitution of the public registry hostname with a configured mirror. Credentials remain separate environment-backed Tapid configuration and are not read during import. See [registry routing](../crates/tapid-cli/README.md#private-npm-registry-routing-development-feature).

An existing npm `node_modules` tree needs an explicit ownership opt-in before Tapid can replace it. Move or remove that tree yourself, or create a regular `.tapid-managed` file containing exactly `tapid-managed-v1` followed by a newline after backing it up. Then run:

```sh
tapid install --frozen
```

The first frozen install can download the exact pinned tarballs. It does not fetch registry metadata or choose versions. Tapid verifies SHA-512, archive safety, and package name/version before recording verified tree digests. Store publication, lockfile receipts, and root and member `node_modules` activation share one durable install decision. Tapid stages every layout before activation and keeps previous trees until commit. Recovery restores all trees and receipts after an interrupted prepared transaction, or keeps all committed layouts and removes their backups. The directory replacements occur sequentially; after a crash, run Tapid to complete recovery before using the project. Dependency lifecycle scripts remain disabled.

Once the applicable packages are verified in the selected store, replay needs no network:

```sh
tapid install --offline --frozen
```

`tapid ci` requires an ordinary verified-tree lock and rejects imported schema 8 locks before mutation. Use `tapid install --frozen` for imported locks, whose verification receipts can change during installation.

A plain `tapid install` also preserves an imported graph. Explicit dependency mutations such as `add`, `remove`, and `update` use Tapid's ordinary resolver and write an ordinary Tapid lock. Those operations can change selections and retain the ordinary resolver's narrower peer semantics. `outdated` currently requires an ordinary verified-tree lock rather than an imported lock.

## Supported subset

The importer accepts v3 `packages` entries for registry tarballs with canonical, credential-free HTTPS URLs, a conventional package/version tarball path, and one canonical padded SHA-512 integrity value. It supports nested duplicate versions, npm aliases with explicit actual package names, already selected ancestor peer providers, and distinct peer contexts when the resulting Tapid instance graph can represent them. It retains ordinary npm descriptive metadata, including license, engines, funding, deprecation, bin metadata, and install-script flags. Retaining engines metadata does not add runtime engine enforcement.

Workspace imports accept `node_modules/<name>` links at the root or directly inside declared, contained workspace members. Member names, versions, dependency sections, peer metadata, and bins must match the npm lock. Bin comparison accepts equivalent string and object forms, including npm's conversion of a string bin to a command named after the unscoped package name. Tapid records each member manifest digest and rejects changed manifests or membership on replay. Tapid preserves these local link placements, including links to members not named in root dependencies. Member registry dependencies and peers can use root-hoisted, member-local, and nested registry placements. Different members can select different versions of the same dependency. Selection follows npm ancestor lookup and checks the nearest provider against the declared requirement. Selected local member dependencies must satisfy their declared ranges.

Remove unmanaged `node_modules` trees inside members or between a member and the workspace root before import and replay. Tapid accepts regular member trees it owns through a valid `.tapid-managed` marker and replaces them during installation. It rejects symlinked trees and trees in undeclared ancestor directories. Import leaves every tree untouched. When a dependency mutation converts the imported lock to native resolution, installation clears prior managed member selections so they cannot override the new root selections.

Renamed local links, local links inside registry packages, links to undeclared targets, and registry dependencies on local members remain unsupported and fail explicitly.

Git/file sources, SHA-1 or multiple SRI values, bundled packages, unknown fields, unreachable entries, self-instance edges, and conflicting placements for the same Tapid instance fail before writing. A diagnostic identifies the JSON pointer, package, field, and reason. Duplicate JSON keys are rejected. Unsupported versions include instructions for producing a v3 lock.

The importer does not reproduce npm's physical hoisting layout. Tapid's managed layout binds the exact selected dependency and peer instances by their local import names. The committed reference fixture checks nested versions and peer lookup with Node, compares selected package versions with offline `npm ci`, and checks byte-stable import output. Workspace tests check scoped links, member-level links and bins, conflicting member versions, ancestor peer lookup, optional omission, frozen installation, offline replay, native conversion, and rollback across root and member trees.

## Optional packages and platforms

The imported lock keeps all selected platform variants and their npm `os`, `cpu`, and `libc` constraints. Each install selects the applicable graph for the current platform without choosing another version. Incompatible optional branches are skipped. A required incompatible package fails, including a required child of an otherwise applicable optional package. Tapid aborts instead of installing that optional parent with a missing required dependency. A platform switch can require another frozen install to fetch the previously unverified variants. Offline replay fails if those verified trees are unavailable.

Tapid differs from `npm ci` when an applicable optional package fails download, integrity, extraction, or identity verification. Tapid aborts the whole install instead of silently accepting that failure. It also disables dependency scripts, which can make native or generated-code packages unusable even when their selected versions agree with npm. The fixture uses a deliberately unsupported OS to check deterministic optional omission on every test platform.

## Rollback

Import failures leave root and member manifests, the previous lock, store, and active dependencies unchanged. Import refuses pending install recovery instead of attempting it. Restore your saved `tapid.lock` to undo a successful import. Import itself has not changed `node_modules`.

After an install, restore the saved lock and matching manifest, then replay their verified trees, or restore the saved npm project and run `npm ci`. Keep the npm lock backup until you have checked your application with its chosen runtime and tools.

Imported locks use Tapid schema 8, separately from ordinary schema 7 verified-tree locks. Schema 8 retains the npm selections and stores tree verification receipts by package placement, bound to each receipt's tarball URL and integrity value. Older Tapid clients reject this schema. Receipts can change on the first frozen install; selected versions, sources, integrity values, and constraints do not. See [ADR 0008](adr/0008-offline-npm-lock-import.md) and the [npm lockfile format](https://docs.npmjs.com/cli/v11/configuring-npm/package-lock-json/).
