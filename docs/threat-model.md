# Threat model

## Scope and trust boundaries

Tapid treats manifests, registry metadata, package archives, filesystem trees, lockfiles, package executable metadata, scripts, policy evidence, and trust envelopes as untrusted inputs at separate boundaries. Inputs are typed or validated before the next contract consumes them. Runtime-derived temporary roots are used by integration fixtures so repository paths and ambient state are not trusted accidentally.

## Implemented defenses

- **Manifest and registry input:** package names, versions, registry origins, URLs, dependency keys, and integrity values are validated. npm and JSR identities remain registry-qualified.
- **Resolution:** candidate selection is deterministic and scoped to registry identity. Unsupported range syntax fails instead of being silently broadened.
- **Archive boundary:** traversal, absolute, drive, UNC, duplicate, case-collision, escaping symlink, special-file, and resource-limit checks reject hostile entry metadata before extraction consumers use them.
- **Artifact and tree storage:** bytes are streamed into private staging, hashed, synced, checked against the expected digest, and activated atomically. Existing digest paths are authoritative. Tree replay requires an exact regular `.tapid-tree` marker.
- **Lockfile replay:** the CLI checks the root manifest digest, exact package identity, tree digest, and store availability before staging managed output. Failed staging does not intentionally activate partial `node_modules`.
- **Link and executable planning:** paths remain under an absolute managed root. Bin targets must be regular files inside verified trees. Traversal, absolute paths, symlinks, special files, collisions, and unsupported platforms fail closed. Install never runs dependency lifecycle scripts.
- **Root script target contract:** ADR 0005 requires `tapid run` containment to be default-on and fail-closed. Checked-in defaults and per-script permissions may grant only project-relative reads and writes, a boolean network capability, named environment variables, subprocess use, and bounded resources. The runtime constructs a minimal environment and controlled `PATH`; unlisted caller variables, credentials, proxy settings, and agent sockets are not inherited. Arguments after `--` are forwarded in order without being interpreted as Tapid options. These are accepted requirements, not implemented defenses, until the parser, platform backend, CLI wiring, and exact-commit runtime probes are integrated.
- **Trust artifacts:** `tapid-signatures` provides Ed25519 primitives for package attestations and future trust protocols. These primitives are not part of current client release authorization.
- **Stable release installation:** Installers select immutable versioned GitHub release assets over HTTPS, verify the selected archive against `SHA256SUMS`, require one expected regular executable, extract into a temporary directory, and stage the destination before replacement.
- **Release publication:** An annotated tag triggers six native builds and a draft GitHub release. Human publication is the promotion boundary. Public installation smoke tests run after publication. crates.io publication uses OIDC Trusted Publishing.

## Non-goals and residual risks

- GitHub, HTTPS, release archives, and `SHA256SUMS` share one trust boundary. A repository or workflow compromise can replace an archive and its checksum. Tapid does not claim independent release authentication.
- `tapid upgrade` is unavailable. Reinstallation is the supported upgrade path until an authenticated self-update design is justified.
- Root scripts remain arbitrary project code. Containment reduces ambient authority; it does not establish that code is benign, prevent abuse of granted project writes or network access, or replace malware and executable analysis.
- The current pre-ADR execution path must be treated as uncontained until the ADR 0005 parser, backend, supervision, limits, receipts, and CLI wiring are integrated. The accepted contract does not justify claiming a verified sandbox today, but the prior behavior is not the permanent design.
- macOS 26 is unsupported because Seatbelt does not provide complete descendant lifecycle ownership, process groups are escapable, and public lineage scanning retains a rapid double-fork race. The recursive Endpoint Security descendants client is introduced in macOS 27 and remains entitlement-gated and unverified. Linux Landlock plus network/process controls and Windows AppContainer plus a non-breakaway Job Object are proposed backends only. Neither platform is supported for this contract without positive and negative runtime evidence from the exact integrated commit.
- Archive validation does not decompress or inspect behavior. A validated archive is not necessarily safe software.
- No provenance verification or transparency-log verification is implemented for release artifacts.
- Registry TLS and server authenticity depend on the transport and operating system. Private-registry authentication, mirrors, cache eviction, and concurrency leases remain outside this slice. Retries improve isolated transient-failure tolerance but do not provide failover or availability guarantees.
- JSR live integrity is unsupported and unverified. Tapid fails closed when JSR metadata lacks an explicit HTTPS npm tarball and valid SHA-512 SRI. It does not treat downloaded bytes or a constructed URL as registry-declared integrity.
- Existing Linux and Windows consumer validation covers the pre-ADR runner path only. It is not containment evidence. Local macOS tests must not be generalized to either platform.

The local and integration tests demonstrate regression coverage for implemented guarantees only. They do not solve the non-goals above or establish production readiness.
