# Tapid release signing key management

Status: Active for release-record sidecars
Created: 2026-08-27
Last reviewed: 2026-09-27

Tapid release-record sidecars use the embedded Ed25519 trust root and a protected signing seed supplied to the publication workflow. GitHub draft releases, SHA-256 checksums, manual publication, public installer smoke tests, and crates.io Trusted Publishing remain part of the release process.

The private signing seed is supplied only through the protected `TAPID_RELEASE_SIGNING_KEY` workflow secret and is never checked into source control or written to the workspace. The key ID must match the embedded public key in `crates/tapid-signatures/data/release-keyring.json`.

Key rotation, revocation, rollback protection, recovery, and operational ownership must remain documented before changing the trust root. Historical public key files may remain for compatibility with old artifacts but are not active release authorization.
