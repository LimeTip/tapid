# ADR 0004: GitHub-native client releases

Status: Accepted
Date: 2026-09-03
Supersedes: ADR 0001 for current client releases

## Context

Tapid is maintained primarily by one person, with occasional review from Doug. The previous signed-manifest design introduced a custom trust protocol, embedded keyring, release state machine, and substantial release-specific code. Its operational and review cost was disproportionate to the current project and team.

Tapid uses a small, conventional GitHub and Cargo release workflow suited to its current team size.

## Decision

Client releases use:

- a normal reviewed version-bump pull request;
- an annotated stable version tag whose commit is on `main`;
- a tag-triggered six-target build matrix;
- standard GitHub Actions artifact aggregation;
- `SHA256SUMS` for download integrity;
- a new draft GitHub release created by a maintained release Action, followed by exact asset read-back;
- manual inspection and publication of the draft;
- post-publication public installer smoke tests;
- a separate crates.io workflow that requires the matching public release and successful installer smoke run, then uses Trusted Publishing and ordinary Cargo commands.

Small release helpers are written in TypeScript and run with Node.js. Rust remains appropriate for product code and shared product semantics, but Tapid will not build a custom Rust release engine without a demonstrated need.

The bootstrap installers rely on GitHub Releases and HTTPS for authenticity. They verify SHA-256 checksums and narrowly validate archive contents before staging the executable. Checksums from the same release are not described as an independent authenticity control.

Signed release manifests, the embedded release keyring, provider-neutral stable metadata, and `tapid upgrade` are deferred. They may be reconsidered if Tapid needs independent update authorization, multiple artifact providers, or stronger rollback protection.

## Consequences

The release path is shorter, easier to review, and uses fewer secrets. GitHub compromise remains inside the client release trust boundary. Public smoke tests detect installation failures only after release publication. Users upgrade by rerunning the installer until a deliberately designed self-update mechanism is justified.
