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
| Documentation examples | `scripts/check-doc-examples.py`, `docs/documentation-contracts.md` | `tests/test_doc_examples.py` |
| Developer commands and cache reuse | `scripts/dev.py`, `.cargo/config.toml` | `tests/test_dev.py` |

## Change rules

- Keep domain behavior in its capability crate. The CLI composes capabilities and owns interaction. `main.rs` only dispatches and converts exits.
- Keep implementation modules private and preserve deliberate public re-exports. Split cohesive responsibilities rather than targeting a file-length quota.
- Use strict red-green-refactor for production behavior. For private refactors, run existing behavior tests before and after.
- Preserve fail-closed verification, registry identity, atomic install/recovery, and containment. Include failure and attack-path tests when changing a trust seam.
- Use `tapid-test-support` temporary projects and homes. Tests must not use real credentials, the user's home, fixed checkout paths, or external network services.
- Update only documentation whose behavior, interface, security claim, or commands changed. ADRs are for consequential decisions. Private file moves do not require an ADR.
- Never commit or push without the user's explicit approval for that action.
- UI work must not add eyebrow headings, overlines, or decorative labels above titles.

## Verification

Use `python3 scripts/dev.py <cargo arguments>` from the repository root. It reuses Cargo artifacts across worktrees and respects `CARGO_TARGET_DIR`. The CLI package name is `tapid`, not `tapid-cli`.

Start with a focused test, such as `python3 scripts/dev.py test -p tapid-resolver --lib --locked <test-name>`. Confirm that the filter ran tests. Before handoff, run the affected crate tests and Clippy plus formatting. Cross-cutting changes use the full local lane once. [docs/testing.md](docs/testing.md) defines the commands and triggers, including non-Rust checks.

CI retains cross-platform, compatibility, security, coverage, nextest, and packaging gates. Installing optional CI tools or running mutation testing is not required for an ordinary local change.

## Further context

- [Development rules](docs/development-rules.md): TDD, source-size review, and security expectations.
- [Architecture](docs/architecture.md): capability ownership and dependency direction.
- [ADRs](docs/adr/): read the decision relevant to the changed contract.
- [Security policy](SECURITY.md): vulnerability reporting.
