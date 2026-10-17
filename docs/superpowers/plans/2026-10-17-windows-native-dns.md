# Windows native hostname lookup follow-up

> Execute inline with TDD and a fresh security review. Do not commit or push without user approval.

**Goal:** Complete the approved network broker's stock-Node DNS path; the existing controlled UDP DNS test did not establish native external lookup.

**Architecture:** Extend the existing per-process authenticated channel with a bounded hostname/service request and pointer-free address results. Resolve only through the Windows DNS namespace using asynchronous GetAddrInfoExW, its timeout and cancellation API. Adapt verified application GetAddrInfoW/FreeAddrInfoW imports; keep custom allocations wholly inside the child and forward frees of genuine system allocations to Windows. Retain zero capability SIDs and existing network=false behavior.

**Tech stack:** Rust, existing Windows bindings, unchanged Node, existing suspended compatibility installation and Job cleanup.

**Spec:** 2026-10-15-windows-project-networking-design.md; this corrects its implementation's missing native-DNS path under the user's instruction to finish.

## Constraints and review focus

- Never delegate a host token, pointer, filesystem RPC, provider selection or caller PID.
- Fixed frames: 616-byte request, 2084-byte response, 255 UTF-16 units for a name, 31 for a service, at most 32 IPv4/IPv6 addresses and one bounded canonical name.
- Reject malformed strings, unsupported flags/families and oversized results. Validate before consuming a client sequence number.
- Keep request ordering, socket acknowledgements, held-object authentication, backpressure and descendant setup intact.
- Cancellation must retain every asynchronous API allocation until Windows reports completion. Shutdown may not leave resolver work referencing freed storage.
- Test both child-owned and genuine system-owned result freeing, repeated lookups, late addon imports and descendants. Tests use localhost/numeric addresses and local fixtures; external acceptance is a separate manual control.

## Tasks

- [x] Reproduce external DNS failure with the committed CLI and a host positive control. Record ENOTFOUND under network=true and host HTTPS 200.
- [x] Fail the import test for native GetAddrInfoW/FreeAddrInfoW before extending the import adapter.
- [x] Add pointer-free query/result validation and cancellable host resolution on the existing authenticated channels.
- [x] Adapt native lookup/free, preserve allocator ownership and reject invalid requests without consuming sequence numbers.
- [x] Pass the original external workload: native lookup succeeds and HTTPS returns 200.
- [x] Complete native regressions, fresh review and the affected verification lane. Runner: 251 passed, two ignored; CLI run: 21 passed; nested integration: five passed; Clippy/format/architecture pass. Full workspace: 18 symlink-privilege failures.
- [x] Run news-site build/test/start through the CLI with pinned Tapid dependencies and record readiness/cleanup. Original Restricted profiles retained; HTTP marker returned on forwarded port 3001; Ctrl+C gives Cancelled/130 and closes the listener. This is Home dirty-tree development evidence.

## Independent blockers

The standard-user kernel probe confirms ordinary AppContainer can bind IPv4/IPv6 loopback. LPAC fails Winsock initialization (10107); this is not a denial pass. Opening WFP succeeds but adding a valid uniquely named empty sublayer returns access denied (5). No WFP policy was installed. Strict bind denial therefore requires a separately designed privileged guard or a reviewed contract decision; import hooks cannot supply that security boundary. Exact Windows 11 Pro VM126 access remains requested. Local Home results cannot substitute for that target. Symlink privilege remains unavailable.

## Final artifact and replay

Windows 11 Home x64 build 26200, Node 22.23.1 and Rust 1.99.0 MSVC. Final local CLI SHA-256: `accd1da6f242b18e5371c6184eb500974b47eca2ee1c7a962e6f7b0ef43140ef`. Source is parent `57fb1a1008b45aa9a8b2ec29d573382d1c2bc250` plus the uncommitted correction, so this is not clean-commit platform acceptance.

With absolute isolated project/store paths, both `install --frozen` and `install --offline --frozen` replay all 26 packages and preserve the lock hash. On that final artifact, the original `run build` and `run test` exit zero; `run start -- --hostname 127.0.0.1 --port 3001` serves the marker and Ctrl+C gives a Cancelled receipt/130 with the listener closed. The external native-DNS/HTTPS control again resolves and returns 200. The fixture policy remains byte-identical. Evidence: `target/issue157-acceptance/evidence.json`, `readiness.http`, `start.*.log`, and `target/issue157-news-*-final.*.log`.

Verification logs: `target/issue157-dns-runner-final.log`, `issue157-dns-cli-run.log`, `issue157-dns-integration.log`, `issue157-dns-clippy-final.log`, `issue157-dns-workspace.log`. The 18 workspace failures again report Windows error 1314; they were not waived or marked passing. Final independent review found no unresolved critical/important code issue. New changes remain uncommitted.
