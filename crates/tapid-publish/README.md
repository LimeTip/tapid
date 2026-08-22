# tapid-publish


[Crates.io](https://crates.io/crates/tapid-publish) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-publish)

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
