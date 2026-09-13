# ADR 0004: GitHub-native client releases

Status: Accepted
Date: 2026-09-03
Supersedes: ADR 0001 for current client releases

## Context

Tapid is maintained primarily by one person, with occasional secondary review. The previous signed-manifest design introduced a custom trust protocol, embedded keyring, release state machine, and substantial release-specific code. Its operational and review cost was disproportionate to the current project and team.

Tapid uses a small, conventional GitHub and Cargo release workflow suited to its current team size.

## Decision

Client releases use:

- a normal reviewed version-bump pull request;
- an annotated stable version tag whose commit is on `main`;
- a tag-triggered six-target build matrix;
- standard GitHub Actions artifact aggregation;
- `SHA256SUMS` for download integrity;
- a new draft GitHub release created through GitHub's API by the reviewed workflow, followed by exact asset read-back;
- manual inspection and publication of the draft;
- post-publication public installer smoke tests;
- a separate crates.io workflow that requires the matching public release and successful installer smoke run, then uses Trusted Publishing and ordinary Cargo commands.

Small release helpers are written in TypeScript and run with Node.js. Rust remains appropriate for product code and shared product semantics, but Tapid will not build a custom Rust release engine without a demonstrated need.

The bootstrap installers rely on GitHub Releases and HTTPS for authenticity. They verify SHA-256 checksums and narrowly validate archive contents before staging the executable. Checksums from the same release are not described as an independent authenticity control.

Through Tapid 0.0.9, `tapid upgrade` was deferred and users upgraded by rerunning the installer. Publication of independently authenticated stable metadata remains deferred; the GitHub release workflow does not publish signed channel manifests or establish a separate release trust root.

### Amendment for Tapid 0.0.10

The client now supports `tapid upgrade` against the same GitHub release assets and `SHA256SUMS` used by the installers. Existing signed-discovery and keyring code is retained and preferred when usable metadata is available. If all default discovery endpoints are unavailable, the client may use the canonical GitHub Releases API with checksum verification. Explicit custom discovery endpoints do not enable that GitHub fallback.

Received signed-discovery metadata that fails parsing, freshness, target selection, or signature validation must fail closed rather than enable checksum-only fallback or cached recovery. Transport unavailability and verification failure are distinct outcomes. Last-known-good state records verification provenance and a local release floor; it does not establish independent authenticity or protect against hostile local-state modification.

This amendment authorizes a development-release self-update path within the existing GitHub trust boundary, not the independently authenticated provider-neutral release system proposed in ADR 0001. Archive checks and staged executable replacement remain required. Published installer and upgrade smoke evidence must be evaluated separately, including platform scope and same-version replacement versus a previous-version upgrade.

## Consequences

The release path is shorter, easier to review, and uses fewer secrets. GitHub compromise remains inside the client release trust boundary. Public smoke tests detect installation failures only after release publication. Clients older than 0.0.10 still require reinstallation; newer clients can use the bounded self-update path described above. Neither path provides independent release authentication for checksum-only GitHub artifacts.
