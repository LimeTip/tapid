# ADR 0001: Provider-neutral stable updates

Status: Superseded by ADR 0004 for current client releases
Date: 2026-08-27

## Decision

Tapid has one official update channel: `stable`. There is no separate preview channel. The latest source commit is the preview/development path and is available only through the explicit development installer with `--source-ref`.

The stable installation and `tapid upgrade` flow use a small, long-lived bootstrap or discovery layer and a signed release channel manifest. The bootstrap layer is not the trust root and must not contain release-specific logic. It discovers the current approved stable release, while the client verifies the signed release manifest before downloading or installing any artifact.

The release manifest is signed with Ed25519 and binds the Tapid product identity, version, immutable commit, release sequence, target artifact, URL, SHA-256 digest, size, timestamps, and optional SBOM and provenance references. GitHub Releases is the initial artifact provider, but provider selection is metadata, not product identity. GitLab, object storage, a CDN, or another provider may be introduced later without changing the client trust model.

## Availability and recovery requirements

1. A stable release is built, tested on supported platforms, checksummed, supplied with SBOM and provenance, signed, and manually approved before the stable channel is advanced.
2. Stable installation never falls back to source or to an unsigned artifact.
3. The trusted public key material is shipped with the verifier or client, versioned in the repository, and documented with a fingerprint. Users do not need to type or remember it.
4. Private signing keys are kept outside source control in protected release infrastructure with encrypted offline backup and a documented recovery procedure.
5. Key rotation is explicit. Unknown keys, invalid signatures, stale metadata, and unsafe downgrades fail closed.
6. Release manifests and artifacts are replicated where practical. Losing one manifest copy must not invalidate a release. A manifest can be regenerated from the immutable release artifact metadata and re-signed through the controlled release process.
7. The client knows more than one discovery or artifact endpoint. A temporary DNS, domain, GitHub, or provider outage must not disable updates permanently. The last verified release remains usable and is cached locally.
8. `tapid.dev` remains a supported endpoint if possible. If `tapid.com` is acquired later, it is added as an endpoint or migration path, not used to silently replace the trust root. Existing clients must continue to work during migration.
9. Release sequence and freshness metadata prevent replay of old channel metadata. Downgrades require an explicit, separately authorized operation.
10. All release publication and update behavior must have failure-path tests, including endpoint outage, provider migration, key rotation, missing manifest copies, signature mismatch, digest mismatch, and interrupted installation.

## Planned implementation order

1. Keep the release manifest schema and Ed25519 primitives strict and independently tested.
2. Add versioned trusted-key material and a release preflight that generates and verifies manifests without manual metadata editing.
3. Add the stable channel manifest and provider-neutral endpoint discovery with fallback and last-known-good state.
4. Add the verified stable installer and atomic replacement/recovery behavior.
5. Add `tapid upgrade` only after the verified release path is exercised end to end.
6. Add protected release CI with platform validation, SBOM, provenance, signing, manual approval, and publication checks.
7. Add domain/provider migration tests and a documented key-rotation and recovery exercise before advertising production updates.

## Non-goals

- No separate preview release channel.
- No silent source fallback from stable installation or upgrade.
- No trust based solely on DNS, HTTPS, a mutable branch, GitHub, GitLab, or a URL.
- No requirement for users to manage signing keys manually.
- No forced update when release discovery is temporarily unavailable.
- No claim that current source installers or current binaries already provide this production update guarantee.

## Acceptance criteria

This decision is complete only when a clean macOS, Linux, and Windows installation can obtain an approved stable release through the bootstrap path, verify its signed manifest and artifact digest, install atomically, and later upgrade through a provider fallback without trusting unsigned or stale data. The same test suite must demonstrate that a provider outage, domain migration, lost mirror copy, invalid signature, key rotation, and interrupted replacement fail safely or recover as documented.
