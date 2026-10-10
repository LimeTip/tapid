# Preserve locked selections and hydrate pinned artifacts

Ordinary installation preserves valid locked selections, while frozen installation
permits retrieval of missing pinned content. This separates graph immutability
from network permission so repeated installs and fresh CI workers can reproduce
the project's accepted artifacts.

Ordinary installation treats a valid existing Tapid lock as the selected graph.
When the root manifest bytes, workspace membership, member manifests, registry
routing, and recorded platform contexts still match, install replays that graph
without registry metadata requests or lock rewrites. Missing verified trees may
be downloaded from their pinned archive URLs.

For changed manifests, resolution starts with the locked versions and exact
edges. Compatible direct bindings and transitive selections take precedence over
newer versions. Metadata is fetched when the existing graph cannot satisfy a
changed requirement. A changed peer provider may require fresh metadata for the
requiring package to recover its original peer range. A preferred version that
cannot satisfy the resulting peer requirements is released. Reused artifacts
retain their pinned integrity, URL, and tree digest.

`tapid update` deliberately refreshes the graph under the supported declared
ranges. `--latest` retains its existing manifest-changing behavior. Add, remove,
and ordinary install preserve compatible selections.

Frozen installation requires an unchanged lock graph, matching manifests and
workspace edges. It can download missing exact archives, but does not resolve
metadata or rewrite the lock. Offline forbids downloads. Combining frozen and
offline requires all content locally. Prune retains local-only replay.

Hydration requires registry-declared SHA-512 integrity provenance and a pinned
HTTPS URL. Downloads use the existing bounded artifact transport and its
origin-scoped credential and redirect rules. Archive validation, pinned SHA-512,
and canonical tree-digest verification precede publication. Corrupt existing
store entries fail rather than being silently repaired. Warm replay retains the
existing controlled legacy-schema support; a legacy lock without provenance or
archive locations cannot hydrate a cold store. Imported npm locks are outside
this decision.

Noncanonical persisted origins and unsupported lock schemas fail in ordinary
online installs too. No install silently migrates identities or relaxes integrity.
After preserving and verifying a separate backup, deliberately remove an
incompatible lock before generating and reviewing a replacement. Changed
routing fails during ordinary and frozen replay. An ordinary online install on
another target resolves a new graph, retaining only target-compatible locked
selections. Frozen, offline, and CI installs reject mismatched platform contexts;
an explicit `tapid update` can resolve a fresh graph for the selected routes and
target.

Store publication stays reversible until project activation and the durable
commit decision. Publication can create digest-verified snapshots while holding
the exclusive store lock, so replay does not reacquire that lock. Failure and
pre-commit crash recovery restore the previous project and remove trees published
by the attempt. Post-commit recovery keeps the decision and completes cleanup.

## Why

Repeated installation should retain the versions a project already accepted.
Lock immutability and network permission are separate requirements: fresh CI
workers need pinned archives, while offline callers require local verified trees.
The existing native lock and transactional store provide the necessary evidence
and recovery contract without changing the lock schema.

## Limits

The supported resolver and npm compatibility subset remain bounded. This does
not add automatic peer placement, portable multi-target optional graphs, imported
lock acceptance, lifecycle script execution, or registry identity migration.
Recorded target-specific packages require the matching target for replay.
Ordinary online installation can regenerate for another target.

Changed-input resolution rejects several persisted contexts for one exact registry
package version rather than collapsing them. Frozen replay retains exact contexts;
an explicit update can generate a fresh supported graph.
