# Public release verification command

The implementation of issue #123 is in progress. The supplemental command below
currently verifies public identity, asset sizes/checksums, and supported smoke
evidence. It **always exits nonzero** because native integrity and tagged package
verification are not yet connected. Do not use it as release approval. The manual
checks in [release distribution](release-distribution.md) remain normative.

```text
node --experimental-strip-types tools/release/verify-public.ts v0.0.11 --json
```

Use Node.js 22.7 or newer from a trusted checkout. No GitHub or crates.io token is
required. The command performs anonymous GET requests and neither publishes nor
executes downloaded binaries or installers. JSON mode reserves stdout for the
versioned report and writes a concise summary to stderr. Exit 1 means a failed or
incomplete report; malformed command arguments exit 2. Evidence ordering is stable;
local temporary paths and observation timestamps are excluded.

Canonical stable tags require unsigned 64-bit numeric components. The command
resolves an annotated tag directly to a commit and requires exactly one public,
immutable, non-prerelease release. Releases through v0.0.10 use six archives plus
`SHA256SUMS`. Beginning with v0.0.11, the exact eleven-asset set also includes the
record, signature, and two generated installers. Missing modern assets never
select the historical contract. Archive hashes compared with same-provider
checksums are labeled `provider-checksum`; other hashes are only observations.
This does not authenticate a signature or certify archive safety.

Each invocation uses fresh temporary asset storage and removes it on success or
failure. Asset downloads are sequential and bounded, but currently buffered in
memory. API responses are limited to 2 MiB, metadata/installers to 256 KiB, and
each compressed archive to 512 MiB. All response bodies, including failed attempts,
share a 4 GiB transfer budget. Discovery is limited to ten pages of 100 results,
requests to three attempts and five redirects, retry delays to 30 seconds,
network attempts to 60 seconds, and the entire transport to 20 minutes. HTTPS
hosts, endpoint shapes, and release asset names are explicitly allowlisted;
credentials, unexpected ports, and downgrade redirects are rejected.

Smoke verification accepts only the pinned, reviewed workflow source contracts.
It requires the resolver and all three platform jobs to succeed in one run
attempt after the selected immutable release was published. Release-event runs
must match the tag and commit. Push coordinators must match the source commit and
commit-addressed release intent. Dispatch recovery inputs are not observable in
the anonymous run API, so those runs cannot supply evidence; discovery continues
for another qualifying run. Unknown workflow revisions fail closed. Workflow
changes require a review and explicit update of the pinned source digests.

The offline Rust `inspect_release_archive` capability and hidden CLI operation
`__inspect-release-archive ARCHIVE EXECUTABLE` are implemented separately. They
inspect one target-specific regular executable without extracting or executing
it. They are not yet invoked by the TypeScript command. Existing arbitrary-byte
signature verification and installer preparation remain unchanged.

Remaining implementation includes the local native build/inspection adapter,
authenticated record parsing, owned version-specific record/signature read-back,
tagged-template installer regeneration, bounded tagged source acquisition,
offline Cargo metadata, the complete publication matrix and strict non-yanked
crates.io reads. Successful end-to-end historical and signed-record fixtures,
remaining hostile-input coverage, and native-adapter integration are required
before enabling a successful verification report. No publication workflow or
release gate is changed by this command.
