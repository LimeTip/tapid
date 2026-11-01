# Package security evidence v1

This is the public client and service contract for issue #312. The Rust types in
`tapid-protocol` and verifier in `tapid-attestations` are the executable definition.
The client and documented wire contract remain open source. Cloud and Web are
separate closed-source services. Both the client and private Web consume these results.
Private Cloud owns analysis,
intelligence, issuance, publication and withdrawal. Web owns sign-in and account
interaction. One account authority owns users, organizations, memberships and roles.
No private code, account, credential, or live service is needed to build or test this
repository. [ADR 0011](adr/0011-security-evidence-contract.md) records the decision.

## Identity and applicability

`SecurityRequest` and `SecurityResult` both require `version: "tapid-security-v1"`.
A request contains one exact graph occurrence and one check. Its `subject` retains
canonical credential-free HTTPS registry origin, actual package name and exact
version, `requestedName` for aliases, stable `placement`, resolved peer identities
including their origins, and explicit `os`, `cpu`, and `libc` target context.
Optional platform values can be null. Each present value is at most 4096 UTF-8 bytes
and must also satisfy core platform validation. Placement is a consumer-owned graph
identifier with normalized relative slash-separated segments. Absolute paths,
backslashes, colons, empty segments, `.` and `..` segments are rejected on every
host platform. Clients must not put absolute local filesystem paths in it. Peer
map keys name the requested dependency, while values retain actual package identity.

Origin is never inferred from name/version. A package from a private registry cannot
match a public npm advisory merely because its name and version coincide. Copied
file/Git sources are unsupported in v1; callers must report `unsupported`, without
rewriting them as registry identities. All contextual fields participate in exact
request equality. A producer unable to represent a context must reject it.

`check` is one of:

- `{"kind":"advisory"}`. `scope` must be `package_version`. Matches concern the
  exact origin-qualified version against the named advisory source revision. They
  say nothing about downloaded bytes. The envelope binding is the literal
  `version-scope`, never an invented artifact digest.
- `{"kind":"artifact","digest":"sha256-<64 lowercase hex digits>"}`. `scope`
  must be `artifact_bytes`. The digest identifies the exact downloaded archive,
  before extraction or lifecycle execution. It appears in both the signed request
  and envelope `artifact_digest`. Derived trees need a future explicit scope.

Each request names nonempty unique `coverage` identifiers understood by the named
scanner. Artifact coverage might include static archive analysis and install-script
inspection. Advisory coverage identifies advisory feeds. These are provider claims;
the verifier checks coverage identifiers, not whether a scanner performed its work.
The result repeats the exact request, including `audience`, which is either
`{"visibility":"public"}` or `{"visibility":"tenant","tenant":"opaque-id"}`.
A private result cannot be substituted for a published result or another tenant.

## Results and explanations

A result carries issuer, sequence, scanner identity and version, rule version,
source revision, scope, completed coverage, `checkedAt`, `issuedAt`, `expiresAt`,
findings with stable IDs, summaries and severities, and explicit limitations.
Times are integer UTC Unix seconds. Require checked <= issued < expires. Sequence
starts at one and increases per issuer and exact request. All signed integers are
at most 9007199254740991, preserving exact values across Rust and JavaScript/JCS.
Source revisions must identify an immutable feed snapshot or analysis input revision;
scanner and rule versions alone do not establish which intelligence was consulted.

The following statuses remain distinct:

- `findings`: at least one finding, with all requested coverage completed.
- `no_known_findings`: no findings within all requested coverage at the stated time.
- `partial`: some analysis or intelligence was incomplete. Findings may still exist.
- `not_scanned`: no analysis performed.
- `unavailable`: the producer could not supply an analysis result.
- `stale`: the producer explicitly reports out-of-date analysis.
- `unsupported`: the requested analysis or context is not supported.
- `invalid_evidence`: the producer reports unusable or withdrawn evidence.

Every status other than `findings` and `no_known_findings` requires a limitation.
Not-scanned, unavailable, unsupported and invalid-evidence results carry no completed
coverage or findings. A complete status with missing coverage fails verification as
`incomplete_coverage`; it is never silently converted to no known findings. Completed
coverage must be unique and a subset of requested coverage. Partial and stale results
may retain earlier findings but never qualify as complete current analysis.

Local verification failures return `SecurityReason`, not a fabricated signed result.
`expired` maps to local stale evidence. Other verification failures map to local
invalid evidence, retaining the precise code. A transport outage is unavailable,
not an authentic issuer claim. Unknown versions remain unsupported. None of these
statuses certifies safety. A valid signature establishes issuer/content authenticity,
not scanner accuracy, advisory completeness, absence of malware, or runtime safety.
`VerifiedSecurityEvidence` means the envelope passed verification; inspect its status
before any policy decision. No new install policy is enabled by this contract.

## Issuer authentication, trust and replay

Results use `tapid-signatures::TrustEnvelope`, version `tapid-trust-envelope-v1`,
subject `tapid-security-v1`, with the entire typed result in `claims`. Use the existing
RFC 8785 JCS contextual Ed25519 payload, which authenticates algorithm and key ID as
well as claims, subject, and artifact binding. The envelope's detached signature is
required. HTTPS, deserialization, `TrustEnvelope::verify()`, and an unsigned envelope
do not establish issuer authenticity. Use `verify_security_evidence`.

The caller supplies `SecurityTrust` containing one authorized issuer, its dedicated
keyring, and the expiry of its independently authenticated trust configuration. No
production evidence keys are shipped yet. Release signing keys are a different trust
root. Never accept keys from an evidence response, login session, or unauthenticated
service discovery. Key IDs are unique, follow the signatures crate's restricted
syntax, and must not be reused after revocation.

Trust distribution is a prerequisite for production integration. Ship pinned issuer
keys in a client release or deliver a versioned trust update authenticated by an
already trusted administrative root. Deliver updates with monotonic revision,
expiry, issuer-to-key bindings and revoked key IDs. Persist its revision against
rollback. Rotation may overlap old and new authorized keys until the old key is
removed; revocation removes a key immediately even for previously accepted results.
A service result cannot extend its own key authority. This repository does not
implement remote trust-update delivery or a revocation feed. The embedding caller
must authenticate and refresh that configuration; `valid_until` prevents indefinite
use of an expired configuration.

`EvidenceFreshness` supplies trusted current time, a maximum age since `checkedAt`,
and the stored sequence floor plus previous envelope digest. Future issuance, expired
results, expired trust, and over-age analysis fail closed. At the sequence floor,
only the exact previous digest is reusable; lower sequences or different content at
the same sequence are replay. Reissuing stale analysis with a newer issue time does
not reset its checked time. Larger sequences still require all other checks.

Persist the highest accepted sequence and digest atomically before allowing evidence
to influence a security decision. Scope this state by issuer and canonical exact
request. Include both complete and withdrawal/negative statuses. Corrupt state must
fail closed; do not reset the floor silently. Removing state loses rollback detection.
On first use, time/maximum-age checks bound replay but cannot prove the absence of a
newer result or withdrawal. Network blocking, clock rollback and delayed revocations
remain limitations; offline evidence must disclose its last trust refresh and age.

## Hosted identity and authorization

Account authorization and evidence issuer trust have independent keys and validation.
Cloud and Web share the same account authority, rather than creating separate user,
organization, membership or role databases. Hosted identity-provider implementation
is private and outside this contract.

Use an opaque, short-lived bearer credential obtained through Web's sign-in flow.
Send it only to its configured exact service origin over HTTPS, never registry
origins or redirected hosts. Browser session cookies are not CLI credentials. Cloud
validates the credential through authenticated authority introspection on every
private request. Unavailable introspection fails closed. This gives revocation and
membership changes effect on the next authorization check. Credentials are never
included in signed evidence, lockfiles, logs, or conformance fixtures.

The authority returns `AccountAuthorization`, pinned to `tapid-security-v1`, with
`authority`, opaque user `subject`, opaque `tenant`, exact service `audience`, scopes,
issued/expiry times, and `credentialId`. The service must authenticate introspection,
match the configured authority and audience, require issued <= now < expires, reject
revoked credentials, and authorize the selected tenant and operation against current
membership. These DTOs are untrusted until that authentication succeeds. There is no
public token verifier or account implementation here. Roles are authority-owned;
consumers use granted scopes instead of inventing a second role model.

The scopes are `read_private`, `submit_analysis`, and `publish_evidence`. The last
is a separate permission, never implied by read or submit. All private retrieval
requires `read_private` for the exact tenant. Submission requires `submit_analysis`.
Changing visibility requires an explicit publication action authorized for that
tenant and evidence digest with `publish_evidence`. It issues a new public envelope
with public audience and a sequence in that public request's stream. Do not edit or
republish the private envelope directly. The publication review must explicitly
approve every disclosed package, peer, graph-context and finding field. Private
package identity must not become discoverable through public lookup failures.

Public retrieval of deliberately published evidence remains anonymous, requiring no
account or account claims. A credential cannot make an unpublished record public.
Public withdrawal is a higher-sequence signed `invalid_evidence` result, empty
coverage/findings and a withdrawal limitation; removing an HTTP object alone cannot
invalidate cached copies. Withdrawal is subject to the freshness limits above.
Private records and publication authorization remain service-owned access controls.

The service binding uses these operations, under a configured service origin:

- `POST /v1/security/evidence/query`, body `SecurityRequest`, returns a signed result.
  Public requests may be anonymous. Private requests require tenant authorization.
  This is retrieval only; querying never authorizes an upload or publication.
- `POST /v1/security/analysis`, body `SecurityRequest` for artifact analysis, requires
  `submit_analysis`. v1 defines no archive-upload transport; reject submissions whose
  exact digest is not already available through separately authorized storage.
- `POST /v1/security/publications`, body `EvidencePublicationRequest`, requires
  `publish_evidence` and checks tenant, verified private evidence digest and sequence
  before publishing. A stale sequence fails as invalid request; retry requires a new
  explicit publication decision.

These paths and authorization rules specify conformance, not implemented endpoints.
Web and CLI must pin the contract version independently of their own release numbers.

Errors use `SecurityErrorResponse` with version, enum code and optional
`retryAfterSeconds`. Codes and HTTP semantics are authentication_required,
invalid_credential, credential_expired, credential_revoked, wrong_audience at 401;
forbidden and tenant_mismatch at 403; not_found at 404; rate_limited at 429;
unavailable at 503; unsupported_version and invalid_request at 400. Do not expose
whether an unauthorized private record exists; retrieval returns `not_found` for
both missing and unauthorized private records. Detailed authorization reasons are
for the caller's own credential/tenant operation, not cross-tenant existence probes.
Retry metadata conveys no security result, and errors do not advance replay state.

## Bounds, evolution and fixtures

Requests, results and errors are UTF-8 JSON. Transport adapters must cap bodies,
including decompressed data, at 262144 bytes before collecting or parsing them.
The verifier enforces this result limit before parsing. Keep serde's recursion limit.
Reject malformed JSON, duplicate struct fields and peer keys, unknown fields and enum
values, unknown envelope/schema versions, noncanonical origins/digests/versions,
invalid package names, invalid timestamps and inconsistent findings or coverage.
Optional platform fields and retry metadata may be omitted. Peer maps have at most
128 entries, coverage and limitations at most 128 each, and findings at most 256.
Nonempty descriptive strings are at most 4096 bytes and contain no control characters.
Package and platform values additionally follow `tapid-core` validation.

Schema v1 is closed. Additive fields or enum cases require a new negotiated contract
version; a client must not guess how to downgrade evidence. Keep v1 support during a
migration until the previous compatible consumer is retired. Publish conformance
fixtures alongside every revision. Current readers and the frozen independent v1 consumer
schema test both read the checked-in artifact and advisory fixtures. The frozen reader covers the full signed envelope, request, result and hosted DTOs,
using independent types and real signature verification. This is the
initial v1 baseline, not a claim that a previous production client or Web deployment
already supports this contract. Future compatible consumer revisions must keep these
fixtures and their accepted meanings. A new version needs separate fixtures and
explicit cross-version rejection tests.

[Offline fixtures](../crates/tapid-attestations/tests/fixtures/) use RFC 8032 public
test material, fake registries, synthetic scanners and fixed historical test times.
They must never be trusted in production. Run:

```text
node --experimental-strip-types scripts/dev.ts test -p tapid-attestations --test security_evidence --locked
```

Tests cover applicability substitution, digest mismatch, unsigned/tampered evidence,
unknown version/issuer/key, removed keys, expiry, future issuance, replay/equivocation,
incomplete coverage, unsafe integers, contradictory status, duplicates and bounds.
They make no claim about live scanner quality, hosted authorization enforcement,
production trust distribution, persistent replay storage, cross-platform networking,
or a deployed Cloud/Web integration.

## Storage and capability ownership

Security evidence changes without changing package resolution identity. Do not put it
in the lock schema, store artifact key, resolver identity or tree-integrity proof.
No lockfile version changes in this work. A future local adapter must store raw signed
envelopes in a separate owner-only cache, keyed by issuer, audience/tenant and canonical
request digest; artifact requests therefore include the artifact digest. Keep public
and tenant namespaces separate and never share private records in a global cache.
Reverify raw bytes, current trust, applicability, coverage and freshness on every use.
Expiration is a status, not permission to overwrite a newer sequence with old bytes.

Store replay floors separately from evictable evidence bodies. Use atomic writes and
recovery for floors and configuration revisions; cache garbage collection must not
remove them. Persist a content digest for accepted equal-sequence reuse. No local
persistence is introduced here. A future adapter needs interrupted-write, concurrency,
permissions, tenant-isolation and garbage-collection tests before activation.

`tapid-protocol` owns wire DTOs and pure structural validation using existing core
identity values. `tapid-attestations` owns applicability and evidence verification,
reusing `tapid-signatures` contextual signing and caller-owned keyrings. Existing
`AttestationClaims` remains a distinct generic artifact-claim contract; it lacks
version-only scope and the service/replay model, so it is not relabeled as a security
result. `tapid-policy` continues to own policy decisions. No new policy or CLI provider
rules are added. Future CLI and Web adapters compose these capabilities.

## Security report contract v2

[ADR 0012](adr/0012-classified-evidence-contract.md) extends the closed v1
contract using `tapid-security-v2`. V1 types, verifier and frozen fixtures remain
supported with their original meanings. V2 uses `SecurityRequest::validate_v2`,
`SecurityResultV2` and `verify_security_evidence_v2`. The envelope subject, result
version and request version must all select v2. Do not downgrade v2 requests to v1
or interpret v1 summaries or report IDs as classification or fix metadata.

V2 retains the exact subject, audience, check, scope, times, source revision and
sequence fields. Registry-version lookup uses `check.kind = "advisory"` and
`scope = "package_version"`; its coverage identifiers are `advisory` and
`reported_malware`. Each may be requested independently. A graph audit needs both
before claiming complete registry-version report coverage. A public registry
origin does not establish that its package identity may be disclosed publicly.
This wire contract grants no permission to submit private inventories or paths.

Each v2 finding requires:

- `id`: stable issuer-owned report identity.
- `revision`: nonempty opaque immutable report revision. This is distinct from the
  per-exact-request monotonic envelope sequence and is never ordered lexically.
- `classification`: `advisory` or `reported_malware`, never inferred from text.
- `state`: `active` or `withdrawn`.
- `summary`, optional `severity` (null/omitted means unknown), and `fixes`.
  Each fix contains an exact canonical package `version` at the subject's existing
  origin and actual name. An empty list means no fix information is supplied,
  rather than proving there is no fix. Fixes are information, not a resolution or
  compatibility guarantee. Each report has at most 128 fixes.

Report IDs are unique within a response. An active registry-version report must
belong to both requested and completed classification coverage. Artifact checks
retain a real existing digest, `artifact_bytes` scope and scanner-defined coverage;
registry-version reports never imply archive inspection. All v1 bounds, strict
parsing, contextual signatures, trust expiry, freshness and replay checks apply.

Withdrawn reports are retained only in `invalid_evidence` results, with explicit
limitations, empty completed coverage and no active reports. Their new report
revision and higher envelope sequence preserve history without treating withdrawal
as a no-known-findings result. Callers must durably advance the sequence/digest floor
before using any result, including withdrawals. A prior active envelope is then
rejected as replay. The pure verifier does not persist that floor itself and cannot
know about a withdrawal it has not received. Equal-sequence equivocation is rejected.

The generic result storage shares metadata validation between v1 and v2; the
version-specific finding schemas remain closed. A verified v2 result still needs
status and policy evaluation. This slice adds no live service, cache, account
requirement or audit CLI. Production issuer provisioning, hosted compatibility,
privacy/cache policy and transport remain prerequisites for later activation.

The checked-in [v2 fixture](../crates/tapid-attestations/tests/fixtures/advisory-v2.json)
and independent v2 consumer agree on report identity/revision, classification and
fix metadata; the frozen v1 schema rejects it. These fixtures do not prove deployed
Web compatibility. Run:

```text
node --experimental-strip-types scripts/dev.ts test -p tapid-attestations --test security_evidence_v2 --locked
```
