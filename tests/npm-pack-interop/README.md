# Offline npm packing interoperability

This private fixture pins npm 10.9.2 with a committed integrity-bearing lockfile.
Provision explicitly before running the Rust interoperability lane:

```sh
npm ci --prefix tests/npm-pack-interop --ignore-scripts --no-audit --no-fund
TAPID_TEST_NODE="$(node -p 'process.execPath')" node --experimental-strip-types scripts/dev.ts test -p tapid-publish --test npm_interop --locked -- --ignored
```

Runtime assertions invoke the fixture-local CLI using `TAPID_TEST_NODE` (an
absolute Node executable). They never provision packages or select a global npm.
Installation is offline with scripts/audit/funding disabled and isolated homes,
configuration, caches, and temporary directories. A missing fixture fails the
explicit lane rather than skipping assertions.
