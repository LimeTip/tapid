# tapid-store

`tapid-store` is a filesystem-authoritative content-addressed store. `Store::ingest` accepts any `std::io::Read`, streams bytes through SHA-256 into a private staging file, calls `sync_all`, verifies the requested `ArtifactDigest`, and atomically renames the staged file into the dynamic store root. Digest paths are never overwritten; an existing regular file is authoritative and ingestion is idempotent. Failed reads and digest mismatches are cleaned up without activating partial bytes.

The archive validator is a separate crate and is intentionally not a dependency of this store.
