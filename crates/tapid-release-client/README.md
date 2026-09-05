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

This crate does not install or execute downloaded artifacts, choose trusted signing keys, provide independent release transparency, or turn same-provider checksums into an independent authenticity proof.
