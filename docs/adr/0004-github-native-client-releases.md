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

Tapid 0.0.10 supports `tapid upgrade` against the same GitHub release assets and `SHA256SUMS` used by the installers. Existing signed-discovery and keyring code is retained and preferred when usable metadata is available. If all default discovery endpoints are unavailable, the client may use the canonical GitHub Releases API with checksum verification. Explicit custom discovery endpoints do not enable that GitHub fallback.

Received signed-discovery metadata that fails parsing, freshness, target selection, or signature validation must fail closed rather than enable checksum-only fallback or cached recovery. Transport unavailability and verification failure are distinct outcomes. Last-known-good state records verification provenance and a local release floor; it does not establish independent authenticity or protect against hostile local-state modification.

This amendment authorizes a development-release self-update path within the existing GitHub trust boundary, not the independently authenticated provider-neutral release system proposed in ADR 0001. Archive checks and staged executable replacement remain required. Published installer and upgrade smoke evidence must be evaluated separately, including platform scope and same-version replacement versus a previous-version upgrade.

### Amendment for the next release, expected 0.0.11

Tapid owns its public discovery address while GitHub remains the initial release host. The installers and default `tapid upgrade` read a small [release record](../release-record-v1.md) through `tapid.dev/releases/v1/latest.tsv`. Versioned records use `tapid.dev/releases/v1/vVERSION.tsv`. These routes redirect to provider-hosted assets; future provider changes do not require another client discovery protocol.

The existing release workflow generates the record from the six archives and uploads it beside `SHA256SUMS` in the same draft. Publishing that draft exposes all eight assets together. No website edit or signing operation is needed for an ordinary release. The record uses TSV so the POSIX installer needs no JSON interpreter.

This keeps provider migration separate from independent release authentication. We retain HTTPS and checksum integrity without restarting the retired key-management and periodic re-signing system. The record contains immutable archive URLs, sizes, and hashes, but no signature or expiry. A domain or release-provider compromise remains within the trust boundary.

The default client no longer probes unpublished signed endpoints or silently changes discovery provider on failure. Received invalid records and mismatched downloads fail. Transport unavailability can use last-known-good recovery, with output stating that the latest release could not be checked. Identical verified executable bytes produce an already-up-to-date result without replacement. Explicit `--endpoint` retains the historical signed protocol; `--release-url` and `TAPID_RELEASE_RECORD_URL` configure the new record protocol.

This change requires a coordinated first rollout. Keep `/stable.json` unchanged for released clients, publish the first record-bearing release, and deploy the new website routes and installer copies at the controlled cutover. Tapid 0.0.10 can reach the new binary through its existing GitHub fallback. Explicit installation of versions through 0.0.10 retains their historical checksum path. The [runbook](../release-distribution.md) requires public website installation, previous-version upgrade, and repeat-upgrade evidence. Checked-in implementation is not evidence of deployment or publication.

## Consequences

The release path uses one draft promotion for binaries and discovery data, with no new signing secret. The owned-domain routes preserve future provider choice. Public smoke tests detect installation failures only after release publication. Clients older than 0.0.10 still require reinstallation; newer clients can use the bounded self-update paths described above. Neither checksum path provides independent release authentication.
