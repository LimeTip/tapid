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
default keys, implicit environment access or implicit credential lookup.
Read failures return an unavailable state without exposing transport error text.
Adapters must enforce read timeouts and credential authorization independently.

`retrieve_recorded` additionally withholds verified evidence until the supplied
replay-state writer succeeds. It passes the issuer, entire request, verified
sequence and envelope digest to the writer. The writer must atomically compare
against current state and durably publish the new floor, rejecting concurrent
rollback or equivocation. Writer failures return `StateUnavailable`; denied
disclosure and invalid evidence never invoke the writer. An in-memory writer does
not provide durability; the platform adapters below supply a durable filesystem
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

This is the retrieval foundation, not a complete audit service. Credential
provisioning and cache bodies remain unimplemented. CLI activation requires explicit
trusted inputs; see the CLI README.
Callers must supply trusted replay floors and persist accepted
sequence/digest state before using evidence for policy. A verified response alone
does not establish collection completeness. No project, verified-package store,
artifact or transaction operation is performed here. See [security evidence](../../docs/security-evidence.md).

On Unix and Windows, `ReplayStore` supplies the durable callback for `retrieve_recorded`:
`load(issuer, request)` returns the optional sequence/digest floor, and `record`
compares again under a permanent cross-process file lock before publishing.
The caller must supply a trusted, stable, owned parent directory. The adapter
creates a mode-0700 state directory and mode-0600 files on Unix, rejects symlinks,
hard-linked or malformed floors, and hashes the issuer plus entire validated
request into filenames. It stores no envelope bodies or plaintext inventory.
Directory identity and permissions are rechecked on operations. This protects
against other users, not a malicious process running as the same user or local
state deletion/backup rollback.

On Unix, floor updates sync a staging file, atomically rename it over the floor,
and sync the directory before returning. Interrupted staging never replaces the
old floor;
a retry discards it under the lock. Errors withhold evidence, even if publication
may already have happened. Equal-sequence retries still sync publication. Cache
body eviction must never delete this directory. There is no eviction API.
Neither persistence adapter alone enables network requests.

Windows creates the directory, lock and staging files with a protected DACL
containing only the current process-token owner, granting that owner full access.
Opened handles validate the exact canonical owner/DACL without repairing existing
ACLs. Reparse points, directories in place of files, and multiply linked files
fail closed. A held directory handle denies delete sharing, pinning the root
against rename/replacement. Cross-process locking uses the same permanent lock
and shared floor comparison. Publication flushes the staging file and uses
same-directory `MoveFileExW` with replacement and write-through, without a
copy/delete fallback. The caller must supply trusted stable ancestors on a local
filesystem supporting Windows ACLs and these operations. Filesystem or flush
errors withhold evidence. Native Windows tests cover ACL broadening, junctions,
root replacement, hard links, interrupted staging and separate-process writers;
cross-compilation alone does not establish these runtime guarantees. Neither
platform prevents same-user state deletion or backup rollback, and interrupted
staging tests do not simulate a power failure.

`HttpsProvider` is an explicitly constructed query adapter for the configured
origin's `POST /v1/security/evidence/query` endpoint. It requires exact inventory
consent through `ProviderConfig::authorize` before credential lookup or transport.
It sends the exact v2 request as JSON and returns an unverified response stream;
use it through `retrieve_recorded` or `collect_recorded`, retaining their byte,
verification and durable replay gates. HTTP success alone is not evidence.
There are no retries, redirects, environment proxies, identity refreshes, cookie
sessions or body caching. Existing account and headless credentials remain
separate. Responses other than HTTP 200 and nonidentity content encodings fail
closed; the adapter requests identity encoding, so retrieval's byte bound applies
directly. Transport failures expose only `Unavailable`, without URLs or secrets.

Construction requires nonzero request and collection timeouts of at most five
minutes. The collection deadline starts at construction. Each query uses the
lesser of its request timeout and remaining collection time, covering connection,
response headers and body reads. Expired collections make no credential lookup
or request. Credential callbacks and replay filesystem operations are caller-owned
and cannot be interrupted by the HTTP deadline. TLS tests use the registry
client's frozen public certificate/key fixtures, copied locally for packaging;
the POST fixture additionally verifies full inventory delivery. These loopback
tests do not establish production service availability or hosted authorization.

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

`ProviderConfig::parse_consent` parses a separate version-1 JSON document with
exact `origin` and `requests`. It validates every v2 request, rejects duplicates,
unknown fields and mismatched destinations, and bounds input to 1 MiB and the
shared 1,000-request collection limit. Empty requests deny all inventory. The
caller must explicitly choose a trusted consent document; parsing is never
authorization to discover project files or infer consent from credentials.
