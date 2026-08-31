# tapid-lockfile


[Crates.io](https://crates.io/crates/tapid-lockfile) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-lockfile)

Deterministic lockfile models and canonical JSON serialization for Tapid.

The current contract provides:

- Schema version `6`, root manifest digest, resolver/linker compatibility versions, and exact canonical direct-root package keys.
- Exact package name and version keys, registry origin, canonical padded SHA-512 SRI, required schema 6 registry-integrity provenance, SHA-256 unpacked digest, and explicit `treeDigest` replay identity.
- Deterministic package ordering through `BTreeMap` serialization.
- HTTPS registry and artifact URL validation.
- Rejection of local file URLs, query fragments, userinfo, and unsupported versions.
- Rejection of missing registry-integrity provenance and missing, dangling, duplicate, unordered, or noncanonical schema 6 roots.
- Round-trip parsing and replay validation through `Lockfile::from_json` and `validate_replay`.

Schema `4` remains readable for controlled compatibility when package keys use canonical or empty contexts and SHA-512 integrity is padded canonically. When such a schema 4 lockfile has no explicit roots, CLI replay reconstructs one root per direct manifest identity by applying all requirements from dependencies, development dependencies, and optional dependencies, then selecting the highest matching locked version. Replay rejects missing candidates, ambiguous package contexts, and legacy values that cannot be normalized safely. Schema versions earlier than 4 are not accepted implicitly.

Package keys encode empty contexts as `peer=-|platform=-`. Non-empty peer contexts use canonical `name=...;version=...` fields. Platform contexts use fixed canonical fields such as `os=linux;cpu=x86_64;libc=gnu`. Reserved context characters are percent-encoded, and noncanonical wire representations are rejected.

Consumer replay uses `STORE/trees/<digest>/` and a regular `.tapid-tree` marker containing the exact digest. Before reading store trees, the CLI rejects schema 5 locks that lack the schema 6 provenance contract and packages marked as lacking registry-declared integrity, validates every explicit schema 6 root against direct manifest identity and version requirements, and requires exactly one root per direct identity. It then validates every referenced tree and stages the managed layout before atomically replacing `node_modules`; dependency lifecycle scripts never run. `tapid install --store-dir PATH` supplies a dynamic store root.

This is a lockfile model and replay contract, not a complete npm lockfile implementation or dependency resolver. Rich peer, optional, platform, lifecycle, provenance, audit, and complete dependency-edge semantics remain limited to the tested subset.
