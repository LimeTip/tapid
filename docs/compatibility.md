# Compatibility matrix

This matrix describes the implemented contract boundary in the current release. “Supported” means the behavior is covered by the contract integration tests; it does not imply a full package-manager implementation.

| Contract | Implemented behavior | Compatibility / non-goal |
|---|---|---|
| Manifest | npm-shaped JSON parsing with typed name/version, dependency and script maps | npm lifecycle semantics and arbitrary manifest extensions are not executed |
| Registry snapshot | HTTPS registry identity, trimmed metadata, typed integrity, duplicate rejection, descending version normalization | No network protocol implementation or registry trust policy |
| Resolution | Deterministic highest matching candidate for the supported exact, caret, tilde and comparison ranges | Full npm semver (including prereleases, unions and tags) is not implemented |
| Archive | Bounded entry validation: traversal/absolute paths, duplicates, case collisions, special files, escaping symlinks and size limits | This validates entry metadata; it is not an archive extractor or malware scanner |
| Store | SHA-256 content-addressed ingestion, staging, idempotent activation and mismatch rejection | No garbage collection, remote cache, or multi-process lease protocol |
| Linker | Deterministic, context-aware materialization plan under an owned absolute root | The planner does not mutate files, create links, or provide process sandboxing |
| Policy / runner | Evidence is retained separately from a decision; unattended inferred/observed evidence fails closed; approvals bind artifact and normalized script | No authorization identity, user interaction UI, or OS containment implementation |
| Trust envelope | Canonical signing bytes and artifact/subject binding checks; unsigned and unsupported signatures are rejected | Cryptographic signing and verification are explicitly not implemented |
| npm consumer registry | `NpmRegistry` accepts a package name, reads npm `versions` metadata, requires matching `name`/`version` and `dist.tarball`, and maps optional `dist.integrity` (`sha512-…`) to a registry artifact; tarballs are fetched through the injected bounded transport | npm aliases, tags, git/file/workspace specs, packuments with non-semver versions, lifecycle scripts, and arbitrary npm metadata are unsupported |
| JSR consumer registry | `JsrRegistry` accepts scoped `@scope/name` only, reads `https://jsr.io/<scope>/<name>/meta.json`, accepts semver version keys, and maps artifacts to the corresponding `https://npm.jsr.io/~/scope__name/<version>.tgz` archive | unscoped JSR names, `jsr:`/npm specifier parsing, JSR exports/types/runtime metadata, provenance, and nonstandard artifact layouts are unsupported |
| Consumer transport boundary | Local/injected transports are supported for isolated tests; production transport is HTTPS-only, allow-listed to npm/JSR origins, rejects cross-origin redirects, and bounds response bodies | HTTP registries, credential forwarding, retries, mirrors, and live-registry integration tests are not supported |
| Consumer replay | Resolver metadata can include transitive dependencies; lockfile JSON is ordered/canonical, store trees are digest-addressed and marked with `.tapid-tree`, and offline/frozen modes reject network fetches | Full lockfile replay validation with dependency edges awaits the existing lockfile package-key parser fix |

All fixtures in `tests/integration/contracts.rs` are created under runtime temporary roots; no repository paths or secrets are assumed.
