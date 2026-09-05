# tapid-publish

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-publish)](https://crates.io/crates/tapid-publish)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-publish)](https://crates.io/crates/tapid-publish)
[![Docs.rs](https://docs.rs/tapid-publish/badge.svg)](https://docs.rs/tapid-publish)
[![License](https://img.shields.io/crates/l/tapid-publish)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Deterministic package packing and staged publication foundations for Tapid.

`PackageSource` describes a package root, immutable version label, and explicit
`ExclusionRules`. `NormalizedFileManifest::from_source` validates and normalizes
relative paths, excludes `.git`, `target`, and configured paths, hashes file
contents, and sorts entries lexicographically. `pack` emits a versioned,
byte-stable pack format and binds its exact bytes to a SHA-256 `ArtifactDigest`.

Publication is deliberately transport-neutral. `Publisher::preview` performs
all filesystem work without side effects; only `promote` invokes the injected
`PublicationTransport`. A promoted version cannot be promoted again, preventing
replacement of immutable version identities. No registry, credentials, or
network transport is included in this crate.
