# ADR 0012: Negotiate security report metadata through v2

[ADR 0011](0011-security-evidence-contract.md) defines closed signed security
schemas. Locked-graph audit needs report classification, revision, withdrawal and
available fixes; adding them to v1 would let an older consumer discard security
semantics. Introduce `tapid-security-v2` with separate validation and verification
entry points, retaining v1 and its frozen fixtures unchanged. Both versions reuse
the same exact identity, contextual signature, freshness and replay rules.

A report revision is an opaque issuer-owned immutable revision, distinct from the
monotonic per-request envelope sequence. Explicit withdrawn reports retain identity
and revision under `invalid_evidence`, with no completed coverage or active findings.
They cannot become a clean assessment. Persisting their higher sequence before
policy evaluation prevents reusing an earlier active result; the pure verifier
continues to require caller-owned durable replay state.

The [v2 contract](../security-evidence.md#security-report-contract-v2) distinguishes
registry-version advisory and reported-malware coverage from digest-bound artifact
analysis. Negotiation cannot silently fall back to v1 and lose those semantics.
Independent offline consumers establish portable interchange only. They do not
establish deployed Web/Cloud support or provide production trust roots, transport,
authorization or an enabled audit command.
