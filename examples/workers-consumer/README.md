# Workers package-management consumer

This synthetic TypeScript Worker checks one package-management combination for
[#166](https://github.com/LimeTip/tapid/issues/166). Tapid selects, verifies and
installs dependencies. Node runs TypeScript and Wrangler; **workerd executes the
Worker**. Wrangler owns bundling, local development and deployment. The harness
never deploys and needs no account, credentials, bindings or remote application.

The exact response is `TAPID_WORKERS_ACCEPTANCE_V1`, returned through the installed
`escape-html` runtime dependency. This is a deliberately small application, not
general Workers or npm compatibility certification.

Pinned inputs: Node **22.23.3**, npm baseline **11.12.1**, Wrangler **4.153.0**,
TypeScript **5.8.3**, Workers types **5.20261023.1**, compatibility date
**2026-10-23**. The committed npm v3 lock selects workerd **1.20261023.1** and
esbuild **0.28.2**. Exact overrides pin transitive ranges to the reviewed baseline;
`unenv` is also a direct dev dependency to satisfy native Tapid's bounded peer
provider contract. There are no conflicting global overrides in this graph.
npm 11 retains libc constraints in v3 locks. Regeneration starts without a lock
or installed tree so prior platform-specific layouts cannot prune the baseline.

From the repository root, with the pinned Node on PATH:

```sh
export CARGO_TARGET_DIR=target
node --experimental-strip-types scripts/dev.ts build --locked --bin tapid
node --experimental-strip-types scripts/dev.ts metadata --no-deps --format-version 1 --locked
# Read target_directory from metadata; supply its debug/tapid executable below.
workers_tools_dir="$(mktemp -d)"
npm install --prefix "$workers_tools_dir" --ignore-scripts --no-fund npm@11.12.1
node --experimental-strip-types scripts/check-workers-consumer.ts \
  --binary /absolute/path/from/metadata/debug/tapid \
  --npm-cli "$workers_tools_dir/node_modules/npm/bin/npm-cli.js" \
  --artifacts "$workers_tools_dir/evidence"
```

The harness checks npm's version before `npm ci` (baseline lifecycle hooks are
allowed). It creates independent temporary npm, native Tapid and npm-import
projects; the two Tapid projects have separate homes and stores. It deletes each
installed layout before frozen and offline frozen replay, checks unchanged input
and lock hashes, and proves an empty-store offline failure preserves inputs and
leaves no installed layout. No npm-produced files enter a Tapid tree.

After frozen and offline replay, each Tapid tree must pass graph comparison,
`node node_modules/typescript/bin/tsc --noEmit`,
`node node_modules/wrangler/bin/wrangler.js deploy --dry-run --outdir dist`, and
`node node_modules/wrangler/bin/wrangler.js dev --local`. The harness bounds
startup, HTTP requests and output, asserts status 200 and the exact body, and
kills the server process group in cleanup. CLI tools are invoked directly, with
no npx or implicit package install. It clears inherited credentials and binary
overrides and disables Wrangler telemetry.

CI's reference lane is Ubuntu 24.04 x86_64/glibc. Its uploaded evidence contains
phase commands/results, runtime logs, exact response receipts, generated Tapid
locks, input hashes, toolchain pins and JSON/text graph reports, including on
failure. A configured lane alone is not passing evidence: consult its exact-source
run. Native macOS/Windows and cloud deployment are outside this initial claim.

## Baseline regeneration

Use the pinned npm CLI above. In this fixture directory run:

```sh
workers_baseline_dir="$(mktemp -d)"
cp package.json "$workers_baseline_dir/package.json"
(cd "$workers_baseline_dir" && node /path/to/npm-11.12.1/bin/npm-cli.js install --package-lock-only --ignore-scripts --no-fund)
cp "$workers_baseline_dir/package-lock.json" ./package-lock.json
```

Review package URLs, integrity, engines and lifecycle declarations in the lock.
When changing pins, review all transitive overrides too; the fixture tests reject
floating native selections and conflicting versions. Do not copy npm-generated
binaries or manually run skipped hooks to make Tapid pass. The native lock is
generated from clean inputs on each run and retained with evidence.

## Migration and rollback

Back up `package.json`, `package-lock.json`, any prior `tapid.lock` and installed
layout. In a fresh copy of the project:

```sh
tapid import-package-lock ./package-lock.json
tapid install --frozen
# Remove only the disposable managed node_modules to demonstrate reconstruction.
tapid install --offline --frozen
```

Import itself is offline. First hydration may add placement-bound verification
receipts, but cannot change npm selections, URLs, integrity, edges or constraints.
Later replays must keep the hydrated lock unchanged. npm is needed only to
reproduce the comparison baseline, never for Tapid import or replay. Restore the
saved matching manifest/lock and replay to roll back, or restore the npm project
and use pinned `npm ci`. See [npm migration](../../docs/npm-lockfile-import.md),
[#151](https://github.com/LimeTip/tapid/issues/151) and compatibility foundations
[#152](https://github.com/LimeTip/tapid/issues/152),
[#153](https://github.com/LimeTip/tapid/issues/153),
[#154](https://github.com/LimeTip/tapid/issues/154).

## Explicit limits

Dependency hooks remain denied, including esbuild/workerd postinstall and sharp's
install hook. Their published optional binary packages must supply usable binaries;
no hook execution, integrity exception or fallback download is permitted. Offline
replay certifies installation from verified artifacts, not all later Wrangler
network behavior. Wrangler can perform update checks; telemetry is disabled.

The npm lock includes foreign OS/CPU/libc optional binaries. Reports inventory
omitted lock records and their constraints; applicable omissions and unexplained
graph/provenance differences fail. Tapid aborts applicable optional artifact
verification failures where npm may skip them. Different physical hoisting is
allowed; selected logical identities, peers, exact tarball URLs, integrity and
edges must agree. Registry-declared integrity and verified receipts stay distinct.
Git/file sources, bundled dependencies, arbitrary lifecycle builds and unsupported
peer topologies are not established by this fixture. Runtime assumptions are
loopback networking, Unix process groups and compatible packaged native binaries.
