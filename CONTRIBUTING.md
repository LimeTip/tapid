# Contributing to Tapid

Thank you for contributing to Tapid. Keep changes focused, testable, and explicit about security and platform assumptions.

## Before contributing

Start with [AGENTS.md](AGENTS.md), the owning crate's README, and the relevant issue or discussion. Read `README.md` for product behavior and `SECURITY.md` when the task concerns security. For a substantial change, describe the problem, proposed solution, affected product phase, and verification plan in the issue, discussion, or requested work before implementation.

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

Every visible CLI command and nested subcommand needs a help description. Add a doc comment or `#[command(about = "...")]` to its Clap definition. The CLI documentation workflow discovers commands automatically and reports any missing or blank descriptions. Hidden commands are excluded. Run the same check locally with `python3 scripts/dev.py test -p tapid --lib --locked commands::documentation::`.

Use [the testing workflow](docs/testing.md) as the single source for local verification commands. Start with the focused test for the changed behavior, run the affected crate checks before handoff, and use the full local lane for cross-cutting changes. CI retains its cross-platform, security, coverage, compatibility, and packaging gates.

Run Cargo through `python3 scripts/dev.py` to reuse build artifacts across this repository's worktrees. Python 3, Git, Rust with rustfmt and Clippy, and Node.js 22.6.0 or later are the development prerequisites. Optional CI tools do not need to be installed for an ordinary local change.

## License and security

The Tapid CLI and its supporting crates in this repository use the [Apache License, Version 2.0](LICENSE). Unless you explicitly state otherwise, your contributions to this repository are provided under the same license, as described in section 5. You retain copyright ownership of your contributions.

For security vulnerabilities, do not open a public issue; follow `SECURITY.md`.
