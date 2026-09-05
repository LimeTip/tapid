# tapid-signatures

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-signatures)](https://crates.io/crates/tapid-signatures)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-signatures)](https://crates.io/crates/tapid-signatures)
[![Docs.rs](https://docs.rs/tapid-signatures/badge.svg)](https://docs.rs/tapid-signatures)
[![License](https://img.shields.io/crates/l/tapid-signatures)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Canonical artifact-bound trust foundations for Tapid, a JavaScript and TypeScript package manager written in Rust.

`TrustEnvelope` is the generic attestation and transparency payload. Its signed context authenticates the Ed25519 algorithm and key ID, while `KeyRing` resolves key IDs only from caller-owned trusted keys. `TrustEnvelope::verify()` is a structural compatibility check and never proves cryptographic authenticity. Use `verify_with_keyring()` for trusted cryptographic verification. The raw `verify_with_public_key()` method is a low-level compatibility API and does not establish trust in the key ID.

Release distribution is intentionally a separate contract. The `release` module signs and verifies the schema-defined `tapid-release-manifest-v1` payload by computing `signed_digest` over the manifest with the top-level `signature` removed, then signing a canonical contextual payload that additionally authenticates `algorithm`, `key_id`, and `signed_digest`. This keeps the manifest wire shape while preventing trusted-alias relabeling. This crate does not yet implement installer policy, freshness, rollback protection, key rotation delivery, artifact extraction, or atomic client replacement.

Key IDs must be non-empty, at most 128 bytes, and contain only ASCII letters, digits, period, underscore, colon, or hyphen. Unknown keys and unsupported algorithms fail closed with distinct errors.
