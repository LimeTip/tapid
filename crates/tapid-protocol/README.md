# tapid-protocol

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-protocol)](https://crates.io/crates/tapid-protocol)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-protocol)](https://crates.io/crates/tapid-protocol)
[![Docs.rs](https://docs.rs/tapid-protocol/badge.svg)](https://docs.rs/tapid-protocol)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Wire-facing package identity contracts for the Tapid package ecosystem.

`PackageInstanceWire` serializes source, package name, and package version as camel-case transport strings while keeping those untrusted strings separate from `tapid-core` domain types. The historical `registry` field carries either a registry origin or an immutable copied file/Git source. Conversion back to `PackageInstanceId` validates the source, package name, and semantic version at the boundary. This is not a complete client-registry protocol.

Versioned package security requests, results, hosted authorization claims and stable error codes are also defined here. Security DTO validation preserves exact origin, alias and graph context; advisory scope remains version-based. See [the public v1 contract](../../docs/security-evidence.md). Parsing a DTO authenticates neither an account nor an evidence issuer.

The separately negotiated `SecurityResultV2` adds explicit report classification,
revision, active/withdrawn state and available exact-version fixes. V1 remains
closed. Use `SecurityRequest::validate_v2` for v2 requests; registry report coverage
distinguishes `advisory` and `reported_malware`. See the [v2 contract](../../docs/security-evidence.md#security-report-contract-v2).
