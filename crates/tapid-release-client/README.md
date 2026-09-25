# tapid-release-client

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-release-client)](https://crates.io/crates/tapid-release-client)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-release-client)](https://crates.io/crates/tapid-release-client)
[![Docs.rs](https://docs.rs/tapid-release-client/badge.svg)](https://docs.rs/tapid-release-client)
[![License](https://img.shields.io/crates/l/tapid-release-client)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Provider-neutral verified release discovery and artifact validation for Tapid.

`tapid-release-client` validates bounded stable-channel indexes and signed release manifests, selects exactly one artifact for the requested target, and verifies its declared size and SHA-256 digest. Network access is supplied by the caller through the `Fetcher` trait; the crate requires HTTPS metadata URLs but does not provide an HTTP transport.

The durable release-state helpers use validated, atomically replaced JSON state. They reject replayed release sequences and versions below the recorded release floor. Version 1 release manifests intentionally contain no sequence field; monotonic sequence policy is maintained separately in the version 2 client state.

Version 0.0.3 adds release-state verification provenance: `signature`, `checksum`, or `unknown`. Older state without this field reads as `unknown`; accepting a subsequent release preserves the recorded provenance until the caller updates it. This field records the caller's verification result and does not itself verify a signature or checksum.

The next version, 0.0.4, accepts stable versions with any major number in durable release state and rollback comparisons. Every component must fit an unsigned 64-bit integer and must not have leading zeroes. The retained signed-manifest v1 parser keeps its existing strict contract, including its 0.x version restriction. This state change does not redefine that signed format. The CLI's new HTTPS release-record parser is separate from this crate's legacy signed discovery.

Discovery tries another endpoint or manifest URL only after a fetch outage. Invalid received indexes or signed manifests, including stale metadata, missing targets, invalid signatures, and oversized responses, return their validation error immediately. Callers must not treat those errors as permission to use a weaker verification path or cached recovery.

`Fetcher::fetch_metadata_with_limit` preserves this distinction for streaming transports. Version 0.0.3 requires every implementation to provide this typed method; there is no default conversion from string errors. Return `Error::Fetch` only for unavailability and a validation error for rejected responses, enforcing the byte limit during reads. Existing implementers must add this method when upgrading from 0.0.2. The CLI's curl transport already classifies these outcomes explicitly.

This crate does not install or execute downloaded artifacts, choose trusted signing keys, provide independent release transparency, or turn same-provider checksums into an independent authenticity proof.
