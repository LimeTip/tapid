# ADR 0008: Offline npm lock import with separate tree evidence

- Status: Accepted
- Date: 2026-10-10

## Context

Issue #151 requires an offline npm v3 import that preserves selected artifacts and later installs them without dependency resolution. Ordinary Tapid schema 7 locks require verified unpacked tree digests. npm locks supply tarball integrity but no Tapid tree digests. Assigning a placeholder digest would falsely claim verification and could permit replay against unrelated store content.

npm also records placement, ancestor peer providers, and platform optional variants that cannot be reconstructed by invoking Tapid's ordinary resolver without changing the selected graph.

## Decision

Use a separate schema 8 imported lock model in `tapid-lockfile`. Persist the validated npm v3 selection graph and root manifest digest, with an initially empty map of verified tree receipts. Reject unknown or unrepresentable entries before writing. Keep schema 7 and its verified-tree validation unchanged.

The lockfile capability uses the resolver capability's existing `Requirement` value object to check selected versions and aliases. It never invokes resolution. Parsing, placement lookup, representability checks, and platform graph selection stay in the lockfile capability. The CLI owns files, credential routing, artifact transport, verification, and install orchestration.

Frozen installation verifies only pinned artifacts and records actual canonical tree digests after checking tarball integrity, safe extraction, and package identity. Each receipt also binds the tree digest to the imported tarball URL and integrity value, so changing either pin cannot reuse the old tree. Imported selections remain in schema 8 so another platform can select its applicable optional entries. Offline installation uses previously recorded digests and store verification. Publication and activation reuse the existing lifecycle journal and store transaction.

Existing nested peer placements become explicit named linker edges and peer contexts. The importer rejects conflicting graphs that collapse to the same Tapid instance, cross-registry peer contexts, and unsupported links/workspaces. This does not expand ordinary online resolver peer placement behavior.

## Consequences

First frozen installation may download artifacts and update verification receipts, while preserving selections. npm is unnecessary after the v3 lock exists. Older Tapid clients reject imported locks. Dependency mutations can deliberately replace the imported lock through ordinary resolution.

The schema preserves npm metadata as migration state rather than claiming that every npm field or runtime behavior is supported. New unknown fields fail closed. Dependency scripts remain disabled, and applicable optional artifact verification failures abort installation. `outdated` support for imported locks is outside this initial install/replay slice.
