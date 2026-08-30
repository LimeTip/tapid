# Tapid engineering principles

These principles define how Tapid turns product and security requirements into maintainable Rust. They apply to new work and guide incremental improvement of existing code.

## 1. Organize around domain capability

Tapid is a modular monolith. A capability owns its domain rules, invariants, errors, and tests. Workspace crates are strong module seams inside one deployable and reviewable codebase, not distributed systems by default.

Prefer capability names such as resolver, lockfile, archive, store, linker, policy, and runner over generic technical buckets. Keep shared code small. Reuse alone does not make a type a core domain primitive.

## 2. Make modules deep

A deep module gives callers substantial coherent behavior through a small interface. Hide parsing details, algorithms, storage layouts, and recovery machinery behind that interface. A shallow pass-through adds vocabulary without leverage and should usually be removed or absorbed.

Implementation modules are private. Crate roots deliberately re-export the types and operations callers need. Internal file layout is not a public contract.

## 3. Put ports at real I/O seams

Use hexagonal ports where Tapid crosses an external or nondeterministic seam: registry transport, filesystem, artifact storage, clocks, process execution, terminal interaction, and similar I/O. Production adapters and deterministic test adapters can meet the same port.

Do not add a port merely because a collaborator is in another module. Pure domain functions and stable in-process collaboration should remain direct. A hypothetical adapter is not enough reason for a trait.

## 4. Deliver vertical slices with strict TDD

Every production behavior starts with a focused failing test. Implement the smallest end-to-end slice that makes it pass, then refactor while green. Tests exercise module interfaces and real adapters where practical rather than private implementation structure.

Each slice includes relevant failure and attack paths. Horizontal scaffolding, unused interfaces, and unexercised layers are not finished architecture.

## 5. Model security as state transitions

Security-sensitive work moves between explicit states. Verification, policy approval, staging, activation, quarantine, revocation, and recovery require named preconditions and evidence. A transition either completes atomically or leaves a known safe state.

Fail closed on missing evidence, invalid signatures, stale state, interruption, replay, path escape, origin confusion, or unsupported recovery. Human and machine-readable explanations derive from the same result and stable reason codes.

## 6. Record consequential decisions

Use an architecture decision record for choices that are cross-cutting, security-sensitive, expensive to reverse, or likely to be questioned later. Record context, decision, alternatives, consequences, and enforcement. Supersede prior records explicitly rather than rewriting history.

## 7. Treat source size as a review signal

A tracked production Rust file above eight hundred physical lines triggers architecture review. This is not an absolute Rust language rule and does not prove poor design. A cohesive file can exceed the threshold only when `docs/architecture-exceptions.txt` names it and gives a concrete rationale.

The CLI entrypoint has a stricter 100 physical line threshold because entrypoint-only code should dispatch arguments and convert process exits. Existing exceptions expose migration debt. They do not justify adding unrelated behavior.

Run `python3 scripts/check_architecture.py` locally and in relevant validation. The executable guard keeps the written standard observable and reviewable.
