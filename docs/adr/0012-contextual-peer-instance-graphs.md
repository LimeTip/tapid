# ADR 0012: Separate contextual placements from reusable artifacts

For [#306](https://github.com/LimeTip/tapid/issues/306), retain selected runtime
placements as deterministic opaque references with separate named ordinary and
exact peer-provider edges. A source-qualified artifact can occur in several
placements. Version-only peer summaries cannot distinguish equal-version
providers with different sources or their own bindings; recursively embedding
provider identities cannot represent mutual peers finitely.

The resolver shares ordinary candidate selection with its legacy interface.
Allocate siblings before binding peers; search local names from the consumer's
enclosing scope outward, retaining nearest-binding shadowing and routed-source
and alias checks. Optional absence does not invent a provider. Distinct
placements are retained conservatively, allowing downstream verified content to
remain independent of runtime placement.

Dependency recurrence can reuse an active ancestor only when exact enclosing
provider references agree for all reachable peer names. This supports stable
external providers and peer-bearing descendants. The comparison deliberately rejects
some equivalent environments when a changed name is shadowed inside the repeated
subtree. Context-changing recurrence is rejected. Instance, edge, depth, text,
cycle-analysis work, and diagnostic limits bound this conservative expansion.

This first implementation supplies resolver graph selection only. The existing
CLI path remains fail-closed for richer native topologies. Schema 11, exact graph
replay and linking, contextual maintenance preferences, workspace/platform
evidence, and runtime/transaction acceptance remain required before native
installation support can be claimed. Schema 8 import still validates a selected
graph rather than selecting candidates; preserve its exact edges through a
narrow adapter instead of copying the importer into the resolver.
