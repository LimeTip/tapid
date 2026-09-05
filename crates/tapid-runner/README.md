# tapid-runner

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-runner)](https://crates.io/crates/tapid-runner)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-runner)](https://crates.io/crates/tapid-runner)
[![Docs.rs](https://docs.rs/tapid-runner/badge.svg)](https://docs.rs/tapid-runner)
[![License](https://img.shields.io/crates/l/tapid-runner)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Planning and validation contract for policy-aware package scripts. A `RunnerRequest` carries the exact artifact digest and script; approvals bind to the exact digest and SHA-256 hash of the normalized script (CRLF/CR normalized to LF and outer whitespace trimmed).

The crate does not execute processes or provide sandboxing/containment. It reports explicit unsupported-OS limitations and validates approvals before any future execution layer is called.
