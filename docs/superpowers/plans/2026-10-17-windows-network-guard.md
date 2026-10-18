# Windows network guard implementation plan

> Execute inline with executing-plans and TDD. Use a fresh whole-change security review. No commit/push without approval.

**Goal:** Enforce network=false native IPv4/IPv6 bind denial on this Windows device with one administrator installation and unattended standard-user runs.

**Architecture:** A protected LocalSystem service owns persistent package-SID deny filters and verifies caller-owned held process/Job handles. The existing lifecycle registers its suspended root before resume and finishes the lease only after verified empty Job. The existing first-in-main dispatch runs the service; CLI management composes a narrow runner interface.

**Tech stack:** Rust, existing windows-sys bindings and sha2, Windows SCM/WFP/local named pipes, unchanged Node.

**Spec:** docs/superpowers/specs/2026-10-17-windows-network-guard-design.md. User explicitly authorized the administrator-installed guard and local-device acceptance.

## Constraints

- No token/capability/allow-rule delegation, user-writable service image, remote pipe, arbitrary caller SID/filter/PID or privilege upgrades through DuplicateHandle.
- Keep exact package SID, original Job, normal grants and existing network=true socket/DNS broker.
- Persistent denial survives service death. Delete only after Job emptiness. Retain orphan denial when proof is unavailable.
- 256 bounded connections, 64 leases/user, fixed <=64-byte messages and bounded nonblocking I/O.
- Parent failure/unavailable service remains fail-closed before project code starts. Administrator install/uninstall only; no routine elevation.

## Review focus

Source granted-access masks and wrong-object handles; thread/process user mismatch and spoofed pipe server; service-image ACL/reparse safety; atomic filter publication and crash lifetime; blocked clients, concurrent leases and cleanup proof.

## Tasks

- [x] Reuse the native root/descendant denial fixture; fail its TCP/UDP IPv4/IPv6 bind test on the current backend. Log: target/issue157-guard-bind-red.log.
- [x] Add fixed guard wire validation and shared bounded token-query reuse, with malformed-frame/access/identity tests.
- [x] Add persistent WFP transaction ownership, exact package-SID conditions, filter counting and only-empty-Job removal.
- [x] Add authenticated local service, bounded poll loop, caller-disconnect Job drain, graceful stop and persistent crash behavior.
- [x] Add precise-rights client, SYSTEM-server verification, native lifecycle registration/finish and receipt diagnostics.
- [x] Add protected one-time SCM installation/status/uninstall plus documented CLI dispatch. Parsing and read-only status are verified; executing install diagnostics remains approval-blocked.
- [ ] Fresh security review; fix regressions; produce concrete installable artifact and perform one administrator installation through the approved elevation path.
- [ ] Pass native bind/connect/descendant denial plus existing positive socket/DNS and lifecycle controls. Verify service-loss/prelaunch failure controls and original news-site CLI acceptance on this device.
- [ ] Run affected crate/CLI tests, full local lane, Clippy, formatting and architecture; record actual OS/build/artifact and any unavailable privilege check honestly.

## Rulings

- The user replaced the historical VM target with this actual device; update current acceptance requirements, preserve historical evidence as history.
- Explicit guard-build authorization covers this concrete deny-service design; routine engineering choices do not need another design approval round. Installation still uses the OS administrator boundary after the artifact is reviewable.
- Persistent orphan denial is preferable to cleanup that silently opens network authority. No blanket uninstall cleanup of orphan filters while process-tree emptiness is unknown.

## Pre-installation checkpoint

- Source-built CLI: target/debug/tapid.exe, SHA-256 787C5A732AF92914E28D12DBEAFEDCDE124B71D2CEA8B8D503143118265C7DD6; parent HEAD 57fb1a1008b45aa9a8b2ec29d573382d1c2bc250 plus dirty DNS/guard changes.
- Six guard tests pass: fixed frames, exact-SID/layer keys, original Job rights, mapped executable identity (rejects a byte-identical copy), precise pipe rights, closed-client recovery and actual peer-token authentication.
- CLI sandbox/help checks pass; eight network-enabled native controls pass; all five nested integration tests pass; workspace Clippy, formatting and architecture pass.
- Full workspace attempt: CLI library 195 passed; CLI integration 131 passed/23 failed (18 existing symlink privilege errors 1314, five missing-guard executions). Separate runner library 207 passed/seven missing-guard-dependent failures/two existing ignored. No test was skipped to claim full acceptance.
- Real original news build invocation returns UnsupportedContainment with actionable missing-guard diagnosis and no enforcement receipt. Guard status reports not installed.
- Full test lane exposed overlong no-network compatibility mechanism evidence; shortened it within the existing 255-byte limit and rechecked the actual CLI path.
- Security review findings fixed: closed pipe listener reset, quota evaluation after authentication, unleased idle expiry, stale image rejection, held mapped-source verification, honest stopped-service filter count, and fresh zero-filter count after stop before uninstall.
- Automatic approval review rejected local machine-wide SYSTEM service installation and automatic privileged CI setup because build authorization did not clearly cover those installation actions. No installation or CI mutation occurred. Explicit approval is required before those steps; no bypass attempted.
- Logs: target/issue157-guard-*.log. Native WFP enforcement, service crash/restart/cleanup acceptance and final original consumer runs remain pending installation. No commit or push performed.
