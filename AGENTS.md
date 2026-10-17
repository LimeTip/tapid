# Working on Tapid

Tapid is a Rust package manager. Start with the task map below, relevant sections of the owning crate's README, its public interface, and nearby tests. Read applicable nested `AGENTS.md` files before changing their code. Read broader architecture or ADRs when changing a shared contract or security assumption; ordinary edits do not require reading every document.

## Task map

| Task | Start here | Tests |
| --- | --- | --- |
| CLI parsing, output, command dispatch | `crates/tapid-cli/src/commands/`, `output.rs` | Package `tapid`, integration target `cli` |
| Install, add/remove, replay, transaction recovery | `crates/tapid-cli/src/application/`, `filesystem/`; `CONTEXT.md` for locked, frozen, offline, and hydration semantics | Package `tapid`, target `cli`; separate integration workspace, target `consumer_install` |
| Registry routing and credentials | `crates/tapid-cli/src/registry.rs`, `online/routing.rs` | Package `tapid`, library tests |
| Online metadata normalization and incremental resolution | `crates/tapid-cli/src/online/resolution.rs` | Package `tapid`, library tests |
| Registry HTTP and npm/JSR parsing | `crates/tapid-registry-client/src/` | Package `tapid-registry-client` |
| Version ranges and dependency graphs | `crates/tapid-resolver/src/lib.rs` | Package `tapid-resolver`; `tests/node-semver-oracle/` for compatibility changes |
| Manifest and workspace semantics | `crates/tapid-manifest/src/` | Package `tapid-manifest` |
| Lockfile format and validation | `crates/tapid-lockfile/src/` | Package `tapid-lockfile` |
| Archive validation, verified storage, installed layout | `crates/tapid-archive/`, `tapid-store/`, `tapid-linker/` | Owning crate; CLI install tests for changed install behavior |
| Script planning and CLI receipts | `crates/tapid-cli/src/run.rs`, `commands/run.rs` | Package `tapid`, targets `run_planning` and `cli` |
| Execution request validation and runtime identity | `crates/tapid-runner/src/execution/request.rs` | Package `tapid-runner` |
| Containment, process supervision, filesystem grants | `crates/tapid-runner/src/execution/`, `execution.rs`, platform backends; read `crates/tapid-runner/AGENTS.md` for runner and CLI containment changes | Package `tapid-runner`; ADR 0005 and `docs/platform-validation.md` |
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

## Code Review Rules

- Assess actionable findings across P0 through P3 at their actual priority. Include maintainability problems and instruction violations when the changed code has a concrete consequence. Do not omit P2 or P3 findings or inflate their priority. Identify the affected code, triggering condition, and impact; do not infer violations of editing or testing chronology from the final diff.
- Flag activation of artifacts or package trees before required integrity verification, or transaction changes that break rollback or interrupted recovery across manifests, lockfiles, and installed layout. Reuse existing verified-store and project-transaction operations; preserve documented restrictions on explicit compatibility modes.
- Flag changes that lose registry origin, peer context, or platform context where they distinguish package instances, or send credentials outside their exact configured origin. Preserve distinct instances or reject unsupported topology before committing project state.
- Keep optional suggestions separate from defects. Avoid speculative issues and style preferences without a repository rule and practical consequence. Leave mechanical formatting and lint checks to CI.

## Reuse before adding code

- Before adding a function, type, module, dependency, or script, use the task map and `rg` to find existing implementations of the behavior. Search by domain terms, error codes, and related operations, not just the proposed function name. Read the owning crate's interface, callers, and relevant tests before deciding that new code is needed.
- Prefer calling an existing function. If it lacks a required behavior that belongs to the same capability, extend its interface or implementation while preserving existing callers' contracts. Avoid copying a function to add one variation or introducing a wrapper that only renames an existing operation.
- Choose ownership by behavior: manifest semantics in `tapid-manifest`, registry parsing and transport in `tapid-registry-client`, verified storage in `tapid-store`, and shared fixtures in `tapid-test-support`. `tapid-core` holds stable, pure domain values, not miscellaneous helpers. Name modules for their capability rather than `utils` or `helpers`. Check existing dependencies and re-exports before adding a crate dependency; preserve the direction in [docs/architecture.md](docs/architecture.md).
- Compare contracts before sharing code. Validation, registry identity, integrity evidence, transaction ordering, recovery, and containment may differ despite similar implementations. Consolidate the same domain rule in its owning capability and migrate affected callers within scope, preserving those guarantees. Keep different contracts separate rather than adding unrelated mode flags or caller-specific branches.
- Extract only behavior needed by current callers. Keep helpers private and expose only the interface consumers need. Do not add traits, frameworks, public interfaces, or wrappers for hypothetical reuse; a shared operation should make callers simpler. Before handoff, check new code for duplicate rules, explain reuse or justified separation, and verify existing callers as well as new behavior.

## Verification

Use `node --experimental-strip-types scripts/dev.ts <cargo arguments>` from the repository root. It reuses Cargo artifacts across worktrees and respects `CARGO_TARGET_DIR`. Run Cargo checks sequentially to avoid shared-cache lock contention. Obtain binary paths through wrapper metadata rather than assuming `target/debug`. The CLI package name is `tapid`, not `tapid-cli`.

Start behavior changes with a focused test, such as `node --experimental-strip-types scripts/dev.ts test -p tapid-resolver --lib --locked <test-name>`. Confirm that the filter ran tests. The separate integration workspace is not covered by `test -p tapid`; use `node --experimental-strip-types scripts/dev.ts test --manifest-path tests/integration/Cargo.toml --test consumer_install --locked <filter>` for its consumer-install tests.

Before handoff, follow the applicable lane in [docs/testing.md](docs/testing.md). Rust changes require affected-crate tests and Clippy plus workspace formatting, including direct consumers when their contracts change. TypeScript changes require the relevant Node tests. Prose-only changes require link and command review. Run the full local lane once after the final change when the documented cross-cutting triggers apply. Reuse successful checks while their inputs remain unchanged.

Report what changed, existing behavior reused or extended, checks run, and remaining verification limits. Explain when similar code stays separate because its contracts differ.

CI retains cross-platform, compatibility, security, coverage, nextest, and packaging gates. Installing optional CI tools or running mutation testing is not required for an ordinary local change.

## Further context

- [Development rules](docs/development-rules.md): TDD, source-size review, and security expectations.
- [Architecture](docs/architecture.md): capability ownership and dependency direction.
- [ADRs](docs/adr/): read the decision relevant to the changed contract.
- [Security policy](SECURITY.md): vulnerability reporting.
