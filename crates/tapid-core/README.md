# tapid-core


[Crates.io](https://crates.io/crates/tapid-core) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-core)

Core domain types and deterministic validation for Tapid.

This crate contains package names, canonical SemVer identities including prereleases, SHA-256 artifact digests, canonical padded SHA-512 package integrity values, peer/platform contexts, and shared domain errors. It is intentionally independent of the CLI, network, and filesystem.

The API is experimental and may change before the first stable release.
