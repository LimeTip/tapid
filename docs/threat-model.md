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
- **Root script execution:** `tapid run` is explicit, uses the project directory, prepends only the managed `.bin` directory, forwards arguments, and returns the child status. This is a compatibility boundary, not containment.
- **Trust artifacts:** `tapid-signatures` provides Ed25519 primitives for package attestations and future trust protocols. These primitives are not part of current client release authorization.
- **Stable release installation:** Installers select immutable versioned GitHub release assets over HTTPS, verify the selected archive against `SHA256SUMS`, require one expected regular executable, extract into a temporary directory, and stage the destination before replacement.
- **Release publication:** An annotated tag triggers six native builds and a draft GitHub release. Human publication is the promotion boundary. Public installation smoke tests run after publication. crates.io publication uses OIDC Trusted Publishing.

## Non-goals and residual risks

- GitHub, HTTPS, release archives, and `SHA256SUMS` share one trust boundary. A repository or workflow compromise can replace an archive and its checksum. Tapid does not claim independent release authentication.
- `tapid upgrade` is unavailable. Reinstallation is the supported upgrade path until an authenticated self-update design is justified.
- Root scripts can execute arbitrary project code through `/bin/sh` on Unix or `cmd.exe` on Windows. Tapid provides no OS sandbox, capability enforcement, malware scanner, executable scanner, or process containment.
- Archive validation does not decompress or inspect behavior. A validated archive is not necessarily safe software.
- No provenance verification or transparency-log verification is implemented for release artifacts.
- Registry TLS and server authenticity depend on the transport and operating system. Private-registry authentication, mirrors, cache eviction, and concurrency leases remain outside this slice. Retries improve isolated transient-failure tolerance but do not provide failover or availability guarantees.
- JSR live integrity is unsupported and unverified. Tapid fails closed when JSR metadata lacks an explicit HTTPS npm tarball and valid SHA-512 SRI. It does not treat downloaded bytes or a constructed URL as registry-declared integrity.
- Linux and Windows consumer validation is configured in GitHub Actions but is not evidence until those jobs execute. Local macOS tests must not be generalized to other platforms.

The local and integration tests demonstrate regression coverage for implemented guarantees only. They do not solve the non-goals above or establish production readiness.
