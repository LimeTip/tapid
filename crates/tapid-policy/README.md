# tapid-policy


[Crates.io](https://crates.io/crates/tapid-policy) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-policy)

Explainable policy planning primitives. Evidence is typed as `declared`, `inferred`, `observed`, `enforced`, or explicitly ambiguous; policy decisions are `allow`, `warn`, `prompt`, or `deny` with stable reason codes and deterministic JSON serialization.

This crate makes no execution or containment claim. Unattended operation fails closed when evidence would require an interactive decision.
