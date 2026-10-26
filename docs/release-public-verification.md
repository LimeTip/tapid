# Public release verification command

Use this supplemental command from a trusted checkout to read back a completed
stable release. The manual checks in [release distribution](release-distribution.md)
remain the normative fallback. This command does not publish, replace assets,
dispatch workflows, execute downloaded programs, or change release gates.

```text
node --experimental-strip-types tools/release/verify-public.ts v0.0.11 --json
```

Prerequisites are Node.js 22.7 or newer, Cargo and Rust. The command builds its
native verifier from the local trusted checkout using locked dependencies and
worktree-local `target` output. A cold build can require package-registry access;
that build is separate from anonymous public evidence requests. Cargo's JSON
artifact metadata supplies the executable path. No downloaded executable or
installer runs. No GitHub or crates.io token is required or forwarded to public
requests. Native inspection runs with an empty environment (except Windows'
`SystemRoot`). Child diagnostics are sanitized rather than copied into reports.

JSON mode reserves stdout for the versioned report and writes a concise summary
to stderr. Exit 0 means every required check passed; exit 1 means a failed or
incomplete report; malformed command arguments exit 2. Reports include tag and
release identities, checks, ordered assets with SHA-256 evidence, smoke jobs and
package versions. Local temporary paths and observation timestamps are excluded.
Identical evidence yields identical JSON. A prerequisite failure marks dependent
checks as not performed; independent smoke and package checks still run.

Canonical stable tags require unsigned 64-bit numeric components. The command
resolves an annotated tag directly to a commit and requires exactly one public,
immutable, non-prerelease release. Releases through v0.0.10 use six archives plus
`SHA256SUMS`. Beginning with v0.0.11, the exact eleven-asset set also includes the
record, signature, and two generated installers. Missing modern assets never
select the historical contract.

All archive provider sizes and checksums must match. The native Rust inspector
requires one target-specific regular executable and rejects malformed or unsafe
archives without extracting them. Historical hashes are labeled
`provider-checksum`: they provide weaker, same-provider evidence without an
independent signature. Modern records are authenticated with the native
verifier's production keyring, then compared byte-for-byte with the existing
publisher's canonical record generator over the downloaded archives. This
checks every target, name, version, size, digest, row and immutable URL without a
second record parser. Modern archive hashes are labeled `authenticated-record`
only after all integrity checks pass; other asset hashes remain observations.
The owned version-specific record/signature routes must match the assets.
Installers must exactly match regeneration from commit-addressed source templates
and the archive pins. Latest routes and legacy `stable.json` are not consulted.

Package expectations come from the peeled commit's complete Git tree, rejecting
truncated discovery, duplicate or unsafe paths and nonregular selected files.
Only Cargo manifests, lockfiles and Rust source targets are materialized; Cargo
configuration is never downloaded. Cargo metadata runs offline, locked and with
no dependencies, from the trusted checkout directory with an isolated Cargo
home. Tagged build scripts and programs never run. The existing publication
planner derives the complete package matrix, including previously published
versions. Every exact crates.io version must have matching identity and
`yanked=false`. The tagged CLI version must match the requested release.

Each invocation uses fresh temporary asset/source storage and removes it on
success or failure. Installer regeneration uses a separate temporary directory.
Downloads are sequential and currently buffered in memory. Limits are:

- API responses: 2 MiB; metadata/installers/templates: 256 KiB.
- Release archives: 512 MiB compressed and 512 MiB expanded per archive.
- All response bodies, including failed attempts: 4 GiB total.
- Source discovery and transfer, including retries: 256 MiB; selected materialized
  files: 1 GiB, at most 100,000 tree entries and 2 MiB per selected source file.
- Discovery: ten pages of 100 results; requests: three attempts and five redirects.
- Retry delay: 30 seconds; network attempt: 60 seconds; shared deadline: 20 minutes.
- Child processes: five minutes each (also bounded by the shared deadline),
  with at most 2 MiB captured output per stream.

HTTPS hosts, endpoint shapes, source paths and release asset names are explicitly
allowlisted; credentials, unexpected ports and downgrade redirects are rejected.
The implementation counts streamed bytes rather than trusting Content-Length.
Insufficient evidence, limits and anonymous API rate limiting cause failure,
not a relaxed verification policy. A cold build that exceeds the limit fails;
prebuilding the local CLI can avoid that cost on the next invocation.

Smoke verification accepts only pinned, reviewed workflow source contracts. It
requires the resolver and all three platform jobs to succeed in one run attempt
after the selected immutable release was published. Release-event runs must match
the tag and commit. Push coordinators must match the source commit and
commit-addressed release intent. Dispatch recovery inputs are not observable in
the anonymous run API, so those runs cannot supply evidence; discovery continues
for another qualifying run. Unknown workflow revisions fail closed. Workflow
changes require review and an explicit update of the pinned source digests.
Unavailable historical smoke evidence can prevent verification of an otherwise
valid historical release; use the normative manual checks in that case.

Fixture success does not certify any live release or establish that the owned
routes are deployed. Hosted public smoke remains responsible for native
cross-platform execution. Live read-back and hosted release acceptance remain
separate operator evidence.
