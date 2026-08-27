# Testing Tapid

Tapid tests are designed to be repeatable on Linux, macOS, and Windows and to avoid side effects outside the test process.

## Shared fixtures

Use `tapid-test-support` for integration-test setup:

- `TempProject` creates a unique project under the runtime platform temporary directory and removes it on drop.
- `TempHome` creates a separate isolated home with the same lifecycle.
- `fixture_name(label, ordinal)` produces deterministic, filesystem-safe names for snapshots and fixture entries.
- `FakeRegistry` stores copied metadata and archive bytes in memory; it does not bind a socket, read credentials, or depend on a production crate.
- `adversarial_inputs()` supplies traversal, absolute-path, NUL, Unicode, empty, and whitespace cases for boundary validation.

Never construct test paths from `/tmp`, `/Users`, `C:\\`, the repository path, or a user-specific home. Never use the network or real credentials. Fixture writers reject absolute paths and parent-directory components.

## Test workflow

Use strict red-green-refactor for new behavior: first add a focused failing test, verify the expected failure, implement the smallest change, then run the focused test and workspace checks.

```text
cargo test -p tapid-test-support
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --manifest-path tests/integration/Cargo.toml --locked
cargo metadata --no-deps --format-version 1 --locked
cargo package --workspace --locked
```

## CI gates

The GitHub Actions workflow runs tests, formatting, and Clippy on Ubuntu, macOS, and Windows. A separate Ubuntu security job installs and runs `cargo deny check` and `cargo audit`. Packaging waits for both test and security jobs and validates metadata before `cargo package --workspace --locked`.

The security and package jobs use runner-provided workspaces and do not rely on local absolute paths. A local command may be unavailable on a developer machine, but CI treats the corresponding gate as required.
