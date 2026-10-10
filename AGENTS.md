# Working on Tapid

Tapid is a Rust package manager with 18 capability crates. Start with the task map below, the owning crate's README, and the relevant tests. Read broader architecture or ADRs when changing a shared contract or security assumption; ordinary edits do not require reading every document.

## Task map

| Task | Start here | Tests |
| --- | --- | --- |
| CLI parsing, output, command dispatch | `crates/tapid-cli/src/commands/`, `output.rs` | Package `tapid`, integration target `cli` |
| Install, add/remove, replay, transaction recovery | `crates/tapid-cli/src/application/`, `filesystem/` | Package `tapid`, target `cli`; `tests/integration/consumer_install.rs` |
| Registry routing and credentials | `crates/tapid-cli/src/registry.rs`, `online/routing.rs` | Package `tapid`, library tests |
| Online metadata normalization and incremental resolution | `crates/tapid-cli/src/online/resolution.rs` | Package `tapid`, library tests |
| Registry HTTP and npm/JSR parsing | `crates/tapid-registry-client/src/` | Package `tapid-registry-client` |
| Version ranges and dependency graphs | `crates/tapid-resolver/src/lib.rs` | Package `tapid-resolver`; `tests/node-semver-oracle/` for compatibility changes |
| Manifest and workspace semantics | `crates/tapid-manifest/src/` | Package `tapid-manifest` |
| Lockfile format and validation | `crates/tapid-lockfile/src/` | Package `tapid-lockfile` |
| Archive validation, verified storage, installed layout | `crates/tapid-archive/`, `tapid-store/`, `tapid-linker/` | Owning crate; CLI install tests for changed install behavior |
| Script planning and CLI receipts | `crates/tapid-cli/src/run.rs`, `commands/run.rs` | Package `tapid`, targets `run_planning` and `cli` |
| Execution request validation and runtime identity | `crates/tapid-runner/src/execution/request.rs` | Package `tapid-runner` |
| Containment, process supervision, filesystem grants | `crates/tapid-runner/src/execution/`, `execution.rs`, platform backends | Package `tapid-runner`; ADR 0005 and `docs/platform-validation.md` |
| Client upgrades and release verification | `crates/tapid-cli/src/application/upgrade.rs`, `tapid-release-client/`, `tapid-signatures/` | Owning crates; package `tapid`, target `upgrade` |
| Releases, installers, publishing | `tools/release/`, `scripts/install.*`, `.github/workflows/` | `tools/release/*_test.ts`; `docs/release-distribution.md` |
| Documentation examples | `scripts/check-doc-examples.ts`, `docs/documentation-contracts.md` | `tests/doc_examples_test.ts` |
| Developer commands and cache reuse | `scripts/dev.ts`, `.cargo/config.toml` | `tests/dev_test.ts` |

## Change rules

- Use Rust for package-manager behavior, capability tests, and security verification. Use TypeScript on Node.js for developer commands, release orchestration, documentation checks, and website/consumer fixtures. Keep regression tests in the same language as the helper. Do not add Python scripts, Python tests, or a Python development dependency.
- Run TypeScript directly with `node --experimental-strip-types`; prefer Node's built-in test runner and standard library. Keep shell and PowerShell for platform-specific installer integration.
- Keep domain behavior in its capability crate. The CLI composes capabilities and owns interaction. `main.rs` only dispatches and converts exits.
- Keep implementation modules private and preserve deliberate public re-exports. Split cohesive responsibilities rather than targeting a file-length quota.
- Use strict red-green-refactor for production behavior. For private refactors, run existing behavior tests before and after.
- Preserve fail-closed verification, registry identity, atomic install/recovery, and containment. Include failure and attack-path tests when changing a trust seam.
- Use `tapid-test-support` temporary projects and homes. Tests must not use real credentials, the user's home, fixed checkout paths, or external network services.
- Update only documentation whose behavior, interface, security claim, or commands changed. ADRs are for consequential decisions. Private file moves do not require an ADR.
- Never commit or push without the user's explicit approval for that action.
- Request pull request reviews from Codex by posting `@codex review` in a PR comment. Do not request reviews from GitHub Copilot.
- UI work must not add eyebrow headings, overlines, or decorative labels above titles.

## Reuse before adding code

- Before adding a function, type, module, dependency, or script, use the task map and `rg` to find existing implementations of the behavior. Search by domain terms, error codes, and related operations, not just the proposed function name. Read the owning crate's interface, callers, and relevant tests before deciding that new code is needed.
- Prefer calling an existing function. If it lacks a required behavior that belongs to the same capability, extend its interface or implementation while preserving existing callers' contracts. Avoid copying a function to add one variation or introducing a wrapper that only renames an existing operation.
- When repeated code implements the same domain rule, put that rule in one named function or type in its owning capability and migrate the affected callers within the task's scope. Keep implementation helpers private; expose only the interface other crates need. Check existing dependencies and re-exports before adding a crate dependency, and preserve the dependency direction in [docs/architecture.md](docs/architecture.md).
- Choose ownership by behavior. Manifest semantics belong in `tapid-manifest`, registry parsing and transport in `tapid-registry-client`, and verified storage in `tapid-store`. Reuse `tapid-test-support` for shared fixtures. `tapid-core` is for stable, pure domain values, not miscellaneous shared helpers. Name modules for the capability they implement rather than adding a general `utils` or `helpers` module.
- Compare contracts before combining similar code. Validation, registry identity, integrity evidence, transaction ordering, recovery, and platform containment may differ even when the code looks alike. Share the common operation only when those guarantees remain explicit. Keep separate implementations when their rules differ; avoid an interface with unrelated mode flags or caller-specific branches.
- Extract shared behavior for current callers. Do not add traits, configurable frameworks, or public interfaces for hypothetical reuse. A shared function should hide a meaningful operation and make its callers simpler.
- Before handoff, review newly added code for a second implementation of an existing rule. For code changes, name the existing behavior reused or extended in the change description. If similar code remains separate, briefly explain the different contract. Run the existing callers' tests as well as tests for the new behavior, following the verification lane below.

## Verification

Use `node --experimental-strip-types scripts/dev.ts <cargo arguments>` from the repository root. It reuses Cargo artifacts across worktrees and respects `CARGO_TARGET_DIR`. The CLI package name is `tapid`, not `tapid-cli`.

Start with a focused test, such as `node --experimental-strip-types scripts/dev.ts test -p tapid-resolver --lib --locked <test-name>`. Confirm that the filter ran tests. Before handoff, run the affected crate tests and Clippy plus formatting. Cross-cutting changes use the full local lane once. [docs/testing.md](docs/testing.md) defines the commands and triggers, including non-Rust checks.

CI retains cross-platform, compatibility, security, coverage, nextest, and packaging gates. Installing optional CI tools or running mutation testing is not required for an ordinary local change.

## Further context

- [Development rules](docs/development-rules.md): TDD, source-size review, and security expectations.
- [Architecture](docs/architecture.md): capability ownership and dependency direction.
- [ADRs](docs/adr/): read the decision relevant to the changed contract.
- [Security policy](SECURITY.md): vulnerability reporting.
