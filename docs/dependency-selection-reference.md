# Dependency selection reference

This document records the review of the crates.io top-downloads list and the decisions made for Tapid. Download counts are adoption signals, not evidence that a dependency is suitable for Tapid.

## Current recommendation

Do not add new dependencies solely because they appear in a popular crates.io list. Tapid should prefer a small, explicit dependency graph with deterministic behavior, clear security boundaries, and measured need.

### Base64

Tapid currently uses `base64` 0.22.x. The reviewed crates.io list showed `base64` 0.23.1.

Recommendation:

- Evaluate upgrading to 0.23.1 as a routine dependency maintenance change.
- Run the full workspace tests, integration tests, Clippy, `cargo deny check`, `cargo audit`, and packaging checks.
- Review the changelog and encoded output compatibility before committing the upgrade.

This is an upgrade candidate, not a reason to redesign the integrity model.

### thiserror

Tapid has public error enums in several crates, including the core, archive, lockfile, linker, manifest, registry-client, resolver, store, and runner crates. These currently contain manual error plumbing.

`thiserror` is a procedural macro crate used to derive standard Rust error implementations. In practice it can generate implementations for:

- `std::error::Error`
- `Display`
- `From` conversions for source errors
- Consistent source-error chains

It does not provide logging, telemetry, global state, or runtime services. It is a compile-time convenience for defining typed library errors.

Recommendation:

- Consider a dedicated, incremental `thiserror` refactoring wave.
- Migrate one crate at a time.
- Preserve stable user-facing error messages unless a deliberate change is documented.
- Add or retain tests for error classification, source chains, and CLI diagnostics.
- Do not combine the refactoring with unrelated dependency upgrades.

## Future logging evaluation

Logging could be useful for Tapid, especially for diagnosing registry access, resolution decisions, installation stages, rollback behavior, and platform-specific process execution. It should be added only after the operational requirements are clear.

### What the `log` crate does

The `log` crate is a small facade for application and library logging. Code emits records at levels such as:

- `error`: an operation failed or an invariant was violated
- `warn`: an unusual condition was handled or a fallback was used
- `info`: a significant operation started or completed
- `debug`: diagnostic details useful during development or troubleshooting
- `trace`: very detailed execution information

The `log` crate does not normally write logs by itself. An application selects a logger implementation, which receives the records and decides where to send them, such as stderr, a file, or a test collector.

This separation allows libraries to emit structured-level records without deciding how the final application configures output.

### Questions to answer before adding logging

- Should normal CLI output remain stable and separate from diagnostics?
- Should logs be disabled by default or enabled at a selected level?
- Should Tapid support `--verbose`, `--quiet`, or `RUST_LOG` style controls?
- Should logs go to stderr so scripts can continue to consume stdout?
- Which fields are safe to record, especially URLs, package metadata, paths, and environment details?
- Should logs include request IDs, package identities, tree digests, or lockfile decisions?
- How should secrets, authorization headers, local usernames, and customer paths be redacted?
- Does the project need library compatibility with `log`, or would the richer `tracing` ecosystem be more appropriate?
- What is the test contract for logs, including deterministic output and platform differences?

### Provisional logging direction

Do not add `log` yet. First define the diagnostic model and CLI behavior. Then compare:

- `log`, a small conventional facade with many compatible logger implementations
- `tracing`, a richer structured and span-oriented ecosystem, potentially better for install phases, registry requests, and process execution

Any future logging implementation should keep these boundaries:

- user-facing command output remains stable;
- diagnostics go to stderr by default;
- secrets and sensitive paths are redacted;
- registry responses and package contents are treated as untrusted data;
- logging never changes resolution, integrity verification, or activation behavior;
- tests verify that logging cannot cause installation success or failure to change.

## Crates reviewed and intentionally not added

### Data structures and performance

- `hashbrown`: no need to replace deterministic `BTreeMap` usage.
- `indexmap`: use only where insertion order is actual domain semantics. Tapid currently wants canonical ordering.
- `smallvec`: no measured allocation problem justifies the added optimization dependency.
- `itertools`: no current iterator pipeline justifies adding it.

### Randomness and platform primitives

- `rand`, `rand_core`, `rand_chacha`, and `getrandom`: no current requirement. Randomness must not undermine reproducible behavior.
- `libc`, `rustix`, `socket2`, and `mio`: remain implementation details unless a measured platform requirement appears.
- `windows-sys`, `windows-targets`, Windows architecture crates, and `linux-raw-sys`: remain transitive platform dependencies.

### Parsing and text processing

- `regex`, `regex-syntax`, `regex-automata`, and `aho-corasick`: typed parsers and explicit validation are preferred for security-sensitive input.
- `strsim`: no current fuzzy matching requirement.

### Build and macro infrastructure

- `syn`, `quote`, `proc-macro2`, `heck`, `unicode-ident`, `autocfg`, and `cc`: remain transitive unless Tapid starts authoring procedural macros or native build steps.

### Encoding and hashing

- `digest`: Tapid already uses `sha2` and typed digest abstractions directly.
- `bytes`: the current bounded HTTP and archive APIs do not require a new buffer abstraction.

## Review method

The list was compared with Tapid's current direct dependency tree and public architecture. Candidates were evaluated against:

- deterministic resolution and replay;
- security boundaries and fail-closed behavior;
- cross-platform support;
- public API stability;
- dependency and maintenance cost;
- measured product need rather than popularity.
