# Threat model update

## Scope and trust boundaries

Tapid treats manifests, registry metadata, package archives, filesystem trees, policy evidence, approvals, and trust envelopes as separate boundaries. Inputs crossing a boundary are typed or validated before the next contract consumes them. The integration path intentionally uses runtime-derived temporary fixtures so tests do not accidentally trust checked-in paths or ambient state.

## Implemented defenses

- **Manifest and registry input:** typed names, versions, registry origins and integrity values; malformed metadata and duplicate versions are rejected. Normalization gives deterministic candidate ordering.
- **Dependency selection:** resolution is deterministic and registry-scoped, preventing an identically named package from another registry from satisfying a dependency.
- **Archive extraction boundary:** entry paths are checked for traversal, absolute and drive-prefixed forms, duplicates, case collisions, hostile symlink targets, special files, and resource limits before use.
- **Artifact store:** bytes are streamed to a private staging file, hashed, and activated only at the expected digest. Existing regular files are authoritative and digest mismatches do not activate partial content.
- **Link planning:** planned targets are derived under an absolute managed root, include peer/platform context, and are sorted deterministically. The planner only describes operations.
- **Evidence versus policy:** evidence is preserved as observations/claims; policy produces a separate decision and reason set. A warning or denial cannot be represented as absent evidence. Unattended inferred/observed evidence fails closed, and runner approvals bind both artifact digest and normalized script hash.
- **Trust artifacts:** canonical envelope bytes bind version, subject, artifact digest, and claims. `verify` rejects unsigned envelopes and does not claim that an unsupported/future algorithm is valid; subject and artifact mismatches are rejected before algorithm handling.
- **Publishing:** file manifests and packed bytes are runtime-derived, sorted, digest-bound, previewable without transport side effects, and immutable after promotion.

## Non-goals and residual risks

- No cryptographic signature implementation, key management, certificate/revocation model, or transparency-log verification exists yet. A valid-looking unsigned envelope is not trust evidence.
- No archive decompression, executable scanning, runtime sandbox, capability enforcement, or OS-level containment is provided by these contracts.
- Registry transport, TLS/server authenticity, cache eviction, concurrency leases, and publication authorization remain outside this release.
- Supported resolution ranges are intentionally narrower than npm semver; unsupported syntax must not be treated as equivalent to a verified resolution.
- Policy is deterministic decision plumbing, not an identity-aware authorization system. Approval possession and user intent are outside the runner contract.

The integration tests are regression coverage for these implemented guarantees, not evidence that the non-goals are solved.
