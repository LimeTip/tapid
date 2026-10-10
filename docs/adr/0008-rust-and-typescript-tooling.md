# ADR 0008: Use Rust and TypeScript for repository tooling

Status: Accepted
Date: 2026-10-10

## Context

Tapid's runtime and capability tests use Rust. Release tooling and consumer
fixtures already use TypeScript and Node.js. Four Python helpers and their tests
introduced a third development runtime for Cargo cache selection, documentation
execution, offline release verification, and installed dependency comparison.
None required Python-specific language features.

## Decision

Keep package-manager behavior and security verification in Rust. Use TypeScript
for developer commands, release orchestration, documentation checks, and
website/consumer fixtures. Test those helpers with Node's built-in test runner.
Run TypeScript directly with `node --experimental-strip-types`, requiring
Node.js 22.7.0 or later. This minimum includes default module syntax detection
for the helpers' ESM imports and exports without a root `package.json` module
declaration. Existing CommonJS `.js` fixtures retain their module behavior.
Prefer the Node standard library instead of adding packages for the migration.

Shell and PowerShell remain for platform-specific installer integration.
Python is no longer a repository prerequisite. Do not add new Python helpers or
tests. This decision does not restrict languages in packages installed by Tapid.

## Consequences

The Cargo wrapper remains a script so it works before the Rust workspace builds.
It preserves explicit `CARGO_TARGET_DIR` and derives shared caches from Git's
canonical common directory.

Documentation execution retains literal command validation, isolated homes,
bounded output, Unix process-group cleanup, exact binary identity, frozen
lockfile checks, and persisted failure reports. Source builds select Cargo's
executable artifact and obtain the CLI version from Cargo metadata.

Offline release fixtures use Node's Ed25519 implementation with a public test
seed. Production signature verification remains in Rust. Local fixture
transport rejects unknown URLs without network access.

CI and published-release smoke checks use Node.js for these helpers. Release
automation requires CodeQL analysis for Actions, Rust, and JavaScript/TypeScript.
GitHub's default CodeQL configuration is managed outside this repository and
should use those scopes when the migration reaches the default branch.

## Verification

Preserve the helpers' regression coverage and run the offline release harness
against a source-built Tapid executable. Check workflow syntax and contracts,
then run the full local lane documented in [testing.md](../testing.md).
