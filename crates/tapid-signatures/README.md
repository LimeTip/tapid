# tapid-signatures

Canonical artifact-bound trust envelope foundations. `canonical_json` sorts all object keys recursively and `TrustEnvelope::signing_bytes` binds version, subject, artifact digest, and claims. `verify` is intentionally unsigned/unsupported until a real cryptographic implementation can be fully tested; it never reports a fake signature as valid.
