# Recover selected manifest edits under one durable decision

Multi-member mutations require one recoverable project operation: individually
committing each manifest would leave earlier members changed when a later edit
fails. Lifecycle journal version 4 therefore records a complete collection of
root-relative manifest paths and original bytes, sharing the existing root lock,
store coordinator, activation recovery, and durable commit decision.

Recovery retains versions 1–3. Single-manifest writers retain their existing
version 2 representation (version 3 when member activations are recorded). Version
4 uses `manifests`, with `path` and base64 `bytes` for every original manifest;
the legacy `manifest` field is empty and `manifest_path` is `package.json`.
Both store coordinator readers accept version 4 so store publication and crash
recovery observe the same Prepared or Committed decision. Older clients must
reject version 4 rather than attempt partial recovery.

Every recorded path and decoded snapshot is validated before restoring any file
or recovering the shared store. Unsafe paths, aliases, duplicate targets,
malformed snapshots, and unknown versions fail closed, retaining recovery
evidence. The existing 32 MiB per-snapshot and 96 MiB encoded aggregate journal
limits include all selected manifests, the root lock snapshot, and structural
overhead. Adding members does not increase these limits. This is pathname
validation, not protection against arbitrary concurrent filesystem writers.

Prepared recovery restores every manifest and the root lock; interrupted
restoration can be retried from the retained journal. Committed recovery keeps
the committed state and completes cleanup. Recovery failure and committed
cleanup failure remain distinct from successful rollback. This format capability
does not itself enable multi-member command orchestration.
