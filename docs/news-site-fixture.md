# Synthetic news-site compatibility fixture

[`examples/news-site-consumer`](../examples/news-site-consumer) is a public, synthetic server-rendered Next.js/React/TypeScript application for evaluating package-manager compatibility on a representative news-site workload. The route at `/acceptance` returns the unique marker `TAPID_NEWS_SITE_ACCEPTANCE_V1`. Its npm-generated `package-lock.json` (lockfile v3) is the reference install. Direct dependencies and every floating transitive dependency are pinned to that reference through exact manifest versions and flat `overrides`; Tapid's native online resolver does not import the npm lock. This prevents later registry publications (including `caniuse-lite`) from changing just the Tapid side. Update those pins and the npm reference together deliberately, not during CI. [`tests/news_site_fixture_test.ts`](../tests/news_site_fixture_test.ts) verifies the entire locked dependency/optional-dependency closure and proves that the strict comparator rejects version/edge drift. Install that reference in a separate directory with `npm ci`, then run a clean Tapid install and frozen/offline replay in the fixture. [`scripts/compare-news-site-package-graphs.ts`](../scripts/compare-news-site-package-graphs.ts) compares reachable names/versions and dependency/peer edges, source origins, integrity, and platform-optional selections; it also reports physical-only packages even when unreachable. Tapid—not npm or a direct Node command—runs the fixture's `build`, `test`, and `start` scripts. Lockfile generation used Node.js v26.10.0 / npm 11.19.1; CI uses Ubuntu 24.04 / Node.js 22 and records its toolchain versions. The fixture contains no private code, customer information, credentials, or proprietary assets. This Bash procedure needs a source build, Node/npm, and a host eligible for the checked-in Restricted profiles. It describes main, not an executed v0.0.13 journey. From the repository root:

```bash
export CARGO_TARGET_DIR="$PWD/target"
node --experimental-strip-types scripts/dev.ts build --locked --bin tapid
tapid_target="$(node --experimental-strip-types scripts/dev.ts metadata --no-deps --format-version 1 --locked | node -p "JSON.parse(require('fs').readFileSync(0, 'utf8')).target_directory")"
export TAPID_FIXTURE_BINARY="$tapid_target/debug/tapid"
cd examples/news-site-consumer
npm_reference="$(mktemp -d)"
cp package.json package-lock.json "$npm_reference/"
npm ci --prefix "$npm_reference"
mkdir -p .next .tmp
tapid() { "$TAPID_FIXTURE_BINARY" "$@"; }
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

This synthetic source fixture is not an external pilot or a published-release receipt. See [evaluation and rollback](npm-lockfile-import.md), [production gates](production-adoption.md), and the [claim audit](adoption-claims-audit.md).
