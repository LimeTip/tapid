# Tapid release signing key management

Status: Active for release-record sidecars
Created: 2026-08-27
Last reviewed: 2026-10-05

Tapid release-record sidecars use the embedded Ed25519 trust root and a protected signing seed supplied to the publication workflow. GitHub draft releases, SHA-256 checksums, exact asset verification, public installer smoke tests, and crates.io Trusted Publishing remain part of the release process. [ADR 0007](../adr/0007-one-candidate-one-release-approval.md) connects them behind one independent candidate approval.

The existing Ed25519 private PEM is stored in the `stable-release` environment as `TAPID_RELEASE_ED25519_PRIVATE_KEY`. The workflows map it to the signer's `TAPID_RELEASE_SIGNING_KEY` environment variable only for the signing or key-check step. The signer also accepts a canonical base64-encoded 32-byte seed. Private material stays in memory and is never checked into source control, written to the workspace, or printed in logs.

The environment variable `TAPID_RELEASE_SIGNING_KEY_ID` identifies the trusted key in `crates/tapid-signatures/data/release-keyring.json`. Before signing, the tool derives the private key's public key and requires an exact match with that keyring entry. An invalid format, unknown key ID, wrong algorithm, or mismatched key stops the operation before any sidecar is written.

Before the first release-record publication, dispatch `release-signing-check.yml` from `main` and obtain the independent `stable-release` environment approval. This read-only workflow validates the stored key against the embedded public identity without signing metadata, uploading artifacts, or creating a release. Only a successful protected run establishes that the currently stored secret is usable; historical setup documentation does not prove its current value.

The same environment protects candidate signing in the automatic release coordinator, which runs from `main`. Keep its deployment policy main-only, its independent reviewer requirement, and self-review prevention. Before approval, the reviewer sees the exact source commit, notes, archive and installer digests, package compatibility evidence, and crate plan. Approval authorizes signing and subsequent GitHub and crates.io publication of that candidate. Changes to those approved bytes or versions invalidate the authorization.

The secret is exposed only to the signing step after the candidate is checked again. Unprivileged build and package-preflight jobs do not receive the signing key or publication credentials. Native draft verification and public installer smoke still gate promotion and registry publication respectively. The separate `crates-io-release` environment retains its main-only OIDC identity; its extra reviewer requirement is removed only after the reviewed candidate gate is active. See the [activation checklist](../release-automation.md#one-time-setup).

The standalone key-check workflow retains diagnostic uses. Binary candidate building and signing run through the coordinator from `main`; a recovery dispatch must enter the same independent candidate approval again. Do not broaden the environment to tags or bypass its reviewer.

Key rotation, revocation, rollback protection, recovery, and operational ownership must remain documented before changing the trust root. Historical public key files may remain for compatibility with old artifacts but are not active release authorization.

Release-record sidecars use the non-expiring immutable schema from [ADR 0006](../adr/0006-immutable-release-signatures.md). They authenticate exact record bytes, not the freshness of latest-release discovery. The existing local rollback floor remains required. A key recovery or rotation must account for permanently verifiable historical signatures; removing a trusted key can make those releases unverifiable. Legacy signed channel manifests retain their expiration checks.
