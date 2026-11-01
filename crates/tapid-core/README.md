# tapid-core

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-core)](https://crates.io/crates/tapid-core)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-core)](https://crates.io/crates/tapid-core)
[![Docs.rs](https://docs.rs/tapid-core/badge.svg)](https://docs.rs/tapid-core)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Core domain types and deterministic validation for Tapid.

This crate contains package names, canonical SemVer identities including prereleases, SHA-256 artifact digests, canonical padded SHA-512 package integrity values, peer/platform contexts, and shared domain errors. It is intentionally independent of the CLI, network, and filesystem.

`ArtifactUrl` validates canonical HTTPS artifact references without credentials,
queries, fragments, or ambiguous percent encoding. Transport and lockfile readers
share this rule without normalizing persisted references.

`PackagePlatform` owns npm OS/CPU aliases, positive and negated constraints, and
Linux libc matching. `matches` selects applicable packages; `selected_context`
records only the constrained dimensions in package-instance identity.

`RegistryOrigin` canonicalizes HTTPS origins (host casing/IDNA, IP spelling,
default port removal and slash-only roots), and rejects credentials, query,
fragment and non-root paths. Non-default ports remain distinct. Persisted identity
consumers must not silently apply this normalization to existing graph keys;
`tapid-lockfile` rejects noncanonical persisted origins with explicit recovery.

The API is experimental and may change before the first stable release.

`PackageSource` retains registry origins or immutable copied file/Git identity.
File identities contain a contained root-relative path and archive SHA-256;
Git identities contain a credential-free canonical HTTPS repository, full commit,
and archive SHA-256. `PackageInstanceId::from_source` preserves these identities
through resolution and layout; `new` retains its registry-origin interface.
`GitRepository` validates repository URLs without admitting them as registry origins.

The domain model also recognizes `directory-v1:<project-relative-path>#<sha256>`
for accepted canonical directory snapshots. Directory origins remain distinct
from file, registry, and other directory origins even when artifact bytes match.
This identity support alone does not enable directory declarations in the CLI.
