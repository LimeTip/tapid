# tapid-policy

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-policy)](https://crates.io/crates/tapid-policy)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-policy)](https://crates.io/crates/tapid-policy)
[![Docs.rs](https://docs.rs/tapid-policy/badge.svg)](https://docs.rs/tapid-policy)
[![License](https://img.shields.io/crates/l/tapid-policy)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Explainable policy planning primitives. Evidence is typed as `declared`, `inferred`, `observed`, `enforced`, or explicitly ambiguous; policy decisions are `allow`, `warn`, `prompt`, or `deny` with stable reason codes and deterministic JSON serialization.

This crate makes no execution or containment claim. Unattended operation fails closed when evidence would require an interactive decision.
