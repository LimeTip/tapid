# Tapid release signing key management

Status: active
Created: 2026-08-27

## Purpose

Tapid stable releases use an Ed25519 signing key to authenticate release manifests. The manifest signature proves that the release metadata was approved by the holder of the release signing key. Artifact hashes and sizes are verified separately.

The private signing key must never be committed to the repository, included in a build, placed in an issue, or printed in logs. Only public key material belongs in the Tapid trust configuration.

## Active public identity

- Key ID: `release-key-2026-01`
- Algorithm: `Ed25519`
- Fingerprint: `sha256-238d16177b1c9ae21b53476d1a9097b5011414a26e6625986ecf1799dacf47f4`
- Keyring format: `tapid-release-keyring-v1`
- Status: Active
- Intended use: Tapid stable release manifest signing only

## Public files

The following public files were generated and verified together:

- `release-key-public.pem`
- `release-key-public.raw`
- `release-keyring.json`

The keyring contains the public Ed25519 key, its key ID, and its SHA-256 fingerprint. The raw public key is exactly 32 bytes.

## Verification performed

The following checks were completed on 2026-08-27:

- All expected key files were present before cleanup.
- `release-keyring.json` was valid JSON.
- The private PEM was a valid Ed25519 private key.
- The public PEM was a valid Ed25519 public key.
- The public key derived from the private key matched the public PEM.
- The public PEM matched `release-key-public.raw`.
- The public raw key matched the Base64 key in `release-keyring.json`.
- The keyring fingerprint matched the raw public key.
- The local private-key file was removed after backup verification.
- After cleanup, the private-key file was verified absent locally.

## Operational rules

1. Keep the private signing key only in approved protected secret storage and a separate encrypted recovery backup.
2. Never copy private key material into Git, documentation, chat, tickets, CI logs, shell history, or build artifacts.
3. Use the key only for signing Tapid stable release manifests.
4. Keep stable publication behind the protected release approval process.
5. Do not change the public keyring without a documented key rotation or recovery event.
6. When rotating keys, publish the new public key and key ID through the established trust-root process before signing releases with it.
7. Retain the old public key long enough to verify and recover previously published release metadata, according to the rotation policy.

## Recovery and rotation

A lost or unavailable signing key must not be replaced by silently adding an unreviewed key. Recovery requires:

1. Confirming the incident scope and preserving existing release metadata.
2. Authorizing a replacement signing key through the approved recovery process.
3. Publishing the replacement public key and fingerprint through the trusted keyring update path.
4. Testing signature verification with a non-production fixture.
5. Running a controlled release and verifying installation and upgrade on supported platforms.
6. Recording the old key status as retired or compromised, as appropriate.

If the active signing key is suspected to be compromised, stop stable publication, preserve existing artifacts, and follow the security incident and key-rotation runbook.

## Implementation note

The production client must trust the reviewed public keyring, while the release CI must access the private signing key only through protected secret injection. The public keyring and the release publication workflow must remain consistent with this document.
