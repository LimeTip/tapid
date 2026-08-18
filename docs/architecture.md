# Tapid Architecture

Tapid starts as a Rust workspace in one public repository. The workspace provides separation between product responsibilities without splitting the contributor experience across multiple repositories.

## Workspace crates

```text
crates/
├── tapid-cli
├── tapid-core
├── tapid-manifest
├── tapid-lockfile
├── tapid-resolver
├── tapid-registry-client
├── tapid-archive
├── tapid-store
├── tapid-linker
├── tapid-policy
├── tapid-runner
├── tapid-publish
├── tapid-protocol
├── tapid-signatures
├── tapid-attestations
├── tapid-transparency
└── tapid-test-support
```

The workspace contains both implemented crates and intentionally narrow scaffold crates. A crate name or README description does not mean that its full future capability is implemented. New behavior should be added as a tested vertical slice in the most focused crate.

## `tapid-core`

`tapid-core` is the pure, small domain foundation shared by Tapid components. It owns stable value objects and deterministic validation that remain meaningful across npm-compatible and Tapid-native workflows.

Current examples include:

- `PackageName`, including scoped and unscoped package identity.
- `PackageVersion`, including semantic version validation.
- `ArtifactDigest`, including validated SHA-256 artifact identity.
- Pure comparison, formatting, and domain errors for those primitives.

`tapid-core` must not depend on the CLI, filesystem, network, operating system, registry implementation, process execution, clock, environment, or global mutable state. It must not become a general utility crate or a place to hide ambiguity between focused boundaries.

The detailed inclusion and exclusion rules are in the Tapid project skill reference `references/crate-boundaries.md`.

## Focused responsibilities

- `tapid-cli`: command parsing, terminal and machine output, exit behavior, prompts, and user-facing orchestration. It currently owns `tapid init` and `tapid manifest validate` filesystem behavior.
- `tapid-manifest`: parsing and validation of supported `package.json` fields. It uses `tapid-core` primitives and does not own CLI behavior or project mutation.
- `tapid-lockfile`: lockfile model, versioning, and canonical serialization. It should remain independent of terminal presentation and registry transport.
- `tapid-resolver`: deterministic dependency graph resolution and peer-dependency context handling.
- `tapid-registry-client`: registry transport, metadata retrieval, origin routing, and authentication boundaries. It is distinct from the future Tapid Registry service.
- `tapid-protocol`: shared wire and API contract types that may be used by clients and services without depending on the CLI or local store.
- `tapid-archive`: package archive validation, safe paths, extraction rules, and artifact integrity handling.
- `tapid-store`: local verified content-addressed storage, offline reuse, quarantine, leases, and conservative garbage collection. It is not the online Tapid Registry.
- `tapid-linker`: deterministic project materialization, `node_modules` layout, links, junctions, shims, and activation.
- `tapid-policy`: evidence evaluation, approval decisions, stable reason codes, and lifecycle policy. It should be pure where possible.
- `tapid-runner`: controlled process and executable invocation, environment handling, output and time limits, and script execution policy enforcement.
- `tapid-publish`: package packing, publication preparation, staging, and promotion workflows.
- `tapid-signatures`: signing and signature verification primitives.
- `tapid-attestations`: artifact-bound provenance and audit attestation models.
- `tapid-transparency`: append-only transparency records and verification foundations.
- `tapid-test-support`: fixtures, fake registries, adversarial inputs, and shared test utilities. Production crates should not depend on it.

## Dependency direction

The initial implemented direction is:

```text
tapid-cli      -> tapid-core, tapid-manifest, tapid-store
tapid-manifest -> tapid-core
tapid-store    -> tapid-core
```

As capabilities are implemented, the CLI may orchestrate focused crates:

```text
tapid-cli
  -> manifest, lockfile, resolver, registry-client, archive,
     store, linker, policy, runner, publish

focused crates -> tapid-core where they need stable domain primitives
protocol, signatures, attestations, and transparency remain independent
of the CLI and local filesystem presentation
```

Avoid circular dependencies. If two crates need a type, decide whether it is a genuine core primitive, a protocol contract, or a test fixture before moving it into a shared crate. Shared use alone is not sufficient reason to put a type in `tapid-core`.

## Registry boundary

The future Tapid Registry is an online service for package metadata, publishing, distribution, identities, verification, and attestations. It is distinct from `tapid-store`, which is local developer-machine storage.

The main repository remains the default home for the client and its libraries. A future registry service or independently operated storage system may use a separate repository when its deployment, security, ownership, or release lifecycle justifies that split.

## Boundary change discipline

When a crate boundary changes, update the implementation, focused tests, crate README, this architecture document, and the Tapid project skill references. Do not expand scaffold crates or `tapid-core` horizontally without an exercised user or protocol behavior.
