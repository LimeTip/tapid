# Portable Run Permission Flags Implementation Plan

> **For agentic workers:** Use executing-plans inline with test-driven development and one whole-change independent review.

**Goal:** Implement #285 with identical permission flags and policy behavior across Windows, macOS and Linux.

**Architecture:** CLI owns flag parsing, profile selection and rendering. Runner owns validated policy semantics and canonical scope checks. Extend the existing preparation path to accept the selected policy, preserving existing config-based callers and native backend enforcement.

**Tech Stack:** Rust, clap, existing runner policy/request types; Node development wrapper.

**Spec:** `docs/superpowers/specs/2026-10-15-run-permission-flags.md`.

## Global constraints

- No OS-specific flag vocabulary, custom Node, unrestricted fallback or persisted permission changes.
- Preserve legacy profile-only behavior, assurance, resource ceilings and registry credential filtering.
- Use existing canonical grant resolution and native containment; no lexical scope shortcuts.
- Commit/push only with the user's explicit approval for this new branch.

## Review focus

- Configured profile/default ceilings and a missing exact profile.
- Symlink aliases, component-prefix lookalikes and missing write targets.
- Workspace-root versus member-cwd path interpretation.
- Case-equivalent/reserved environment names and credential filtering after composition.
- Child arguments that look like permission flags and unsupported native dimensions.

## Tasks

- [x] Add a failing CLI help/parser regression; implement shared permission arguments and opaque forwarding tests.
- [x] Settle profile composition; add failing runner policy/scope tests and implement shared validation through existing grant resolution.
- [x] Refactor preparation through one explicit-policy path, preserving existing callers; verify planning before/after.
- [x] Add failing CLI invocation regressions; compose immutable config/default/invocation inputs before environment filtering and execute through that shared path.
- [x] Add provenance to the existing receipt, documentation and native positive/negative/descendant controls. Preserve profile-only receipts.
- [x] Run owning crate tests, affected CLI/planning tests, Clippy, formatting and the applicable full lane; independently review the diff and report native/platform limits accurately.

## Local validation and handoff

- Red/green confirmed for parsing/help, policy selection, canonical ceilings, early request rejection and no-profile invocation. Existing planning tests passed before and after the shared preparation refactor (9 tests).
- Native Windows invocation controls passed with standard Node 22.23.3 and 24.21.0 (7 tests each): scoped writes, denied writes, environment filtering, descendants, profile narrowing, prelaunch widening rejection and unsupported networking.
- All non-CLI workspace capability tests passed, including 193 runner library tests (2 pre-existing ignored tests) and 24 runner integration tests. CLI library tests passed (195); the workspace CLI target passed 133 and failed 18 installation/workspace tests with Windows symlink privilege error 1314.
- Workspace Clippy, formatting, nested integration tests (5), architecture checks including both new production modules, and diff whitespace checks passed. Independent read-only review found no actionable issues.
- Documentation tests passed 10, failed 1 and skipped 37 on Windows. The failure compares an unchanged CRLF quickstart fixture against LF text.
- Native macOS/Linux enforcement and the Unix symlink-alias regression require CI. Local results do not establish those platform results or resolve the separate Windows ambient-authority issue #273.
- User explicitly approved committing, pushing and opening the PR after reviewing the implementation and validation limits.
