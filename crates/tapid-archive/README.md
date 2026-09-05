# tapid-archive

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-archive)](https://crates.io/crates/tapid-archive)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-archive)](https://crates.io/crates/tapid-archive)
[![Docs.rs](https://docs.rs/tapid-archive/badge.svg)](https://docs.rs/tapid-archive)
[![License](https://img.shields.io/crates/l/tapid-archive)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

`tapid-archive` validates archive entry metadata before extraction. Names using either slash style are checked. Traversal, absolute, drive, and UNC paths are rejected. Entry count, path length, compressed size, extracted size, and path limits are explicit. Duplicate and case-colliding names, links, device nodes, and other special files are rejected. Tar and tar.gz archives are extracted into a fresh staging directory.

Unix extraction normalizes regular files to `0644` or `0755`, preserves only the executable distinction, and strips ownership and privilege bits. A canonical internal mode manifest makes executable-aware tree digests identical across Unix and Windows while Unix verification also checks the actual mode. The crate does not scan executable content or provide malware detection; store activation remains a separate layer.
