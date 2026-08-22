# tapid-lockfile

Deterministic lockfile models and canonical JSON serialization for Tapid.

The current implementation provides:

- Schema version `3` with root manifest digest and resolver/linker compatibility versions.
- Exact package name and version keys, registry origin, SHA-512 Subresource Integrity, SHA-256 unpacked digest, and explicit `treeDigest` replay identity.
- Deterministic package ordering through `BTreeMap` serialization.
- HTTPS registry and artifact URL validation.
- Rejection of local file URLs, query fragments, userinfo, and unsupported lockfile versions.
- Round-trip parsing and validation through `Lockfile::from_json`.

Install replay uses `treeDigest` with the store contract `STORE/trees/<digest>/` and a regular `.tapid-tree` marker containing the exact digest. The CLI validates every tree before staging output, copies only regular files/directories, and atomically replaces `node_modules`; lifecycle scripts are never run. `tapid install --store-dir PATH` supplies a dynamic store root.

This is a lockfile model, not a dependency resolver or installer. Dependency edges and richer platform, peer, lifecycle, provenance, and audit fields will be added only with corresponding tested behavior.
