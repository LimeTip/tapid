# tapid-lockfile

Deterministic lockfile models and canonical JSON serialization for Tapid.

The current implementation provides:

- Schema version `1` with root manifest digest and resolver/linker compatibility versions.
- Exact package name and version keys, registry origin, SHA-512 Subresource Integrity, and SHA-256 unpacked tree digest.
- Deterministic package ordering through `BTreeMap` serialization.
- HTTPS registry and artifact URL validation.
- Rejection of local file URLs, query fragments, userinfo, and unsupported lockfile versions.
- Round-trip parsing and validation through `Lockfile::from_json`.

This is a lockfile model, not a dependency resolver or installer. Dependency edges and richer platform, peer, lifecycle, provenance, and audit fields will be added only with corresponding tested behavior.
