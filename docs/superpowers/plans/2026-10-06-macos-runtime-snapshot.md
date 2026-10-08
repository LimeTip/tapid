# macOS Runtime Snapshot Implementation Plan

> **For agentic workers:** Execute this bounded plan task-by-task with TDD.

**Goal:** Reject Node binaries whose dynamic dependencies cannot safely survive private executable relocation, before any project code runs.

**Architecture:** Keep the byte-verified distinct-inode snapshot. Inspect the held source Mach-O load commands before copying, accepting only absolute Apple system-library dependencies. Reject relative, rpath, external-library, malformed and universal Mach-O runtimes explicitly rather than executing them or rewriting signed binaries. Scripts retain their existing behavior. Supporting external dylibs requires a separate verified transitive-library closure design.

**Tech Stack:** Rust, macOS Mach-O, clang-built dynamic-linking test fixtures.

## Global Constraints
- No mutation of the selected runtime; no broader write grants.
- No Windows or CI edits, CLI messaging handled separately.
- Work only in `fix/macos-runtime-snapshot`; commit locally, no push or merge.
- Use Rust and rustdoc from `/Users/doug/.rustup/toolchains/1.99.0-aarch64-apple-darwin/bin`.

## Task 1: Fail closed before nonrelocatable snapshots run
**Files:** Modify `crates/tapid-runner/src/macos_restricted.rs`; document limits in `crates/tapid-runner/README.md`.
- [x] Add a clang-built executable linked to `@rpath/libfixture.dylib` via `@loader_path/../lib`; confirm original runs, require snapshot creation to fail with an actionable dependency error, and assert original bytes unchanged.
- [x] Run `cargo test --locked -p tapid-runner reserved_node_rejects_nonrelocatable_macho -- --nocapture` and observe the current code incorrectly accepts it.
- [x] Inspect bounded load commands on the held source file, reject unsupported dependency paths with `InvalidData`, rewind before byte-copy. Keep exact snapshot verification and policy unchanged.
- [x] Re-run the regression and reserved-node tests. Add malformed/universal input rejection coverage and standalone native acceptance.

## Task 2: Keep hardlink containment regression runtime-independent
- [x] Run `cargo test --locked -p tapid-runner project_hardlink_cannot_mutate_reserved_node_or_trusted_runtime -- --nocapture` to capture the existing Homebrew dyld failure.
- [x] Replace its ambient Node copy with an explicit script fixture using system Ruby to produce the requested markers, retaining all mutation, inode and surviving-descendant assertions.
- [x] Run `cargo test --locked -p tapid-runner`, `cargo fmt --all -- --check`, and `cargo clippy --locked -p tapid-runner --all-targets -- -D warnings`.
- [x] Review diff; commit only these runner files and this plan locally.
