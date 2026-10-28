# tapid-security-provider

Explicitly authorized retrieval of signed v2 security reports. `retrieve` accepts
an exact request, disclosure decision, issuer trust, freshness/replay inputs and
a raw-stream adapter. It validates requests before transport, reads at most
262,145 decompressed bytes, and reuses `tapid-attestations` for exact applicability,
signature, freshness and replay verification. Public report access does not
authorize disclosing an inventory: `Disclosure::Denied` is the default. Consent
must match the entire request, including audience, peers and placement.

The adapter cannot return preverified claims or inject a verifier bypass. Tests
use frozen signed conformance input and RFC public test keys; the crate has no
default keys, environment configuration, credential lookup or network adapter.
Read failures return an unavailable state without exposing transport error text.
Adapters must enforce read timeouts and credential authorization independently.

`retrieve_recorded` additionally withholds verified evidence until the supplied
replay-state writer succeeds. It passes the issuer, entire request, verified
sequence and envelope digest to the writer. The writer must atomically compare
against current state and durably publish the new floor, rejecting concurrent
rollback or equivocation. Writer failures return `StateUnavailable`; denied
disclosure and invalid evidence never invoke the writer. This interface does
not itself provide a durable filesystem store or make an in-memory writer durable.

This is the retrieval foundation, not a complete audit service. Durable replay
persistence, credentials, cache, collection budgets and live transport remain
unimplemented. Callers must supply trusted replay floors and persist accepted
sequence/digest state before using evidence for policy. A verified response alone
does not establish collection completeness. No project, store, artifact or
transaction operation is performed here. See [security evidence](../../docs/security-evidence.md).
