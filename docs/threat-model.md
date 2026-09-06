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
- **Root script preflight:** ADR 0005 CLI wiring is implemented. `tapid run` requires an exact checked-in script profile, bounds `package.json` and `tapid.toml`, constructs an allowlist-only environment and controlled `PATH`, preserves forwarded arguments, and fails before spawn unless a backend proves every requested restriction. These are implemented input and fail-closed defenses, not evidence of native Restricted or ManagedTree containment; no backend is currently supported.
- **Trust artifacts:** `tapid-signatures` provides Ed25519 primitives for package attestations and future trust protocols. These primitives are not part of current client release authorization.
- **Stable release installation:** Installers select immutable versioned GitHub release assets over HTTPS, verify the selected archive against `SHA256SUMS`, require one expected regular executable, extract into a temporary directory, and stage the destination before replacement.
- **Release publication:** An annotated tag triggers six native builds and a draft GitHub release. Human publication is the promotion boundary. Public installation smoke tests run after publication. crates.io publication uses OIDC Trusted Publishing.

## Non-goals and residual risks

- GitHub, HTTPS, release archives, and `SHA256SUMS` share one trust boundary. A repository or workflow compromise can replace an archive and its checksum. Tapid does not claim independent release authentication.
- `tapid upgrade` is unavailable. Reinstallation is the supported upgrade path until an authenticated self-update design is justified.
- Root scripts remain arbitrary project code. Containment reduces ambient authority; it does not establish that code is benign, prevent abuse of granted project writes or network access, or replace malware and executable analysis.
- The integrated CLI reaches ADR 0005 preflight and fails before spawn because no backend is supported. This prevents an uncontained fallback but does not provide a usable sandbox or prove filesystem, network, descendant, cleanup, or resource enforcement. The planned `--no-sandbox` trusted-interactive escape is also not implemented.
- Restricted reduces ambient authority but does not claim complete descendant ownership. It provides no cleanup guarantee; a backend may separately report best-effort cleanup that it actually attempted or observed. Detached or rapidly re-parented descendants may create lifecycle uncertainty even when native authority restrictions continue to propagate. ManagedTree closes that gap only with a race-free kernel- or VM-owned boundary and complete tree-wide limits and cleanup.
- `network = true` currently grants unrestricted networking. Script arguments such as `--hostname 127.0.0.1` and `--port 3001` do not constrain listen/connect authority. Brokered or declared endpoint scopes remain future schema and backend work.
- The planned macOS 26 Restricted backend uses experimental Seatbelt through deprecated, path-based `sandbox-exec`; it is not implemented or validated, and native ManagedTree remains unsupported because public lineage tracking cannot close the rapid double-fork race. A strict Linux VM through Virtualization.framework would be a separate backend with different platform semantics. Linux Landlock/`no_new_privs`/seccomp and Windows AppContainer or LPAC plus Job Object are proposed backends only. No platform or assurance level is supported without positive and negative runtime evidence from the exact integrated commit.
- Checked launch receipts require the exact requested enforcement and must state each mechanism, assurance level, scope, and limitation. Canonical pathname revalidation is not native-object identity, and observation is not enforcement. Completion evidence describes lifecycle and cleanup only; it cannot re-confirm launch authority, and best-effort cleanup is not ManagedTree ownership.
- Archive validation does not decompress or inspect behavior. A validated archive is not necessarily safe software.
- No provenance verification or transparency-log verification is implemented for release artifacts.
- Registry TLS and server authenticity depend on the transport and operating system. Private-registry authentication, mirrors, cache eviction, and concurrency leases remain outside this slice. Retries improve isolated transient-failure tolerance but do not provide failover or availability guarantees.
- JSR live integrity is unsupported and unverified. Tapid fails closed when JSR metadata lacks an explicit HTTPS npm tarball and valid SHA-512 SRI. It does not treat downloaded bytes or a constructed URL as registry-declared integrity.
- Existing Linux and Windows consumer validation is package-manager and fail-closed preflight evidence only. It is not native containment evidence. Local macOS tests must not be generalized to either platform.

The local and integration tests demonstrate regression coverage for implemented guarantees only. They do not solve the non-goals above or establish production readiness.
