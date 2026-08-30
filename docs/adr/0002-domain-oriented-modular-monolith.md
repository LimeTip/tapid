# ADR 0002: Domain-oriented modular monolith

Status: Accepted; source-size enforcement superseded by ADR 0003
Date: 2026-08-30

## Context

Tapid has grown as a Rust workspace with focused crates, but several production files and the CLI entrypoint now contain broad responsibilities. The project needs a consistent rule for module shape, I/O seams, testing, security workflows, and architecture review without turning file length into a language constraint or splitting the product into premature systems.

Security-sensitive package management also requires reviewers to see when untrusted input becomes verified, approved, staged, active, quarantined, or rejected. Implicit state and partial side effects make those guarantees hard to test.

## Decision

Tapid uses a domain-oriented modular monolith. Domain capabilities are deep modules with small interfaces, private implementation, and curated crate-root re-exports. Workspace crates express cohesive capability and dependency direction while the product remains one codebase.

Hexagonal ports are introduced only at real I/O seams, including registry transport, filesystem access, artifact storage, clocks, process execution, and terminal interaction. Pure domain collaboration remains direct. Traits are not added for hypothetical variation.

Production behavior is delivered as vertical slices using strict test-driven development. A focused failing test precedes implementation. Each slice includes relevant failure and attack paths before broader refactoring.

Security-sensitive behavior uses explicit state transitions with named evidence, authorization, failure state, and recovery behavior. Invalid or incomplete transitions fail closed.

Consequential and difficult-to-reverse decisions use ADRs. New ADRs supersede old decisions explicitly rather than editing accepted history.

Eight hundred physical lines in a tracked production Rust file is an architecture review threshold, not an absolute Rust language rule. A file above it must be split or listed with a concrete rationale in the committed `docs/architecture-exceptions.txt` file. The `tapid-cli` `main.rs` entrypoint has a stricter 100 physical line threshold because it should contain only argument dispatch and process exit conversion. Temporary documented exceptions expose current migration debt.

The executable guard is `scripts/check_architecture.py`. It scans Git-tracked production Rust files, excludes tests and generated or build trees, validates exception rationales, and reports violations deterministically.

## Alternatives considered

### Continue with informal crate guidance

Rejected because prose alone does not detect oversized files, entrypoint growth, stale exceptions, or undocumented deviations.

### Enforce 800 lines as an absolute language rule

Rejected because physical length is a review signal, not a reliable measure of cohesion or module depth. Some algorithms or tables may be clearer in one file when the exception is explicit and reviewed.

### Split into independent services or repositories

Rejected for current client work. Distribution would add network, deployment, versioning, and failure complexity without improving the domain model. A future online registry can be separated when its operational and security lifecycle requires it.

### Use ports for every module collaboration

Rejected because speculative traits create shallow interfaces and indirection. Ports belong at actual I/O seams where behavior or environment varies.

## Consequences

- New behavior should move behind focused domain capability interfaces rather than expand CLI orchestration.
- Crate public surfaces become deliberate and internal modules remain private.
- Tests are written before production behavior and follow observable vertical slices.
- Security reviews can reason about explicit transitions and safe failure states.
- Files crossing thresholds require visible rationale and follow-up instead of silent growth.
- Existing exceptions are accepted migration debt and should shrink through tested slices, not broad rewrites.
- The architecture checker becomes a required documentation and script validation command.

## Enforcement

Run:

```text
python3 -m unittest tests.test_check_architecture -v
python3 scripts/check_architecture.py
```

Review changes to `docs/architecture-exceptions.txt` as architecture decisions. An exception must identify an exact tracked production Rust path and explain why cohesion currently outweighs splitting or why migration must be incremental.
