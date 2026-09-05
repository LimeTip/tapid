# tapid-test-support

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-test-support)](https://crates.io/crates/tapid-test-support)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-test-support)](https://crates.io/crates/tapid-test-support)
[![Docs.rs](https://docs.rs/tapid-test-support/badge.svg)](https://docs.rs/tapid-test-support)
[![License](https://img.shields.io/crates/l/tapid-test-support)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Reusable, dependency-free test infrastructure for Tapid crates.

The crate provides automatically cleaned temporary project and home directories, path-escape-safe fixture writes, deterministic filesystem-safe fixture names, an in-memory registry seam, and adversarial boundary inputs. It deliberately performs no network access and has no production dependencies.
