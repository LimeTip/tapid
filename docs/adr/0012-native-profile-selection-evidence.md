# ADR 0012: Native profile selections and pending source evidence

Status: Accepted for implementation; installation integration pending.

## Decision

Schema 11 separates the complete native accepted selection from a materialized
profile. Typed root and workspace declarations, required/optional package edges,
and exact required peer-provider edges determine reachability. Validate every
selected record before computing a closure, including omitted records. A closure
is a set of exact keys, never a replacement lock. Legacy native and imported
readers retain their existing contracts and reject this schema.

A registry selection may be pending only with a pinned approved HTTPS archive URL
and canonical registry-declared SHA-512 integrity. Pending selections have no tree
digest and cannot satisfy verified-tree interfaces. Verified selections reuse the
existing source and derived-output evidence contract. Copied selections cannot
use pending registry evidence.

Materialization must verify the pinned archive, extraction and package identity
through existing archive/store capabilities before publication. Recorded tree
digests remain mandatory comparisons. Mutable installed metadata is never
independent authority for an archive-to-tree relationship: reverify the pinned
archive unless an existing validated interface establishes that relationship.
Offline replay fails safely if that evidence is unavailable. Frozen replay does
not rewrite the selection lock to add receipts.

Stage installed profile and receipts inside the managed installation tree, bound
to selection, manifests, target, exact keys and layout generation. Existing project
transactions and lifecycle journals activate and recover this state with layouts
and bins. Derived outputs still require authenticated graph-sensitive recipes,
approvals and containment; a profile record cannot authorize hook execution.

## Consequences

Cold production selection can retain omitted packages without downloading their
archives. Complete metadata resolution may still be required. Native schema 11
must be explicitly dispatched by consumers before it is emitted by installation.
Non-full imported and insufficient-evidence legacy replay reject rather than
infer transitive kinds. This decision does not change legacy verification, add
automatic peer placement, or establish that profile installation is shipped.
