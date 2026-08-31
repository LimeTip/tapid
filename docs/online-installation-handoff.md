# Online installation status

The CLI supports bounded live npm metadata and artifact retrieval in addition to fixture-driven tests. Registry parsing remains in `tapid-registry-client`; the resolver consumes normalized metadata and performs no network or filesystem work.

## Current contract

- npm metadata requests `application/vnd.npm.install-v1+json` and is limited to 32 MiB.
- Artifact responses are separately limited to 512 MiB.
- Normal npm candidates require HTTPS tarball URLs and registry-declared SHA-512 integrity.
- Historical versions with unsupported dependency syntax or missing integrity are excluded without hiding otherwise usable versions.
- Exact package metadata HTTP 404 and narrowly validated unpublished tombstones produce no candidates. Other malformed or unsuccessful responses fail closed.
- Metadata is fetched incrementally for packages reached by the selected graph.
- Verified npm trees materialize from either a direct package root or exactly one named top-level wrapper containing `package.json`; missing and ambiguous roots are rejected before activation.
- Metadata and immutable artifact GETs retry a bounded set of transient failures, with three total attempts and deterministic 100 ms and 200 ms delays.
- Distinct parents can select different exact versions of one transitive package.
- Resolver root selections and exact dependency edges are preserved through lockfile construction, linking, and replay.
- Project activation uses an operating-system advisory lock and owner-marked staging directories. A later run reclaims only stages matching the prior unlocked owner's exact marker; live, malformed, oversized, symlinked, and ambiguous state fails closed.
- Lifecycle scripts remain disabled during installation.

The explicit `--allow-unverified-registry-artifacts` compatibility option can retain npm versions without declared integrity for an interactive online install. It emits a warning and cannot be combined with `--offline` or `--frozen`. It does not turn a locally computed digest into registry authentication.

## Remaining limitations

- Live JSR installation and integrity behavior are not verified.
- Full npm range, alias, tag, peer, optional dependency, workspace, private registry authentication, and platform-condition compatibility are incomplete.
- Metadata and artifact downloads remain sequential; retry delays and per-attempt timeouts are bounded but can extend a single resource fetch.
- Frozen replay does not yet implement every npm frozen-lockfile policy rule.

## Required release evidence

Before claiming broad npm compatibility, exercise the real Tapid binary against representative applications, verify `tapid.lock`, verified store trees, root and nested `node_modules` placement, offline and frozen replay, root scripts, lint, build, and deployment-related commands. Linux and Windows behavior requires completed CI or VM evidence in addition to local macOS checks.

## Verified store location

Tapid keeps its default verified store outside the consumer project so linters, build tools, file watchers, and deployment tooling do not traverse cached package source as application code:

- macOS: `$HOME/Library/Caches/tapid/store`
- Linux and other Unix platforms: `$XDG_CACHE_HOME/tapid/store`, or `$HOME/.cache/tapid/store` when `XDG_CACHE_HOME` is unset
- Windows: `%LOCALAPPDATA%\tapid\store`

The selected cache environment path must be absolute. An explicit `--store-dir` continues to override the default for isolated tests and deliberate custom layouts.

Older project-local `.tapid-store` directories are not silently moved or deleted. Run one successful online installation with the new default, then verify offline or frozen replay before removing the legacy directory. Use `--store-dir .tapid-store` only when deliberately replaying the legacy store.
