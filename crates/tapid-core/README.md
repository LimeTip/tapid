# tapid-core

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-core)](https://crates.io/crates/tapid-core)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-core)](https://crates.io/crates/tapid-core)
[![Docs.rs](https://docs.rs/tapid-core/badge.svg)](https://docs.rs/tapid-core)
[![License](https://img.shields.io/crates/l/tapid-core)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Core domain types and deterministic validation for Tapid.

This crate contains package names, canonical SemVer identities including prereleases, SHA-256 artifact digests, canonical padded SHA-512 package integrity values, peer/platform contexts, and shared domain errors. It is intentionally independent of the CLI, network, and filesystem.

The API is experimental and may change before the first stable release.
