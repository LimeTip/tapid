# tapid-manifest

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-manifest)](https://crates.io/crates/tapid-manifest)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-manifest)](https://crates.io/crates/tapid-manifest)
[![Docs.rs](https://docs.rs/tapid-manifest/badge.svg)](https://docs.rs/tapid-manifest)
[![License](https://img.shields.io/crates/l/tapid-manifest)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Pure parsing, validation, and deterministic serialization for the selected npm-compatible `package.json` fields.

The crate validates required `name` and `version`, optional `private`, `description`, and `license`, string-valued dependency maps, string-valued `scripts`, and package executable metadata. `bin` accepts either a string target or an object of command names to relative targets. Commands and targets are validated and exposed in deterministic order. Absolute, traversal, malformed, and unsupported bin values are rejected.

`PackageManifest::parse` accepts document text and `to_json` emits stable JSON with sorted map keys. Filesystem reads, archive extraction, link creation, and process execution are intentionally owned by other crates and the CLI.
