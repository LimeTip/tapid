# Protected release tooling

This directory contains the deterministic helpers used by Tapid's protected release workflows. Operator procedures live in:

- [`docs/releasing.md`](../../docs/releasing.md) for the end-to-end release, recovery, public smoke, and website gates;
- [`docs/crates-io-releasing.md`](../../docs/crates-io-releasing.md) for crates.io planning, Trusted Publishing, and partial-failure recovery;
- [`docs/security/release-key-management.md`](../../docs/security/release-key-management.md) for stable signing-key custody and rotation.

GitHub binary publication and crates.io publication are separate channels. They use separate plans, workflows, protected environments, credentials, approvals, and verification evidence.

## Guarded interfaces

`scripts/release.py` exposes separate operations:

- `plan --version VERSION --commit FULL_SHA [--output PATH]` gathers read-only repository and GitHub state and emits a canonical `tapid-release-plan-v1` plus its digest;
- `tag --plan PATH --expect-digest DIGEST` creates or safely verifies an annotated tag only after revalidating the reviewed plan;
- `dispatch --plan PATH --expect-digest DIGEST` revalidates the plan and dispatches binary publication with all derived inputs.

Planning is the default safe mode. Tag creation and dispatch are never combined implicitly.

`scripts/crates_release.py plan --commit FULL_SHA --output PATH` emits a canonical `tapid-crates-publication-plan-v1`. It discovers the publishable workspace graph, registry state, dependent requirement changes, package archive digests, and deterministic publication order without reading a publishing credential or changing crates.io.

`.github/workflows/crates-publication.yml` recomputes the reviewed crates plan and publishes only approved entries behind `crates-io-release`. It uses official crates.io Trusted Publishing through OIDC. No long-lived crates.io token is stored by default.

`.github/workflows/release-public-smoke.yml` performs unauthenticated read-back of a public stable or explicit-tag release. It verifies GitHub assets, signed metadata, native archives, public installers, consumer installation, and frozen replay without publication permission. The post-promotion call from `release-publication.yml` uses explicit tag mode. A separate stable-mode run is required to verify stable-channel resolution.

The workflow currently invokes `public_release.py` with `--skip-website` in both modes, so its `tapid-public-release-verification-v1` artifact is not website evidence. Release completion separately requires a website-enabled stable invocation of `public_release.py` and a report with `website.status` equal to `verified`. Adding that mandatory website check to protected automation remains an acceptance requirement.

## Binary publication contract

`generate_manifest.py` is the single manifest construction path. It reads each artifact once, records exact byte size and lowercase SHA-256, binds version, tag, full commit, targets, and freshness, and signs the canonical unsigned manifest context with Ed25519. The private key is read only from `RELEASE_SIGNING_KEY`; it is never a repository file. Missing keys, key IDs, artifacts, invalid versions, non-HTTPS URLs, and unsigned output fail closed.

The release workflow verifies the generated manifest with `.github/release/verifier`, whose dependency is the workspace's `tapid-signatures` crate. It calls `tapid_signatures::release::verify`, so verification uses the exact RFC 8785 JCS bytes and signature context used by production clients. It also binds the manifest version, tag, and full commit to workflow inputs before publication.

The protected workflow builds six native archives:

- macOS x86_64;
- macOS aarch64;
- Linux x86_64;
- Linux aarch64;
- Windows x86_64;
- Windows aarch64.

All platforms use `.tar.gz`; ZIP extraction is not a client release contract. The complete GitHub release has exactly eight assets: the six native archives, `release-manifest.json`, and `stable.json`.

The `stable-release` environment protects manifest signing and stable advancement. The `TAPID_RELEASE_ED25519_PRIVATE_KEY` secret is exposed only after approval. Before public promotion, the workflow verifies the exact draft asset set, manifest identity, archive contract, and freshness. A rerun may replace expected assets only while the same exact release remains a draft. It refuses to overwrite a public release.

`stable_channel.py` publishes only ordered provider-neutral discovery metadata. It does not sign or establish trust in a manifest. The signed `release-manifest.json` remains separately required.

## Crates.io publication contract

The crates plan records the exact source commit, root lockfile digest, publishable workspace graph, observed crates.io versions, required dependent updates, deterministic topological order, package archive digests, and preflight results. Version bumping remains an explicit source-preparation task.

The protected workflow checks registry state before each mutation. An existing exact version is accepted only when package identity and checksum match. Each new publication is read back before its dependents may proceed. The uploaded progress file records a partial run but is not restored as execution state. On rerun, registry read-back must prove that prior exact versions and checksums form a dependency-order prefix of the original reviewed plan. The executor revalidates and skips that prefix before publishing the first unverified crate.

The `crates-io-release` environment is separate from `stable-release`. Only its publish job receives `id-token: write`, after approval, and obtains a short-lived registry credential with `rust-lang/crates-io-auth-action@v1`. A long-lived token is an exceptional, separately approved fallback, not the default.

## Release sequence limitation

This tooling does not add `release_sequence`. The accepted schema at `schemas/tapid-release-manifest-v1.json` and current release client do not expose that field. Publication binds the immutable commit, version, tag, freshness, and artifact metadata supported by the existing contract. Do not treat it as sequence-based rollback prevention.

## Tests

Run release tooling and workflow checks without repository or fixture keys:

```bash
python3 -m unittest discover -s .github/release -p 'test_*.py' -v
python3 tests/test_installer_scripts.py
actionlint .github/workflows/*.yml
git diff --check
```

Tests generate ephemeral Ed25519 keys and use temporary artifacts. Release tests must not require repository secrets.
