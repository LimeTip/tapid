# Tapid Architecture

Tapid is a domain-oriented modular monolith in one Rust workspace and one public repository. Domain capability modules provide strong internal separation without distributing the contributor experience or adding network boundaries. A crate is useful when it expresses a cohesive capability and dependency direction, not merely because code can be moved into it.

The engineering standard behind this architecture is in [engineering-principles.md](engineering-principles.md). ADR 0002 records the decision.

## Module design

Each domain capability should be a deep module: a small interface hides substantial cohesive behavior, invariants, and failure handling. Implementation stays private. A library crate root exposes a curated interface through deliberate re-exports rather than making its internal module tree public. Callers should not need to understand internal file layout.

Use hexagonal ports only at real I/O seams such as registries, artifact storage, the filesystem, clocks, process execution, and terminal interaction. Pure domain collaboration does not need a port. Do not create traits for hypothetical replacement or turn every function call into an adapter boundary.

Behavior grows as tested vertical slices. A slice starts at an observable interface, crosses only the capabilities it needs, and includes failure behavior. Broad horizontal layers and speculative abstractions are not completion.

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

The workspace contains both implemented crates and intentionally narrow scaffold crates. A crate name or README description does not mean its future capability is implemented. New behavior belongs in a tested vertical slice in the most focused capability.

## `tapid-core`

`tapid-core` is the small, pure domain foundation shared by Tapid capabilities. It owns stable value objects and deterministic validation that remain meaningful across npm-compatible and Tapid-native workflows.

Current examples include:

- `PackageName`, including scoped and unscoped package identity.
- `PackageVersion`, including semantic version validation.
- `ArtifactDigest`, including validated SHA-256 artifact identity.
- `RegistryOrigin`, `PackageInstanceId`, and lossless `PackageIntegrity` for registry-qualified package identity and integrity metadata.
- `PeerContext` and `PlatformContext`, deterministic context primitives used by dependency resolution and lockfile identity.

`tapid-core` must not depend on the CLI, filesystem, network, operating system, registry implementation, process execution, clock, environment, or global mutable state. It must not become a general utility crate or a place to hide ambiguity between focused capabilities.

The detailed inclusion and exclusion rules are in the Tapid project skill reference `references/crate-boundaries.md`.

## Focused responsibilities

- `tapid-cli`: command parsing, terminal and machine output, exit behavior, prompts, and user-facing orchestration.
- `tapid-manifest`: parsing and validation of supported `package.json` fields. It uses `tapid-core` primitives and does not own CLI behavior or project mutation.
- `tapid-lockfile`: lockfile model, versioning, and canonical serialization. It remains independent of terminal presentation and registry transport.
- `tapid-resolver`: deterministic dependency graph resolution and peer-dependency context handling.
- `tapid-registry-client`: registry transport, metadata retrieval, origin routing, and authentication seams. It is distinct from the future Tapid Registry service.
- `tapid-protocol`: shared wire and interface contract types that clients and services can use without depending on the CLI or local store.
- `tapid-archive`: package archive validation, safe paths, extraction rules, and artifact integrity handling.
- `tapid-store`: local verified content-addressed storage, offline reuse, quarantine, leases, and conservative garbage collection. It is not the online Tapid Registry.
- `tapid-linker`: deterministic project materialization, `node_modules` layout, links, junctions, shims, and activation.
- `tapid-policy`: evidence evaluation, approval decisions, stable reason codes, and lifecycle policy. It is pure where possible.
- `tapid-runner`: controlled process and executable invocation, environment handling, output and time limits, and script execution policy enforcement.
- `tapid-publish`: package packing, publication preparation, staging, and promotion workflows.
- `tapid-signatures`: signing and signature verification primitives.
- `tapid-attestations`: artifact-bound provenance and audit attestation models.
- `tapid-transparency`: append-only transparency records and verification foundations.
- `tapid-test-support`: fixtures, fake registries, adversarial inputs, and shared test utilities. Production crates must not depend on it.

## Dependency direction

The CLI composes domain capabilities but does not own their rules:

```text
tapid-cli
  -> manifest, lockfile, resolver, registry-client, archive,
     store, linker, policy, runner, publish

focused capabilities -> tapid-core only for stable domain primitives
protocol, signatures, attestations, and transparency remain independent
of CLI and local filesystem presentation
```

Avoid circular dependencies. If two crates need a type, decide whether it is a genuine core primitive, a protocol contract, or a test fixture before moving it into a shared crate. Shared use alone is not sufficient reason to put a type in `tapid-core`.

## Security state transitions

Security-sensitive behavior is modeled as explicit, validated state transitions. Examples include unverified to verified artifact, staged to active layout, candidate to accepted policy decision, and staged to active or quarantined release. Each transition identifies evidence, authorization, failure state, and allowed recovery. Invalid, stale, interrupted, or unauthorized transitions fail closed. State must not be inferred from a filename, mutable URL, or partially completed side effect.

Tests cover successful transitions and rejection, interruption, replay, and recovery paths. Human and machine-readable output derive from the same transition result.

## Registry seam

The future Tapid Registry is an online system for package metadata, publishing, distribution, identities, verification, and attestations. It is distinct from `tapid-store`, which is local developer-machine storage. Registry transport is a real I/O seam and can have production and deterministic test adapters.

The main repository remains the default home for the client and its libraries. A future registry system or independently operated storage system may use a separate repository when deployment, security, ownership, or release lifecycle justifies that split.

## Architecture decisions and checks

Consequential, cross-cutting, or difficult-to-reverse decisions require an ADR in `docs/adr/`. An ADR records context, decision, alternatives, consequences, and enforcement or follow-up.

Node.js 22.6.0 or later is required. `node --experimental-strip-types tools/check_architecture.ts` scans tracked production Rust files. Tests and top-level generated or build output trees are excluded, while ordinary production modules named `build` or `generated` remain in scope. Eight hundred physical lines is an advisory review trigger, not a pass/fail rule. When a file exceeds it, review cohesion, interface depth, change locality, and navigability before deciding whether splitting improves the module. A cohesive file may remain larger without an exception. The checker separately enforces a 100 physical line entrypoint threshold for `crates/tapid-cli/src/main.rs`; a temporary exception must document any existing migration debt.

When a capability or crate interface changes, update its implementation, focused tests, crate README, relevant ADR, and this architecture document. Do not expand scaffold crates or `tapid-core` horizontally without an exercised user or protocol behavior.
