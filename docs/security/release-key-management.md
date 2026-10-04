# Tapid release signing key management

Status: Active for release-record sidecars
Created: 2026-08-27
Last reviewed: 2026-09-27

Tapid release-record sidecars use the embedded Ed25519 trust root and a protected signing seed supplied to the publication workflow. GitHub draft releases, SHA-256 checksums, manual publication, public installer smoke tests, and crates.io Trusted Publishing remain part of the release process.

The existing Ed25519 private PEM is stored in the `stable-release` environment as `TAPID_RELEASE_ED25519_PRIVATE_KEY`. The workflows map it to the signer's `TAPID_RELEASE_SIGNING_KEY` environment variable only for the signing or key-check step. The signer also accepts a canonical base64-encoded 32-byte seed. Private material stays in memory and is never checked into source control, written to the workspace, or printed in logs.

The environment variable `TAPID_RELEASE_SIGNING_KEY_ID` identifies the trusted key in `crates/tapid-signatures/data/release-keyring.json`. Before signing, the tool derives the private key's public key and requires an exact match with that keyring entry. An invalid format, unknown key ID, wrong algorithm, or mismatched key stops the operation before any sidecar is written.

Before the first release-record publication, dispatch `release-signing-check.yml` from `main` and obtain the independent `stable-release` environment approval. This read-only workflow validates the stored key against the embedded public identity without signing metadata, uploading artifacts, or creating a release. Only a successful protected run establishes that the currently stored secret is usable; historical setup documentation does not prove its current value.

The same environment protects draft release signing. Its deployment policy must permit the chosen workflow ref. Automatic tag-triggered signing requires a permitted release-tag policy; a `main`-only environment instead requires dispatching the build workflow from `main` after the annotated tag exists. Preserve the environment's independent reviewer and self-review prevention settings.

Key rotation, revocation, rollback protection, recovery, and operational ownership must remain documented before changing the trust root. Historical public key files may remain for compatibility with old artifacts but are not active release authorization.
