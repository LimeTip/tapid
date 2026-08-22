# tapid-store


[Crates.io](https://crates.io/crates/tapid-store) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-store)

`tapid-store` is a filesystem-authoritative content-addressed store. `Store::ingest` accepts any `std::io::Read`, streams bytes through SHA-256 into a private staging file, calls `sync_all`, verifies the requested `ArtifactDigest`, and atomically activates the staged file under the dynamic store root. Digest paths are never overwritten. An existing regular file is authoritative and ingestion is idempotent. Failed reads and digest mismatches are cleaned up without activating partial bytes.

The CLI also replays verified package trees at `STORE/trees/<sha256-...>/`. Each tree requires a regular `.tapid-tree` marker containing the exact digest before offline or frozen installation can use it. The store does not fetch archives, run lifecycle scripts, garbage-collect, coordinate multi-process leases, or provide a remote cache.

The archive validator is a separate crate and is intentionally not a dependency of this store.
