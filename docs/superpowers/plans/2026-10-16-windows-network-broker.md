# Windows standard-user networking implementation plan

> Execute inline with independent host/client module work where useful; apply TDD and a fresh security review. Do not commit or push without user approval.

**Goal:** Make explicit Windows Node network grants usable without administrator setup while retaining the no-network path and exact-policy receipts.

**Architecture:** A request-owned broker creates IP sockets under the invoking user and transfers them only to verified held processes in the execution's exact AppContainer and Job. Per-process private pipe pairs separate clients. The embedded component adapts application socket imports and registers suspended descendants through the existing initialization seam.

**Tech Stack:** Rust, existing windows-sys 0.52, embedded Rust DLL, Windows AppContainer/Job/Winsock.

**Spec:** docs/superpowers/specs/2026-10-15-windows-project-networking-design.md (user authorized implementation on October 16).

## Global constraints

- Standard user; no global exemptions, elevated helper, custom Node, or uncontained retry.
- Keep zero network capability SIDs; a network grant must not broaden filesystem ACL authority.
- No broker for network=false. Raw sockets, AF_UNIX, arbitrary providers, paths, or host execution are never broker requests.
- Preserve explicit environment allowlisting, Restricted/ManagedTree distinction, CanonicalPath and existing compatibility exclusions.
- Per-client serialized requests, 64 live clients, fixed bounded frames; cancellation must stop all broker I/O.
- No commit/push. Exact-commit Pro VM126 evidence cannot be fabricated from local Home development results.

## Review focus

Wrong-object/other-Job/other-SID recipient handles; malicious clients not draining replies; socket provider/flag confusion; descendant setup races and handle leaks; DNS/UDP/IPv6 and failed-hook compatibility.

### Task 1: Fixed protocol and host socket broker

Files: create windows_compat/network_protocol.rs and windows_network.rs; existing windows_job.rs exposes its held Job handle privately; Cargo.toml extends existing Windows bindings with Winsock.

Interfaces: Broker::new(job:isize,sid:PSID)->Result<Broker,ExecutionError>; Broker::attach(process:isize)->Result<Channel,ExecutionError>. Channel has request:isize and response:isize (remote child endpoints). Broker drop stops/joins every worker. Request is 40 bytes; response 664 bytes including a 628-byte WSAPROTOCOL_INFOW at offset32. Operations SOCKET=1, REGISTER=2, ACK=3. Fields and validation are specified in the shared module. No caller-provided recipient PID.

- [x] Write and run a stock-Node network=true loopback test; observe existing prelaunch UnsupportedContainment (red).
- [x] Add malformed-frame/family/type/flag and wrong-target tests; implement bounded private channels and held-object verification.
- [x] Prove socket transfer, cancellation, blocked-reader shutdown, registration limits, no host-process delegation and native struct size agreement.

### Task 2: Contained socket imports and descendant channels

Files: windows_compat/dll.rs, imports.rs, protocol.rs, native.rs; create windows_compat/network.rs.

Interfaces: Setup gains network_request/network_response; both zero or both valid. network::socket(af,kind,protocol,info,group,flags)->usize; network::register_child(process)->Result<(isize,isize),u32>. Component serializes per-process RPC, materializes transferred sockets through verified ws2_32 exports, ACKs before reuse, and returns Winsock failures. Use shared wire codec. Original non-creation protocol-info socket duplication stays in the contained process.

- [x] Test socket import recognition, malformed Setup channels and network=false absence.
- [x] Extend verified export matching for ws2_32 imports, root hooks and normal later addon imports.
- [x] Register each suspended descendant before its initializer resumes it. Reject setup failure and preserve caller suspension.

### Task 3: Lifecycle, receipts and actual workloads

Files: windows_execution.rs, windows_compatibility.rs, tests/windows_network.rs, relevant CLI tests and Windows support docs.

- [x] Attach the broker after root Job assignment and before compatibility initialization; keep it through verified-empty-Job cleanup.
- [x] Permit networking only with a trusted Node compatibility binding; unsupported requests without it still reject before spawn.
- [x] Replace obsolete network rejection assertions with unsupported-runtime guards; add native allowed/denied TCP, UDP, IPv6, resolver, descendant and failure controls.
- [ ] Exact-commit news-site build/test/start/readiness on Windows 11 Pro VM126. Local fixture dependencies are unavailable. A separate real CLI HTTP readiness regression passes both policy and permission-flag paths on Windows 11 Home; this does not substitute for the target acceptance.
- [x] Run the full local verification lane, record unavailable platform checks and fix review findings. Results and the unrelated privilege failures are recorded below.

## Final local verification (October 16)

- Complete tapid-runner suite: 246 passed, zero failed, two existing ignored diagnostics. Includes live-broker Ctrl+C with actual payload readiness, timeout cleanup, native socket recovery, root/descendant network grants and denial controls. Log: target/issue157-runner-final.log.
- CLI run filter: 21 passed, including real HTTP readiness under profile and invocation flags and established receipt exclusions. Log: target/issue157-cli-run.log.
- Full workspace Clippy with all targets/features and warnings denied: passed. Formatting and git diff --check: passed.
- All non-CLI capability crates: passed, including runner; nested integration workspace: five passed. Remaining CLI integration targets (run_planning, json_results, npm_import, release_verification, upgrade): passed on this target.
- Offline news-fixture contract/native resolution: 11 passed with the source-built binary, no skipped tests. This is not Next build/test/start acceptance. Architecture check passed for tracked files. New broker module cohesion was reviewed separately: about 530 production lines plus adversarial tests, one private socket-delegation capability.
- Full workspace test lane was attempted and stopped in CLI workspace-link tests: 134 passed, 18 failed, all failures Windows error 1314 (symlink privilege unavailable). These are outside the networking path; no account privilege or global Windows setting was changed. Log: target/issue157-workspace-tests.log.
- Fresh security review completed; invalid-socket sequence bug fixed via native red/green regression. Existing launch, import-address, addon compiler and Job/ACL cleanup behavior was reused rather than duplicated.

Remaining issue acceptance: exact integrated-commit Windows 11 Pro VM126 news-site build/test/start, external connectivity matrix, and no-network local-bind denial. No commit or push was performed.
