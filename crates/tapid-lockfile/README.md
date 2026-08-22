# tapid-lockfile

Deterministic lockfile models and canonical JSON serialization for Tapid.

The current contract provides:

- Schema version `3`, root manifest digest, and resolver/linker compatibility versions.
- Exact package name and version keys, registry origin, SHA-512 SRI, SHA-256 unpacked digest, and explicit `treeDigest` replay identity.
- Deterministic package ordering through `BTreeMap` serialization.
- HTTPS registry and artifact URL validation.
- Rejection of local file URLs, query fragments, userinfo, and unsupported versions.
- Round-trip parsing and replay validation through `Lockfile::from_json` and `validate_replay`.

Consumer replay uses `STORE/trees/<digest>/` and a regular `.tapid-tree` marker containing the exact digest. The CLI validates every referenced tree and the root manifest digest before staging and atomically replacing `node_modules`; dependency lifecycle scripts never run. `tapid install --store-dir PATH` supplies a dynamic store root.

This is a lockfile model and replay contract, not a complete npm lockfile implementation or dependency resolver. Rich peer, optional, platform, lifecycle, provenance, audit, and complete dependency-edge semantics remain limited to the tested subset.
