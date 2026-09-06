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
- **Root script preflight:** ADR 0005 CLI wiring is implemented. `tapid run` requires an exact checked-in script profile, bounds `package.json` and `tapid.toml`, constructs an allowlist-only environment and controlled `PATH`, preserves forwarded arguments, and fails before spawn unless a backend proves every requested restriction. These are implemented input and fail-closed defenses, not evidence of native containment; no backend is currently supported.
- **Trust artifacts:** `tapid-signatures` provides Ed25519 primitives for package attestations and future trust protocols. These primitives are not part of current client release authorization.
- **Stable release installation:** Installers select immutable versioned GitHub release assets over HTTPS, verify the selected archive against `SHA256SUMS`, require one expected regular executable, extract into a temporary directory, and stage the destination before replacement.
- **Release publication:** An annotated tag triggers six native builds and a draft GitHub release. Human publication is the promotion boundary. Public installation smoke tests run after publication. crates.io publication uses OIDC Trusted Publishing.

## Non-goals and residual risks

- GitHub, HTTPS, release archives, and `SHA256SUMS` share one trust boundary. A repository or workflow compromise can replace an archive and its checksum. Tapid does not claim independent release authentication.
- `tapid upgrade` is unavailable. Reinstallation is the supported upgrade path until an authenticated self-update design is justified.
- Root scripts remain arbitrary project code. Containment reduces ambient authority; it does not establish that code is benign, prevent abuse of granted project writes or network access, or replace malware and executable analysis.
- The integrated CLI reaches ADR 0005 preflight and fails before spawn because no backend is supported. This prevents an uncontained fallback but does not provide a usable sandbox or prove filesystem, network, descendant, or resource enforcement.
- macOS 26 is unsupported because Seatbelt does not provide complete descendant lifecycle ownership, process groups are escapable, and public lineage scanning retains a rapid double-fork race. The recursive Endpoint Security descendants client is introduced in macOS 27 and remains entitlement-gated and unverified. Linux Landlock plus network/process controls and Windows AppContainer plus a non-breakaway Job Object are proposed backends only. Neither platform is supported for this contract without positive and negative runtime evidence from the exact integrated commit.
- Archive validation does not decompress or inspect behavior. A validated archive is not necessarily safe software.
- No provenance verification or transparency-log verification is implemented for release artifacts.
- Registry TLS and server authenticity depend on the transport and operating system. Private-registry authentication, mirrors, cache eviction, and concurrency leases remain outside this slice. Retries improve isolated transient-failure tolerance but do not provide failover or availability guarantees.
- JSR live integrity is unsupported and unverified. Tapid fails closed when JSR metadata lacks an explicit HTTPS npm tarball and valid SHA-512 SRI. It does not treat downloaded bytes or a constructed URL as registry-declared integrity.
- Existing Linux and Windows consumer validation is package-manager and fail-closed preflight evidence only. It is not native containment evidence. Local macOS tests must not be generalized to either platform.

The local and integration tests demonstrate regression coverage for implemented guarantees only. They do not solve the non-goals above or establish production readiness.
