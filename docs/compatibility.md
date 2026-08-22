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
| Publishing | Runtime filesystem manifests, sorted paths, reproducible pack bytes, preview-before-promote and immutable versions | No registry transport, credentials, or server-side publish semantics |

All fixtures in `tests/integration/contracts.rs` are created under runtime temporary roots; no repository paths or secrets are assumed.
