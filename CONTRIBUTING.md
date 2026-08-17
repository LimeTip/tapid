# Contributing to Tapid

Thank you for your interest in Tapid.

Tapid is a small project in the design and planning stage. The best contributions right now are thoughtful documentation, architecture reviews, security feedback, compatibility research, tests, and small focused implementation changes once the Rust workspace is available.

## Before contributing

Please read:

- `README.md`
- `SECURITY.md`
- The relevant issue or discussion

For a substantial change, open an issue or discussion first. Explain the problem, proposed solution, affected product phase, and how the change will be verified.

## Pull requests

Please keep pull requests focused and easy to review. A good pull request should:

- Explain the problem and intended behavior.
- Include tests for changed behavior.
- Include security tests when a trust boundary is affected.
- Update documentation when behavior or decisions change.
- Avoid unrelated refactoring.
- State known limitations and platform-specific behavior.
- Contain no credentials, private package data, customer information, or confidential security details.

Suggested branch names:

```text
feat/lockfile-schema
fix/archive-path-validation
docs/contributing-guide
security/private-registry-routing
```

Suggested commit style:

```text
docs: improve contribution guide
feat: add deterministic lockfile validation
security: reject cross-origin registry credentials
```

## Development principles

Tapid favors:

- Small, reviewable vertical slices.
- Tests before production implementation.
- Deterministic behavior and stable machine-readable output.
- Explicit security assumptions.
- Cross-platform testing on macOS, Linux, and Windows.
- Honest documentation of compatibility differences and limitations.

Treat registry metadata, package archives, lifecycle scripts, native binaries, and executable code as untrusted.

## Verification

When the Rust workspace is available, run the relevant focused tests followed by:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
cargo audit
```

Integration tests must use temporary homes and isolated registries. Never use real credentials in tests.

## License

Tapid source code is licensed under the MIT License. Contributions must be submitted by people who have the right to submit them and must be compatible with that license.

Submitting a contribution does not automatically transfer copyright ownership to LimeTip Company. LimeTip may introduce a Contributor License Agreement in the future if additional rights are required.

For security vulnerabilities, do not open a public issue. Follow `SECURITY.md`.
