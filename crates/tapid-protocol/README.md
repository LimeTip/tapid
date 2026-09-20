# tapid-protocol

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-protocol)](https://crates.io/crates/tapid-protocol)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-protocol)](https://crates.io/crates/tapid-protocol)
[![Docs.rs](https://docs.rs/tapid-protocol/badge.svg)](https://docs.rs/tapid-protocol)
[![License](https://img.shields.io/crates/l/tapid-protocol)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Wire-facing package identity contracts for the Tapid package ecosystem.

`PackageInstanceWire` serializes registry, package name, and package version as camel-case transport strings while keeping those untrusted strings separate from `tapid-core` domain types. Conversion back to `PackageInstanceId` validates the registry origin, package name, and semantic version at the boundary. The crate currently defines package identity transport only; it is not a complete client-registry protocol.
