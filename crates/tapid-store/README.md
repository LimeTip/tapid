# tapid-store

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-store)](https://crates.io/crates/tapid-store)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-store)](https://crates.io/crates/tapid-store)
[![Docs.rs](https://docs.rs/tapid-store/badge.svg)](https://docs.rs/tapid-store)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

`tapid-store` is a filesystem-authoritative content-addressed store. `Store::ingest` accepts any `std::io::Read`, streams bytes through SHA-256 into a private staging file, calls `sync_all`, verifies the requested `ArtifactDigest`, and atomically activates the staged file under the dynamic store root. Digest paths are never overwritten. An existing regular file is authoritative and ingestion is idempotent. Failed reads and digest mismatches are cleaned up without activating partial bytes.

The CLI also replays verified package trees at `STORE/trees/<sha256-...>/`. Each tree requires a regular `.tapid-tree` marker containing the exact digest before offline or frozen installation can use it. Replay uses atomically reserved snapshots, advisory ownership leases, stale-state recovery, copy-on-write cloning where supported, and verified byte-copy fallback. The store does not fetch registry metadata, run lifecycle scripts, automatically garbage-collect, or provide a remote cache.

`tapid-archive` is a direct dependency used to validate extraction limits and canonical tree digests before activation. `StoreTransaction` stages verified package trees privately, publishes them under a cross-process store lock, and keeps rollback available until the caller commits. Readers that consume a verified tree path across operations must hold `StoreReadGuard` for the duration; replay snapshots acquire that guard while copying.

`StorePublication::verified_tree_snapshot` verifies and copies a tree under the publication's existing exclusive lock. Frozen hydration uses these snapshots before activation, and retains rollback until the project commit decision. The snapshot is independent of subsequent publication rollback.

Lifecycle outputs carry store-local HMAC-SHA256 attestations over their recipe and output tree identities. An owner-only 32-byte `.tapid-lifecycle-key` is separate from ingested archives. Replay verifies the tag and tree together; missing, forged, symlinked, or publicly readable key material fails closed. Key creation is durable store infrastructure, while derived tree publication retains the existing transaction rollback boundary.

`Store::cache_info` reports counts and logical byte sizes for published digest-named artifacts and marked trees without creating files or recovering transactions. `Store::clean_cache` explicitly evicts that recognized data under an exclusive store lock. Staging, lifecycle keys, journals, and unrecognized entries remain with their owners. Maintenance refuses busy locks, symlinked roots/namespaces/locks, pending recovery, and populated stores without a lock. Artifact ingestion also holds the store lock, so eviction cannot race publication. A `CacheCleanup` error means eviction started and may have removed some data; retry is safe. Installed project materializations and authoritative lockfiles are not cache data.

Cache maintenance pins the store root and cache namespaces before traversal. Unix traversal and recursive removal use descriptor-relative no-follow operations. Windows holds directory and ancestor handles without delete sharing, rejects reparse points during traversal, and deletes entries through their own handles. Candidate identities are checked before deletion, so a swapped parent cannot redirect cleanup outside the inspected directories.
