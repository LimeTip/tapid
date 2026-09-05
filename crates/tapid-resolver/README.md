# tapid-resolver

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-resolver)](https://crates.io/crates/tapid-resolver)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-resolver)](https://crates.io/crates/tapid-resolver)
[![Docs.rs](https://docs.rs/tapid-resolver/badge.svg)](https://docs.rs/tapid-resolver)
[![License](https://img.shields.io/crates/l/tapid-resolver)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Pure deterministic resolution for normalized npm- and JSR-compatible registry metadata.

Registry adapters own network access and normalize metadata. The resolver accepts only those values and performs no HTTP, archive, or filesystem work. Package identities retain registry origin, so equal names and versions from npm and JSR remain distinct.

The supported requirement language is intentionally bounded: exact stable or prerelease versions, the `*` wildcard, caret ranges such as `^1.2.3` and `^2.0.0-rc.1`, major-only caret shorthand such as `^3`, tilde ranges, whitespace-separated intersections, and non-empty `||` alternatives. Stable ranges do not select prerelease candidates. Unsupported comparators, empty alternatives, tags, aliases, git, file, workspace, and other npm syntax return structured `UnsupportedRange` errors.

`resolve_graph` walks transitive dependency maps deterministically, handles cycles through registry-qualified exact identities, and can select different versions of one transitive package for different parent edges. It reports sorted requirements and candidates for conflicts. Offline resolution succeeds only with supplied cached metadata. Frozen resolution requires a lockfile replay input from the consumer layer and never falls back to the network.
