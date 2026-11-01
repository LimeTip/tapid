# JSR installation

Tapid installs public scoped JSR packages through JSR's official npm compatibility
registry. This is a bounded, experimental compatibility feature. A live macOS run
on 2026-10-26 installed `@std/path@1.1.6` and `@std/internal@1.0.14`, executed
Node exports, and replayed the lock with a rejecting HTTPS proxy. The proxy saw
cold-install requests and zero replay requests. The
[live receipt](evidence/jsr-live-2026-10-26.json) records exact metadata, revision
URLs, declared/computed SHA-512, generated manifests, tree digests, and lock data.
This proves those versions and that platform/runtime combination, not all JSR
packages or native containment behavior.

## Declarations and runtime names

Use a scoped JSR key with a supported semver version or range:

```json
{"dependencies":{"jsr:@std/path":"1.1.6"}}
```

`tapid add 'jsr:@std/path@1.1.6'` saves that declaration. Import the direct
binding as `@std/path`, for example `import { join } from '@std/path'`.
JSR dist-tags and npm aliases under JSR declaration keys are rejected.

Generated transitive names such as `@jsr/std__internal` remain the runtime
bindings in the generated JavaScript. Tapid resolves that binding to logical
`https://jsr.io` / `@std/internal`, preserving its source separately from its
runtime name. Ordinary dependencies declared by JSR packages use configured npm
routing, including scoped registries. Equal npm and JSR names remain distinct.
Ambiguous or malformed generated names and generic npm aliases inside JSR
metadata are rejected, rather than redirected to another package source.

Regular, optional, platform, and root-provider peer behavior uses the existing
[bounded resolver contracts](../crates/tapid-resolver/README.md). Unsupported
peer placement fails before activation; JSR does not add automatic peer
installation or nested peer lookup. Native TypeScript loading, Deno runtime
semantics, publishing, and private JSR registries are outside this feature.

## Metadata and artifact trust

Tapid maps logical `@scope/name` to `@jsr/scope__name` only for official
`https://npm.jsr.io` compatibility transport. Metadata must identify the requested
generated package and each usable exact version. Each version must declare
SHA-512 SRI and a canonical HTTPS revision-specific archive URL on that same
origin, with the matching generated name and version. There is no npm fallback,
no missing-integrity compatibility exception for JSR, and no integrity inferred
from downloaded bytes or native JSR source-file checksums.

Compressed archive bytes must match the declared SHA-512. After bounded safe
extraction, the generated `package.json` must match the selected name, version,
and `_jsr_revision` from the archive URL. These checks precede verified-store
staging and project activation. Generated archive contents remain unchanged.
Transport retains existing response bounds, bounded retries, origin-restricted
redirects, and exact-origin credential rules.

The lock retains logical JSR identity, the exact archive URL (including revision),
registry-declared SRI provenance, verified tree digest, and named dependency
bindings. Existing schema 7 represents this without a new schema. Integrity binds
the archive to trusted registry metadata; it is not an independent publisher
signature. Registry compromise and upstream package deletion remain risks.

## Replay and failure behavior

Offline and frozen replay of a graph containing JSR require **every selected
locked tree** locally verified, including npm transitives. Missing or corrupt
content fails before network acquisition or publication. Keep a warm store;
ordinary online installation can reacquire pinned missing archives. npm-only
frozen hydration retains its existing behavior. See [ADR 0012](adr/0012-local-only-jsr-replay.md).

Deterministic tests cover metadata identity/origin/integrity rejection, generated
archive identity/revision rejection, explicit dependency routing, lock round-trip,
missing/corrupt mixed-graph replay with zero fetches, and later dependency failures
that preserve manifests, lock bytes, verified-store trees, and active layouts.
They complement the explicitly invoked [live test lane](testing.md#live-jsr-integration).
