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

## Development prerequisites and build reuse

Use Python 3, Git, Rust with rustfmt and Clippy, and Node.js 22.6.0 or later. Run Cargo from the repository root through:

```text
python3 scripts/dev.py <cargo arguments>
```

On Windows, use `python` if that is the name of your Python 3 executable.

The wrapper selects `<git-common-dir>/target/dev` using Git's canonical common directory for every repository layout. For a conventional checkout this is `<primary-checkout>/.git/target/dev`. All worktrees of the same repository and the nested integration workspace reuse that directory. Distinct Git common directories have separate caches, including bare and custom layouts. Existing conventional-checkout artifacts in `<primary-checkout>/target/dev` are not reused at the new path, so its first build is cold. Cargo still checks source changes, toolchains, profiles, features, and compiler flags; incompatible artifacts are rebuilt. Concurrent Cargo builds sharing a target directory may wait for its build lock. Keep compiler flags consistent to maximize reuse.

An explicit `CARGO_TARGET_DIR` takes precedence. Direct `cargo` commands use the root worktree's `target` directory for both workspaces through `.cargo/config.toml`, but do not share it across worktrees. Commands and documentation that need a binary should obtain the target path from `python3 scripts/dev.py metadata --no-deps --format-version 1 --locked` rather than assume `target/debug/tapid` after a wrapper build.

## Focused development lane

Use strict red-green-refactor for production behavior. Add one observable failing test, confirm its expected failure, implement the smallest change, then rerun that test. Pure refactors use existing behavior tests before and after the move. Run Cargo commands sequentially to avoid build-lock contention.

```text
python3 scripts/dev.py test -p tapid-resolver --lib --locked <test-name>
```

Replace the package and filter with the relevant test. The CLI package is named `tapid`, although its directory is `crates/tapid-cli`. Use `--test cli`, `--test upgrade`, or `--test run_planning` for the corresponding CLI integration target. A filter matching zero tests is not validation.

## Before handoff

For an isolated Rust change, run the affected crate's tests and Clippy plus workspace formatting:

```text
python3 scripts/dev.py test -p tapid-resolver --all-features --locked
python3 scripts/dev.py clippy -p tapid-resolver --all-targets --all-features --locked -- -D warnings
python3 scripts/dev.py fmt --all --check
```

Include direct consumers when a public interface or behavior they rely on changes. Changes to shared core types, dependency manifests, multi-crate behavior, containment, install transactions, or broad refactoring require the full local lane once after the final change:

```text
python3 scripts/dev.py fmt --all --check
python3 scripts/dev.py clippy --workspace --all-targets --all-features --locked -- -D warnings
python3 scripts/dev.py test --workspace --all-features --locked
python3 scripts/dev.py test --manifest-path tests/integration/Cargo.toml --locked
node --experimental-strip-types tools/check_architecture.ts
```

Add checks for the files changed:

- Architecture tooling: `node --experimental-strip-types --test tools/check_architecture_test.ts`.
- Release tooling: `node --experimental-strip-types --test tools/release/*_test.ts`.
- Documentation runner: `python3 -m unittest discover -s tests -p test_doc_examples.py -v` and the affected examples in `scripts/check-doc-examples.py`.
- Development wrapper: `python3 -m unittest discover -s tests -p test_dev.py -v`.
- Dependency or publication metadata: `python3 scripts/dev.py metadata --no-deps --format-version 1 --locked` and `python3 scripts/dev.py package --workspace --locked`.

Prose-only documentation changes need link and command review, not Rust compilation. Report the commands run and any unavailable platform checks. Do not rerun successful checks unless subsequent changes affect their results.

## CI and periodic checks

Workspace packaging, nextest, coverage, dependency audit, and dependency policy remain required in their existing CI jobs. They are not additional local steps for every code change. Run them locally when investigating their results or changing their configuration:

```text
python3 scripts/dev.py package --workspace --locked
python3 scripts/dev.py nextest run --workspace --all-features --locked
python3 scripts/dev.py llvm-cov --workspace --all-features --locked --lcov --output-path lcov.info
python3 scripts/dev.py deny check
python3 scripts/dev.py audit
```

Coverage writes the CI artifact `lcov.info`. Mutation testing is a focused periodic test-strength check, not a per-change requirement:

```text
python3 scripts/dev.py mutants --package tapid-manifest --timeout 60
```

Install optional tools when working on their lanes. Their absence does not block unrelated local changes or justify weakening CI.

## CI gates

The GitHub Actions workflow runs tests, formatting, and Clippy on Ubuntu, macOS, and Windows. Separate Ubuntu jobs run nextest, generate an LCOV coverage artifact, and enforce dependency policy with `cargo deny check` and `cargo audit`. Packaging waits for both test and security jobs and validates metadata before `cargo package --workspace --locked`.

The security and package jobs use runner-provided workspaces and do not rely on local absolute paths. A local command may be unavailable on a developer machine, but CI treats the corresponding gate as required.
