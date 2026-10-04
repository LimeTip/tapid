# tapid-resolver

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-resolver)](https://crates.io/crates/tapid-resolver)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-resolver)](https://crates.io/crates/tapid-resolver)
[![Docs.rs](https://docs.rs/tapid-resolver/badge.svg)](https://docs.rs/tapid-resolver)
[![License](https://img.shields.io/crates/l/tapid-resolver)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Pure deterministic resolution for normalized npm- and JSR-compatible registry metadata.

Registry adapters own network access and normalize metadata. The resolver accepts only those values and performs no HTTP, archive, or filesystem work. Package identities retain registry origin, so equal names and versions from npm and JSR remain distinct.

The supported npm range contract is differentially tested against pinned `node-semver` 7.8.5: exact and partial versions, `x`/`*` wildcards, comparator sets and intersections, caret/tilde including zero-major boundaries, hyphen ranges, and non-empty `||` alternatives. Empty range strings have npm wildcard semantics at the range parser; registry metadata separately rejects blank dependency declarations as malformed. Prerelease eligibility follows npm's same-base comparator rule, and build metadata is ignored for satisfaction. Candidate selection remains a separate deterministic policy. Tags, git, file, workspace, and other non-range package specs return structured `UnsupportedRange` errors.

`resolve_graph` walks transitive dependency maps deterministically, handles cycles through registry-qualified exact identities, and can select different versions of one transitive package for different parent edges. `resolve_graph_with_routing` accepts a caller-owned route selector for each transitive dependency and peer provider; root identities remain caller-selected, and routing failures are returned as `RegistryRouting` errors. It reports sorted requirements and candidates for conflicts. Offline resolution succeeds only with supplied cached metadata. Frozen resolution requires a lockfile replay input from the consumer layer and never falls back to the network.

Npm alias requirements retain the actual target name alongside the parsed version range. Resolution selects the actual registry identity while retaining local root bindings and parent edge names. Routing callbacks receive the actual target name. Distinct local aliases can select different versions of one actual package.
