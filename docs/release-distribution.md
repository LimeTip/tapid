# Signed client release distribution

Status: design contract for the next release-distribution implementation slice.

## Scope

Tapid's developer installers build from an explicitly selected source ref. They are not a release trust channel. Release installation must instead select an immutable version and verify signed metadata before downloading or replacing a client binary.

The release manifest is `schemas/tapid-release-manifest-v1.json`. It binds a Tapid version and Git commit to platform artifacts, optional SBOM and provenance documents, and an Ed25519 signature over canonical manifest bytes.

## Verification order

A client or installer must:

1. Accept only an HTTPS manifest URL from the canonical Tapid release origin.
2. Parse the manifest strictly and reject unknown fields, malformed versions, unsupported targets, non-HTTPS URLs, and invalid digests.
3. Verify the signature using a trusted key identified by `key_id`.
4. Verify manifest freshness and reject expired metadata.
5. Reject a version lower than the locally recorded release floor unless an explicit downgrade mode is supported and authorized.
6. Resolve the requested target from the manifest, not from an independently constructed mutable URL.
7. Download the artifact with bounded size and timeout controls.
8. Verify the artifact SHA-256 digest and expected size.
9. Validate archive members before extraction. The archive must contain only the expected executable and no links, traversal paths, or special files.
10. Stage the executable in the destination directory and atomically replace only a regular Tapid-managed destination.
11. Preserve the previous binary until activation succeeds and retain enough metadata for interrupted-operation recovery.

Signature verification must fail closed. HTTPS and a checksum file alone are not sufficient release authentication.

## Key lifecycle

The trusted key set must be versioned and shipped through a controlled client update or repository release process. Each key has a stable identifier and validity interval. Rotation adds a new key before retiring the old key. A compromised key requires a signed trust update, release quarantine, and a documented recovery path. The client must not silently accept an unknown key or infer trust from the key ID.

## Required implementation tests

Before advertising release installation:

- valid signed manifest and artifact installation;
- invalid signature;
- unknown key;
- expired manifest;
- rollback or downgrade;
- checksum mismatch;
- artifact size mismatch;
- malformed archive;
- archive traversal, links, duplicates, and special files;
- interrupted staging and replacement recovery;
- existing user-managed destination refusal;
- clean installations on macOS, Ubuntu, and Windows.

The release workflow must additionally produce checksums, SBOMs, and signed provenance, then install a prerelease through every advertised channel.

## Explicit non-goals

This design does not enable `tapid upgrade`, automatic update checks, or production release installation by itself. Those behaviors remain disabled until the verification implementation and end-to-end release evidence exist.
