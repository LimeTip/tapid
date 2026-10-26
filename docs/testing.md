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

On Unix, the runtime temporary directory must permit executing fixture files.
Some installer and package-bin tests deliberately launch freshly written scripts
directly, so a `noexec` temporary mount produces `PermissionDenied` even with the
correct executable mode. For an isolated worker with a non-executable default
temporary mount, configure `TMPDIR` before starting Node/Cargo to an absolute,
private directory on the worker's executable workspace filesystem. For example,
from the repository root:

```sh
mkdir -p target/test-tmp
chmod 700 target/test-tmp
TMPDIR="$PWD/target/test-tmp" CARGO_TARGET_DIR=target node --experimental-strip-types scripts/dev.ts test -p tapid --lib --locked
```

This selects fixture storage through the existing runtime API; do not replace
direct executable launches with shell invocations or skip execution assertions.
If execution is still denied, inspect the worker mount/security policy before
attributing the failure to executable permissions. This configuration does not
supply native containment prerequisites or establish native platform support.

## Development prerequisites and build reuse

Use Git, Rust with rustfmt and Clippy, and a supported Node.js major (26 preferred,
24, or 22), following the [repository support policy](../README.md#development).
Rust owns package-manager behavior and security verification. TypeScript owns
developer scripts, release orchestration, documentation checks, and website/consumer
fixtures. Python and uv are not required to develop or test Tapid.

Run Cargo from the repository root through:

```text
node --experimental-strip-types scripts/dev.ts <cargo arguments>
```

The wrapper selects `<git-common-dir>/target/dev` using Git's canonical common directory for every repository layout. For a conventional checkout this is `<primary-checkout>/.git/target/dev`. All worktrees of the same repository and the nested integration workspace reuse that directory. Distinct Git common directories have separate caches, including bare and custom layouts. Existing conventional-checkout artifacts in `<primary-checkout>/target/dev` are not reused at the new path, so its first build is cold. Cargo still checks source changes, toolchains, profiles, features, and compiler flags; incompatible artifacts are rebuilt. Concurrent Cargo builds sharing a target directory may wait for its build lock. Keep compiler flags consistent to maximize reuse.

An explicit `CARGO_TARGET_DIR` takes precedence. Direct `cargo` commands use the root worktree's `target` directory for both workspaces through `.cargo/config.toml`, but do not share it across worktrees. Commands and documentation that need a binary should obtain the target path from `node --experimental-strip-types scripts/dev.ts metadata --no-deps --format-version 1 --locked` rather than assume `target/debug/tapid` after a wrapper build.

Windows native no-network execution tests require the administrator-installed guard. Install it once with `tapid sandbox install` from an administrator terminal, then run tests as the ordinary user; missing setup fails closed. Explicit network grants use the standard-user broker independently. The Windows CI test job installs the guard before native tests and uninstalls it afterward. Its separate `guard_owned_namespace_removal` control runs with `--ignored` under administrator rights against unique metadata-only WFP GUIDs, testing rollback and idempotent removal without creating filters or touching the production namespace.

## Focused development lane

Use strict red-green-refactor for production behavior. Add one observable failing test, confirm its expected failure, implement the smallest change, then rerun that test. Pure refactors use existing behavior tests before and after the move. Run Cargo commands sequentially to avoid build-lock contention.

```text
node --experimental-strip-types scripts/dev.ts test -p tapid-resolver --lib --locked <test-name>
```

Replace the package and filter with the relevant test. The CLI package is named `tapid`, although its directory is `crates/tapid-cli`. Use `--test cli`, `--test upgrade`, or `--test run_planning` for the corresponding CLI integration target. A filter matching zero tests is not validation.

## Before handoff

For an isolated Rust change, run the affected crate's tests and Clippy plus workspace formatting:

```text
node --experimental-strip-types scripts/dev.ts test -p tapid-resolver --all-features --locked
node --experimental-strip-types scripts/dev.ts clippy -p tapid-resolver --all-targets --all-features --locked -- -D warnings
node --experimental-strip-types scripts/dev.ts fmt --all --check
```

Include direct consumers when a public interface or behavior they rely on changes. Changes to shared core types, dependency manifests, multi-crate behavior, containment, install transactions, or broad refactoring require the full local lane once after the final change:

```text
node --experimental-strip-types scripts/dev.ts fmt --all --check
node --experimental-strip-types scripts/dev.ts clippy --workspace --all-targets --all-features --locked -- -D warnings
node --experimental-strip-types scripts/dev.ts test --workspace --all-features --locked
node --experimental-strip-types scripts/dev.ts test --manifest-path tests/integration/Cargo.toml --locked
node --experimental-strip-types tools/check_architecture.ts
```

Add checks for the files changed:

- Architecture tooling: `node --experimental-strip-types --test tools/check_architecture_test.ts`.
- Release tooling: `node --experimental-strip-types --test tools/release/*_test.ts`.
- Documentation runner: `node --experimental-strip-types --test tests/doc_examples_test.ts` and the affected examples in `scripts/check-doc-examples.ts`.
- Development wrapper: `node --experimental-strip-types --test tests/dev_test.ts`.
- Bundled agent skills: `node --experimental-strip-types scripts/sync-agent-skills.ts --check` and `node --experimental-strip-types --test tests/agent_skills_test.ts`.
- All TypeScript helpers and release tooling: `node --experimental-strip-types --test tests/*_test.ts tools/release/*_test.ts`.
- News-site native-resolution fixture: set `TAPID_NEWS_FIXTURE_BINARY` to a source-built CLI and run `node --experimental-strip-types --test tests/news_site_fixture_test.ts`. CI requires the binary; without it, only that native probe skips locally.
- Dependency or publication metadata: `node --experimental-strip-types scripts/dev.ts metadata --no-deps --format-version 1 --locked` and `node --experimental-strip-types scripts/dev.ts package --workspace --locked`.

Prose-only documentation changes need link and command review, not Rust compilation. Report the commands run and any unavailable platform checks. Do not rerun successful checks unless subsequent changes affect their results.

## CI and periodic checks

The advanced CodeQL workflow in `.github/workflows/codeql.yml` scans Rust,
JavaScript/TypeScript, and GitHub Actions with the `security-extended` query suite.
It runs when pull requests open, receive commits, reopen, become ready for review,
or are edited, including base-branch changes. There are no PR branch or path
filters. It also runs on pushes to `main`, daily at 03:17 UTC, and manual dispatch.
Extraction starts at the repository root without path filters or crate selection,
covering supported source files across crates, nested workspaces, tests, tools,
scripts, and website fixtures. Rust uses `build-mode: none` rather than selecting
only crates built for the runner platform. CodeQL does not analyze unsupported
file types such as shell scripts or Markdown.

Scheduled runs start after the workflow reaches the default branch. When moving
from GitHub's default CodeQL setup, disable default setup before running this
advanced workflow so GitHub accepts its analysis results.
Main's protection rules require all three `Analyze (...)` checks. First-time fork
contributors require workflow approval under the repository's Actions settings.
GitHub does not run `pull_request` workflows while a PR has merge conflicts and
may delay or drop scheduled runs during service load. Public repositories also
have scheduled workflows disabled after 60 days without repository activity;
re-enable the workflow if this happens. See GitHub's [schedule event
documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule).
These platform limits mean the cron schedule is not a guarantee of a completed
scan every calendar day.
Check the first successful PR run and a default-branch run after activation,
including extraction diagnostics. Source discovery does not guarantee error-free
extraction or complete semantic analysis of platform-specific code.

Workspace packaging, nextest, coverage, dependency audit, and dependency policy remain required in their existing CI jobs. They are not additional local steps for every code change. Run them locally when investigating their results or changing their configuration:

```text
node --experimental-strip-types scripts/dev.ts package --workspace --locked
node --experimental-strip-types scripts/dev.ts nextest run --workspace --all-features --locked
node --experimental-strip-types scripts/dev.ts llvm-cov --workspace --all-features --locked --lcov --output-path lcov.info
node --experimental-strip-types scripts/dev.ts deny check
node --experimental-strip-types scripts/dev.ts audit --deny unsound
node --experimental-strip-types scripts/dev.ts audit --deny unsound --file tests/integration/Cargo.lock
```

Coverage writes the CI artifact `lcov.info`. Mutation testing is a focused periodic test-strength check, not a per-change requirement:

```text
node --experimental-strip-types scripts/dev.ts mutants --package tapid-manifest --timeout 60
```

Install optional tools when working on their lanes. Their absence does not block unrelated local changes or justify weakening CI.

## CI gates

The GitHub Actions workflow runs native tests and Clippy on Ubuntu, macOS, and Windows. Source-only formatting and the pinned JavaScript node-semver oracle run once, on the Ubuntu leg of the existing `Test` matrix: both steps use `if: matrix.os == 'ubuntu-latest'`, and neither the job nor these steps permits failure. Main-push and pull-request CI always include that Ubuntu owner; oracle or formatting failures fail the native test job and therefore block dependent packaging. No separate checkout or toolchain job is added. Separate Ubuntu jobs run nextest, generate an LCOV coverage artifact, and enforce dependency policy with `cargo deny check` and `cargo audit`. Packaging waits for both test and security jobs and validates metadata before `cargo package --workspace --locked`.

Standalone nextest and normal Cargo/libtest coverage remain separate required lanes. Per-test process isolation in combined nextest coverage lost execution of a store traversal instantiation despite matching test identities and aggregate source coverage; normal Cargo coverage therefore remains authoritative. Both Ubuntu lanes explicitly install Node.js 22 and require CLI Node assertions (`TAPID_REQUIRE_NODE_ASSERTIONS=1`). Coverage retains the `lcov-coverage` artifact; no combined lane or comparison-only discovery build is retained.

Failed coverage runs also upload raw profiles and the merge input list as
`failed-coverage-profiles` for one day. A profile merge failure still fails the job.

The oracle uses Node.js 22 and lockfile-pinned `semver` 7.8.5 to validate the authoritative compatibility fixture answers. To reproduce it locally:

```sh
(cd tests/node-semver-oracle && npm ci --ignore-scripts --no-audit --no-fund && npm test --offline)
node --experimental-strip-types scripts/dev.ts fmt --all --check
```

Formatting uses the stable Rust toolchain's rustfmt component, with no repository-specific rustfmt configuration or pinned rustfmt version. Its CI input is the Ubuntu checkout of the same source revision; this assumes the tracked source text is canonical there, rather than validating foreign-OS checkout line-ending conversions. Formatting and JavaScript oracle environment checks on macOS/Windows are intentionally retired. Rust resolver/semver compatibility tests still run within the native workspace suite on every platform, as do Clippy, TypeScript development-cache tests, and the nested integration workspace. Documentation process-supervision tests remain on both Unix platforms. Node.js 22 setup remains unconditional on all three native platforms for the CLI assertions and sandbox tests, not just for the Ubuntu oracle.

The main native `test` job runs `cargo test --workspace --all-features --locked -- --show-output` from the repository root on all three platforms, including the CLI command-description tests. The Windows leg also owns `native_windows_shim_materialization_rejects_collisions_before_writes`: its source remains `#[cfg(windows)]`, `#[test]`, and not ignored; there is no separate filtered invocation. Node.js 22 is installed before testing and `TAPID_REQUIRE_NODE_ASSERTIONS=1` makes the CLI Node probes fail closed, with successful assertion receipts in the logs. Windows also validates the absolute executable resolved from PATH and exports `TAPID_TEST_NODE` before the workspace suite, which owns the nonignored ordinary-project mutation and write-only native contracts; missing or unexecutable Node fails the prerequisite rather than skipping them. The nested integration workspace remains a separate invocation.

The `news-site-consumer` job also runs the CLI `workspace_acceptance` filter on
the same Ubuntu 24.04 and Node 22 reference environment as the news-site fixture.
It requires Node assertion receipts for root/member imports through local links
and registry packages after install, replay, and lifecycle commands. Reproduce
that focused check with
`TAPID_REQUIRE_NODE_ASSERTIONS=1 node --experimental-strip-types scripts/dev.ts test -p tapid --test cli --locked workspace_acceptance -- --show-output`.

The main `release-contract` job owns Unix installer syntax (`sh -n`) and offline `--help`, plus PowerShell tokenization with a nonzero exit on parser errors. Its release-helper suite also exercises that exact PowerShell validator against the real installer and deterministic malformed input, without executing either input or adding a workflow lane. The regression can skip only when `pwsh` is missing locally; CI must execute it. Standalone CLI documentation and website installer validation workflows are no longer needed; no replacement duplicate or manual runner is introduced. Release automation requires successful exact-SHA main-push CI and CodeQL analysis for Actions, Rust, and JavaScript/TypeScript; the obsolete standalone command-help check is not a separate prerequisite. Other release identity, candidate, publication, and smoke gates remain unchanged.

The Windows installer contract consumes the Windows CLI from the same workflow run and attempt. The producer exports its validated image revision through a job output; verification binds the artifact's recorded image revision to that upstream value, rather than to the separate verifier runner's image revision. Hosted image rollouts can give these jobs different revisions of the same Windows image. Both revisions are logged, while the image OS, architecture, source SHA, run/attempt, build settings, compiler receipts, binary digest, and size checks remain enforced. A missing or mismatched producer image output fails before materializing the executable.

The security and package jobs use runner-provided workspaces and do not rely on local absolute paths. A local command may be unavailable on a developer machine, but CI treats the corresponding gate as required.

## Deterministic agent workflows

`agent_workflows` executes the compact [agent guide v1](agent-guide.md) against
local npm-import fixtures and isolated projects/homes. Run it with:

```sh
node --experimental-strip-types scripts/dev.ts test -p tapid --test agent_workflows --locked -- --show-output
```

Each scenario asserts its task result and emits measured command calls, output
bytes, and unnecessary repeated failures. Calls have an 8 KiB result budget.
The scenarios check unchanged locked bytes on replay, unselected package records
across add/remove, preserved npm locks, failure mutations, missing credentials,
peer conflicts, unsupported declarations, cold and shared caches, project-lock
contention, and recovery after a lost commit response. Separate temporary
checkouts model worktree consumers of the same cache; they do not invoke Git.
These are deterministic contract tests, not measurements of an external agent's
reasoning or model tool selection. Existing transaction and containment tests
remain responsible for their broader platform and attack-path coverage.
