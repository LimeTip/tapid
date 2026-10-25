# Public client and private hosted ownership

Accepted direction, 2026-10-18. Implementation and rollout remain pending.

Tapid retains its public Rust workspace for independently usable local package
management. The private hosted product uses `tapid-web` for the account and
website experience and `tapid-cloud` for security intelligence and scanning.
This separates hostile artifact processing and hosted evidence operations from
the account application while preserving one user-facing product.

## Decision

The [architecture ownership section](../architecture.md#client-and-hosted-ownership)
defines the public client contract. Public schemas, evidence-verification rules,
limitations, and conformance fixtures stay in `tapid`; private applications pin
reviewed versions. The public client builds and tests without private source,
credentials, or a running service. This decision changes no licenses.

Web presents authentication, accounts and organizations. One account authority
owns membership and roles; Cloud validates credentials and authorizes its own
resources. CLI and CI authorization use scoped credentials rather than browser
cookies. The exact identity integration and revocation contract must be proven
before rollout.

Cloud owns security-evidence ingestion, analysis and issuance, including the
authoritative publication and withdrawal state. Web presents that evidence
without a second security collector or independently writable evidence record.
Private analysis is not public until separately authorized for publication.
Future native registry publication is a different contract and is not delivered
by this decision.

## Alternatives and consequences

Keeping all hosted work in Web would reduce repository coordination but combine
account implementation with security-intelligence and hostile-workload concerns.
A separate Cloud frontend would duplicate account and report experiences. Keep
one Web experience and a focused private Cloud backend instead.

The split requires versioned interfaces, explicit authoritative writers, and
cross-application revocation, withdrawal, shutdown and recovery tests. It does
not require a deployment per internal module or authorize hosting expenditure.
Separate repositories do not establish sandboxing or tenant isolation.

Start with known-report checks and read-only audits. Add measured static
analysis and explicit pre-activation policy separately; behavioral execution
requires its own isolation evidence. A signed result does not certify safety.
Track the public contract, audit, integration and policy in issues
[#312](https://github.com/LimeTip/tapid/issues/312),
[#313](https://github.com/LimeTip/tapid/issues/313),
[#314](https://github.com/LimeTip/tapid/issues/314), and
[#315](https://github.com/LimeTip/tapid/issues/315).
