# Tapid development rules

The adoption path is the first product priority. Every change should preserve a working path for:

```text
tapid init
tapid i <package>
tapid install <package>
tapid install
tapid run dev
```

## Architecture rules

1. Build a domain-oriented modular monolith. Group behavior by domain capability, not by technical layer or arbitrary file size.
2. Design deep modules with small interfaces. Keep implementation private and expose a curated interface through deliberate crate-root re-exports.
3. Introduce hexagonal ports only at real I/O seams such as network, filesystem, clock, process, and terminal interaction. Do not add traits for hypothetical variation.
4. Keep domain rules out of `tapid-cli`. `crates/tapid-cli/src/main.rs` is entrypoint-only code for argument dispatch and process exit conversion.
5. Represent security-sensitive operations as explicit state transitions with validated evidence, authorization, failure, and recovery behavior.
6. Record consequential or difficult-to-reverse decisions in `docs/adr/`.

Prefer a directory module only when a capability has a meaningful private hierarchy. A file should normally have one cohesive responsibility. Split it when it contains independently testable concerns, multiple error models, or unrelated reasons to change. Do not split a cohesive algorithm merely to satisfy a number.

## Strict TDD and vertical slices

Use strict red, green, refactor cycles for production behavior:

1. Add one focused test for an observable behavior.
2. Run it and confirm that it fails for the expected missing behavior.
3. Implement the smallest complete vertical slice.
4. Run the focused test and confirm it passes.
5. Refactor only while tests remain green.
6. Add failure and attack-path tests at each affected trust seam.
7. Run related crate and workspace checks before completion.

A vertical slice crosses only the capabilities needed to deliver behavior. Do not build broad horizontal layers, unused ports, or speculative commands. Keep manifest semantics in `tapid-manifest`, registry transport in `tapid-registry-client`, resolution in `tapid-resolver`, storage in `tapid-store`, and materialization in `tapid-linker`.

Tests use runtime-derived temporary paths. Preserve existing user files. Malformed metadata, unverified artifacts, invalid security state transitions, and unsupported recovery paths fail closed.

## Source size review

Run:

```text
python3 scripts/check_architecture.py
```

The checker scans Git-tracked production Rust files and excludes tests plus top-level generated or build output trees. Ordinary production modules remain in scope even when their directory is named `build` or `generated`. Eight hundred physical lines is an advisory review recommendation, not a validation failure. Crossing it prompts review of cohesion, interface depth, change locality, and navigability. Split only when that review identifies clearer responsibility or a better seam; a cohesive deep module may remain larger without an exception.

`crates/tapid-cli/src/main.rs` has a separate hard 100 physical line entrypoint threshold. It should contain argument dispatch and exit conversion only. A documented temporary exception can acknowledge existing migration debt, but new behavior should move behind capability interfaces and must not increase that debt without review. `docs/architecture-exceptions.txt` is reserved for explicit hard architecture limits.

## Completion checks

Run the narrow test during each TDD cycle, then the relevant checks:

```text
python3 -m unittest tests.test_check_architecture -v
python3 scripts/check_architecture.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --manifest-path tests/integration/Cargo.toml --locked
cargo package --workspace --locked
```

Use ADRs and durable documentation to keep code, tests, security claims, and architecture consistent. Do not add registry publication, sandbox claims, or authentication shortcuts as part of adoption work.
