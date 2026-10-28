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
default keys, implicit environment access, credential lookup or network adapter.
Read failures return an unavailable state without exposing transport error text.
Adapters must enforce read timeouts and credential authorization independently.

`retrieve_recorded` additionally withholds verified evidence until the supplied
replay-state writer succeeds. It passes the issuer, entire request, verified
sequence and envelope digest to the writer. The writer must atomically compare
against current state and durably publish the new floor, rejecting concurrent
rollback or equivocation. Writer failures return `StateUnavailable`; denied
disclosure and invalid evidence never invoke the writer. An in-memory writer does
not provide durability; the Unix adapter below supplies a durable filesystem
implementation.

`collect_recorded` evaluates exact requests in input order and retains successful
reports alongside per-request failures. It admits at most 1,000 inputs and reads
at most 32 MiB of decompressed responses across the collection, including invalid
and oversized responses. Each response still has the existing 262,144-byte
verification limit. Results correspond to the processed input prefix; `not_checked`
counts the remaining suffix, and `limit` explains request/response-budget exhaustion.
`MAX_COLLECTION_REQUESTS` exposes the same bound so adapters can limit preparation
before loading replay floors, freshness or consent for individual occurrences.
Adapters that prepare a bounded prefix must retain the unprepared suffix count
and cannot claim complete graph coverage from that prefix alone.
The response that exhausts the aggregate read budget fails closed as unavailable.
Processing is sequential, so a collection uses one transport request at a time.

Collection completeness requires a nonempty collection, no unchecked inputs,
successful replay recording, and current signed full-coverage results without
limitations. It does not establish reachable-graph coverage, authorization to
private evidence, scanner accuracy or artifact analysis. No report is discarded
because another request failed. The caller must still enforce graph completeness
and policy thresholds. Transport timeouts and collection deadlines remain the
adapter's responsibility; this synchronous interface cannot interrupt a blocked read.

This is the retrieval foundation, not a complete audit service. Live credentials,
cache bodies, deadline enforcement and live transport remain unimplemented.
Callers must supply trusted replay floors and persist accepted
sequence/digest state before using evidence for policy. A verified response alone
does not establish collection completeness. No project, verified-package store,
artifact or transaction operation is performed here. See [security evidence](../../docs/security-evidence.md).

On Unix, `ReplayStore` supplies the durable callback for `retrieve_recorded`:
`load(issuer, request)` returns the optional sequence/digest floor, and `record`
compares again under a permanent cross-process file lock before publishing.
The caller must supply a trusted, stable, owned parent directory. The adapter
creates a mode-0700 state directory and mode-0600 files, rejects symlinks,
hard-linked or malformed floors, and hashes the issuer plus entire validated
request into filenames. It stores no envelope bodies or plaintext inventory.
Directory identity and permissions are rechecked on operations. This protects
against other users, not a malicious process running as the same user or local
state deletion/backup rollback.

Floor updates sync a staging file, atomically rename it over the floor, and sync
the directory before returning. Interrupted staging never replaces the old floor;
a retry discards it under the lock. Errors withhold evidence, even if publication
may already have happened. Equal-sequence retries still sync publication. Cache
body eviction must never delete this directory. There is no eviction API.
Windows persistence, CLI configuration loading and live provider activation remain
unimplemented; the Unix adapter alone does not enable network requests.

`ProviderConfig::parse` accepts at most 64 KiB of version-1 JSON adapter
configuration. It requires a canonical credential-free HTTPS `origin` and an
explicit `"cache":"disabled"` policy. Optional `accountCredentialEnv` and
`headlessCredentialEnv` name separate credential sources; they cannot name the
same variable, including names differing only in ASCII case so configuration is
portable to case-insensitive environments. Unknown fields, duplicate fields,
unsupported versions, invalid variable names and unimplemented cache modes fail
closed. No secrets belong in this document. It is an adapter input, not a
discovered project configuration or a live CLI activation mechanism.

`authorize` requires a separately consented exact request and exact destination
origin before invoking the supplied credential lookup. Account sign-in alone
grants no inventory disclosure. Anonymous mode permits only public audiences;
headless mode never falls back to account credentials. Empty, malformed or missing
credentials fail closed without returning their contents in errors. The caller
must disable transport redirects and pass the actual destination origin. Existing
credentials require no browser refresh at this gate; Cloud must still enforce
current authorization and fail closed if its authority is unavailable. Parsing
and authorization perform no network requests, browser operations or writes.
