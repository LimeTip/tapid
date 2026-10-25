# ADR 0011: Public security evidence contract and separate issuer trust

Tapid's public repository owns versioned security interchange contracts, verification
rules and offline conformance fixtures. Private Web owns sign-in and account interaction,
with one account authority for users, organizations, memberships and roles. Private Cloud
owns intelligence, analysis, issuance and publication or withdrawal. Client and Web
consume the same results. Public builds and local operations require no private code
or accounts. The [v1 contract](../security-evidence.md) defines the guarantees and limits.

Keep account authorization separate from issuer authentication. An account session or
HTTPS response cannot establish scan-result authenticity. Reuse contextual Ed25519
trust envelopes with independently provisioned issuer keys, explicit trust expiry,
exact request/digest binding and persisted replay floors. Signatures authenticate the
issuer and content; they do not prove scanner accuracy or safety. Deliberately published
public evidence stays anonymously readable, while private retrieval and publication
require distinct tenant authorization.

Changing security evidence remains outside immutable resolution and lock identity.
A future adapter stores signed envelopes and durable replay floors separately. Putting
mutable evidence in lockfiles would turn intelligence refresh into a resolution change
and still would not establish freshness. Generic artifact attestations stay separate
because advisory matches are version-scoped and must never imply byte analysis.

ADR 0010 already identifies Windows dependency write trees in this checkout, so this
decision uses 0011. No production trust roots, hosted implementation, persistence adapter
or install-time security enforcement are introduced by this decision.
