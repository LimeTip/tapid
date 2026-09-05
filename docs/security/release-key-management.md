# Tapid release signing key management

Status: Retired for current client releases
Created: 2026-08-27
Retired: 2026-09-03

Tapid previously used this key for a custom signed release-manifest design. ADR 0004 replaced that design with GitHub-native draft releases, SHA-256 checksums, manual publication, public installer smoke tests, and crates.io Trusted Publishing.

The current release workflow does not use a release signing key or embedded release keyring. Any retained private key material must remain protected and must not be used to imply that current client releases are independently signed.

A new release-signing system requires a separate accepted design covering the trust root, rotation, revocation, rollback protection, recovery, and operational ownership. Historical public key files may remain for compatibility with old artifacts but are not active release authorization.
