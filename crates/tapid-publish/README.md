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

## Consistency and portable-path policy

`pack` reads each included file once. The encoded content and its manifest size
and digest come from that same buffer. Use the manifest returned by `pack` when
you need this binding; a separate `NormalizedFileManifest::from_source` call is
an independent read. Neither operation provides an atomic directory snapshot.
Keep the source tree stable and trusted during traversal and reads: concurrent
file changes, replacement by symlinks, and directory changes are not prevented.

After case-sensitive exclusions and separator normalization, complete file
paths with equal Rust `to_uppercase().to_lowercase()` keys are rejected. This
conservative casing policy includes ASCII case pairs and ordinary/final Greek
sigma (`σ`/`ς`). It deliberately can reject names that a particular filesystem
keeps distinct, such as `ß` and `ss`. Casing follows the Rust toolchain's Unicode
tables; it is not full Unicode case folding. Original path spelling is retained
in accepted artifacts.

This is **not** a universal portable-filesystem guarantee: Unicode normalization
(e.g. composed/decomposed accents), filesystem-specific casing tables, reserved
names, trailing dots/spaces, short-name aliases, and file/directory component
collisions with different complete paths are not covered. Collisions already
collapsed by the source filesystem cannot be recovered. Consumers still need
target-specific extraction validation. Non-UTF-8 source names are converted
lossily by the current traversal; this policy does not make them portable.

Native publishing remains future product scope; this crate supplies local pack
and preview/promote foundations, not a deployed registry or complete protocol.

Publication is deliberately transport-neutral. `Publisher::preview` performs
filesystem reads without writing source files; only `promote` invokes the injected
`PublicationTransport`. Repeated promotion is rejected only within the same
`Publisher` instance; durable immutability must be enforced by the transport and
registry. Public artifact/preview fields are mutable and are not revalidated by
`promote`. No registry, credentials, or
network transport is included in this crate.
