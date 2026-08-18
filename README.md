# Tapid

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)

Tapid is a planned package manager, package runner, and registry ecosystem for JavaScript and TypeScript, written in Rust.

The project starts with npm compatibility so it can be useful before a native package ecosystem exists. Its intended destination is broader: a Tapid-native package format, public and private registries, safer one-shot execution, explainable package risk evidence, easier publishing, controlled release revocation, and independent package audits.

> Status: early implementation. Tapid can initialize a private `package.json` project and validate its minimal manifest, but it is not yet ready for package installation or production use.

## Why Tapid

Package installation and one-shot execution currently require users and automated agents to trust more than they can easily inspect. A normal package prompt often does not explain:

- Who published the selected release.
- What changed from the previous release.
- Whether the artifact matches its source.
- Whether it contains install scripts, native code, or obfuscated code.
- What filesystem, network, environment, or subprocess capabilities it may use.
- Whether a maintainer or publisher identity recently changed.
- Why the package manager decided to allow, warn, prompt, or block.

Tapid is intended to make those facts visible before installation or execution, while retaining compatibility with real Node.js projects.

## Product direction

Tapid has two delivery stages.

### 1. npm-compatible client

The first useful version will focus on:

- Existing `package.json` projects.
- Deterministic `tapid init` project initialization.
- Strict validation of required package metadata.
- npm registry metadata and package artifacts.
- Deterministic dependency resolution.
- A versioned `tapid.lock` file.
- A verified content-addressed global store.
- Node-compatible `node_modules` output.
- Lifecycle scripts disabled until explicitly approved.
- Safe project updates, pruning, and cache garbage collection.
- A policy-aware `tapid x` command for one-shot package execution.
- Human-readable and machine-readable risk decisions.
- Explicit private registry routing without silent public fallback.

### 2. Tapid-native ecosystem

The long-term product includes:

- A Tapid-native package and publishing protocol.
- Public and private registries.
- Immutable artifact storage and CDN delivery.
- Scoped package identities and verified organizations.
- Preview-first, two-stage publishing.
- Deprecation, yank, quarantine, hard-revocation, and tombstone states.
- Publisher, source, provenance, and release-diff evidence.
- Declared, inferred, observed, and enforced capability labels.
- Automated and human audit attestations bound to artifact digests.
- Typosquatting detection, namespace governance, disputes, and appeals.
- Package discovery pages and ecosystem APIs.
- Safe, deterministic behavior for CI and autonomous agents.

These are committed long-term product capabilities, although their implementation will be phased.

## Design principles

### Evidence before trust

Tapid should show factual evidence separately from policy decisions. It should not compress package safety into an unexplained universal score.

### Exact and reproducible

Every installed package should resolve to an exact registry identity, package identity, version, and artifact digest. Frozen lockfile installation should be deterministic and work offline when required artifacts and valid metadata are cached.

### Secure execution by default

Dependency lifecycle scripts should not run silently. Approvals should bind to the artifact digest and normalized script hash. Non-interactive operation should fail closed when policy requires human approval.

### Registry boundaries are trust boundaries

Private scopes should route to explicitly configured registries. Credentials must not cross origins, and a private package must not silently fall back to a public package with the same name.

### Correction without destroying reproducibility

A mistaken release can be yanked from new resolution while its immutable artifact remains available to existing exact lockfiles. Dangerous releases can be quarantined or hard-revoked. A package version can never be republished with different bytes.

### Safe cleanup

Updates and removals should prune obsolete package instances only from Tapid-managed project paths. Shared cache content should be garbage-collected separately using reference checks, leases, grace periods, and dry-run previews.

## Planned command surface

The exact interface may evolve, but the intended CLI includes:

```text
tapid init
tapid install
tapid add <package-spec>
tapid remove <package>
tapid update [package]
tapid prune --dry-run
tapid why <package>
tapid info <package-spec>
tapid run <script>
tapid approve-build <package-spec>
tapid audit
tapid x <package-spec> -- <args>
tapid lock verify
tapid store verify
tapid store gc --dry-run
tapid pack --dry-run
tapid publish --stage
tapid publish --promote <release-id>
tapid deprecate <package>@<version>
tapid yank <package>@<version>
```

## Proposed architecture

Tapid will use a Rust workspace with separate components for:

- CLI and terminal or JSON output.
- Manifest and package-spec parsing.
- Lockfile parsing and canonical serialization.
- Dependency resolution and peer contexts.
- Registry clients and registry routing.
- Archive validation.
- Content-addressed storage.
- `node_modules` linking and executable shims.
- Policy evaluation and script approval.
- Package execution.
- Native package publishing.
- Test registries, fixtures, and adversarial test support.

The future registry platform is expected to use a versioned API, PostgreSQL, S3-compatible immutable object storage, CDN delivery, signed metadata, isolated verification workers, and append-only release events.

## Release lifecycle

Tapid distinguishes package availability from artifact existence:

- **Staged:** Validated but not available for resolution.
- **Active:** Available for new resolution.
- **Deprecated:** Available with a migration warning.
- **Yanked:** Excluded from new range resolution, but available to existing exact locks.
- **Quarantined:** Blocked by default during a security, legal, or integrity investigation.
- **Hard-revoked:** Known-dangerous or invalid and blocked from installation and execution.
- **Tombstoned:** Identity retained, but ordinary artifact access removed for an exceptional legal or privacy reason.

## Roadmap

1. Rust workspace, CLI, documentation, and continuous integration.
2. Manifest parsing and deterministic lockfile.
3. npm-compatible registry client and resolver.
4. Verified content-addressed store and Node-compatible linker.
5. Install, update, remove, prune, and offline workflows.
6. Lifecycle-script policy and safe `tapid x` execution.
7. Workspaces and broader npm compatibility.
8. Tapid-native package and registry protocol.
9. Self-hosted private registry.
10. Verification, provenance, and audit attestations.
11. Hosted private registry and public registry closed beta.
12. Public discovery, namespace governance, and operational readiness.
13. Paid audit providers and broader ecosystem integrations.

## Development

The repository contains the initial Rust workspace under `crates/`. The `tapid` CLI currently provides help and version commands, while `tapid-core` and `tapid-store` contain the first tested domain and local-store foundations. Package installation and registry access are not implemented yet.

The intended quality gates are:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
cargo audit
```

Cross-platform behavior should be tested on macOS, Linux, and Windows from the beginning.

## Security posture

Tapid will process untrusted registry metadata, archives, package scripts, and executable code. Security-sensitive code should be designed around:

- Streamed integrity verification.
- Strict archive path validation.
- Atomic filesystem operations.
- Read-only verified package trees.
- Exact-origin credential handling.
- Signed metadata freshness and rollback protection.
- Fail-closed unattended policy.
- Adversarial tests and fuzzing.
- Explicit limitations where operating-system sandbox enforcement differs.

A security reporting process and `SECURITY.md` will be added before any public release.

## Project documentation

Detailed implementation and future ecosystem plans are maintained outside version control. Stable architecture decisions and specifications belong in repository-owned `docs/`, `schemas/`, and `openapi/` directories.

## Contributing

Tapid is intended to benefit from community participation. People are welcome to inspect the project, propose improvements, open issues, submit pull requests, and continue building on the code under the MIT License.

The MIT License permits commercial use, commercial distribution, resale, modification, and the creation of competing or complementary products. This is intentional: Tapid should be easy to adopt and improve, and contributors should not be afraid that the licensing model prevents useful work.

Submitting a contribution does not automatically transfer copyright ownership to LimeTip. Contributors must have the right to submit their work and grant the project the rights required to use, modify, distribute, and maintain the contribution under the MIT License. LimeTip may introduce a separate Contributor License Agreement before accepting contributions where additional licensing rights are required.

## Ownership, license, and trademarks

Tapid is developed and maintained by LimeTip Company. The Tapid name, logo, brand assets, domains, and LimeTip trademarks are separate from the copyright license for the source code.

The source code is licensed under the MIT License. The MIT License permits use, modification, distribution, and resale of the code, but it does not grant permission to imply endorsement by LimeTip, use LimeTip trademarks, or present a modified project as the official Tapid project.

Forks and derivative products should use their own names, logos, and domains, and should clearly identify their relationship to the original Tapid project. LimeTip retains the right to protect the Tapid and LimeTip names and marks under applicable trademark law.

See the `LICENSE` file for the complete license text. LimeTip may offer separate commercial services, hosted registries, verification services, and support around Tapid.
