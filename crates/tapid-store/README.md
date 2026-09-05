# tapid-store

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-store)](https://crates.io/crates/tapid-store)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-store)](https://crates.io/crates/tapid-store)
[![Docs.rs](https://docs.rs/tapid-store/badge.svg)](https://docs.rs/tapid-store)
[![License](https://img.shields.io/crates/l/tapid-store)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

`tapid-store` is a filesystem-authoritative content-addressed store. `Store::ingest` accepts any `std::io::Read`, streams bytes through SHA-256 into a private staging file, calls `sync_all`, verifies the requested `ArtifactDigest`, and atomically activates the staged file under the dynamic store root. Digest paths are never overwritten. An existing regular file is authoritative and ingestion is idempotent. Failed reads and digest mismatches are cleaned up without activating partial bytes.

The CLI also replays verified package trees at `STORE/trees/<sha256-...>/`. Each tree requires a regular `.tapid-tree` marker containing the exact digest before offline or frozen installation can use it. Replay uses atomically reserved snapshots, advisory ownership leases, stale-state recovery, copy-on-write cloning where supported, and verified byte-copy fallback. The store does not fetch registry metadata, run lifecycle scripts, garbage-collect, or provide a remote cache.

`tapid-archive` is a direct dependency used to validate extraction limits and canonical tree digests before activation.
