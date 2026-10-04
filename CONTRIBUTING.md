# Contributing to Tapid

Thank you for contributing to Tapid. Keep changes focused, testable, and explicit about security and platform assumptions.

## Before contributing

Please read `README.md`, `SECURITY.md`, and the relevant issue or discussion. For a substantial change, open an issue or discussion first with the problem, proposed solution, affected product phase, and verification plan.

## Pull requests

A good pull request:

- Explains the problem and intended behavior.
- Includes tests for changed behavior, including trust-boundary and adversarial cases where relevant.
- Updates documentation when behavior or decisions change.
- Avoids unrelated refactoring.
- States known limitations and platform-specific behavior.
- Contains no credentials, private package data, customer information, or confidential security details.

Use focused branch names such as `feat/lockfile-schema`, `fix/archive-path-validation`, or `ci/security-gates`. Prefer concise conventional commits (`test: add isolated project fixture`, `ci: add dependency audit`).

## Development principles

Tapid favors small vertical slices, tests before production implementation, deterministic machine-readable behavior, explicit security assumptions, and cross-platform verification on macOS, Linux, and Windows. Treat registry metadata, package archives, lifecycle scripts, native binaries, and executable code as untrusted.

Integration tests must use `tapid-test-support` temporary projects and homes. Do not use the current checkout, a fixed absolute path, the real user home, the network, or real credentials. Fake registry fixtures are in-memory and must remain independent of production crates.

## Local verification

Every visible CLI command and nested subcommand needs a help description. Add a doc comment or `#[command(about = "...")]` to its Clap definition. The CLI documentation workflow discovers commands automatically and reports any missing or blank descriptions. Hidden commands are excluded. Run the same check locally with `cargo test -p tapid --lib --locked commands::documentation::`.

Run the narrowest relevant test first, then the full checks that are available in your environment:

```text
cargo test -p tapid-test-support
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo metadata --no-deps --format-version 1 --locked
cargo package --workspace --locked
cargo deny check
cargo nextest run --workspace --all-features --locked
cargo llvm-cov --workspace --all-features --locked --lcov --output-path lcov.info
cargo mutants --package tapid-manifest --timeout 60
cargo audit
```

`cargo nextest` is the faster test runner used by the dedicated Ubuntu CI gate; the canonical `cargo test` command remains required for cross-platform coverage. `cargo llvm-cov` writes an LCOV report to `lcov.info`, matching the CI artifact. `cargo-deny` and `cargo-audit` are CI security gates. `cargo-mutants` is a periodic test-strength check for focused crates, not a per-PR gate. Install these tools with their upstream installers or skip only when documenting why the local tool is unavailable; do not weaken the CI jobs.

## License and security

Tapid source code is licensed under the MIT License. For security vulnerabilities, do not open a public issue; follow `SECURITY.md`.
