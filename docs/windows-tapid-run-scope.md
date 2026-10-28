# Windows `tapid run` support scope

## Goal

Make the existing `tapid run <SCRIPT> -- <ARGS...>` workflow usable on Windows for real Node.js project scripts, with the same explicit policy and fail-closed behavior as the supported runner contract. Tapid selects the project script and Node runtime; Node executes JavaScript. A runner-only prototype is not completion.

## Initial target

- Windows 11 Pro x64, validated on the Proxmox Windows 11 VM.
- Node.js 22, matching the Windows target in issue #157 and the checked-in `examples/news-site-consumer` workload.
- GitHub-hosted `windows-latest` validation is enabled with explicit user approval. These Windows Server checks complement, but do not replace, native Windows 11 acceptance; they do not establish support for every Windows edition/build.

## End-to-end acceptance

Run the checked-in `examples/news-site-consumer` build, test, and start scripts through the integrated `tapid run` CLI on the Windows 11 VM. Use the fixture's Tapid-managed project dependencies; do not substitute `npm run` for the execution path. Verify:

- selected Node runtime and project-local `node_modules/.bin` resolution;
- exact script argument forwarding, stdout/stderr, exit codes, and start/readiness response;
- the script runs only after its requested policy is enforced and launch evidence matches that policy;
- filesystem positive controls for each declared read/write grant and negative controls for undeclared paths, parents/siblings, home/temp, traversal, and reparse/link escapes;
- network positive/negative controls that match the configured boolean policy, with a reachable control endpoint;
- explicit environment allowlisting and secret non-inheritance, including descendants;
- descendants cannot escape the ManagedTree boundary; timeout, output, process-count, and memory limits cover the complete tree; cancellation and normal completion leave no surviving descendants;
- unsupported OS/configuration, unavailable primitives, or insufficient rights fail before spawn with a structured diagnostic and no child marker.

Native acceptance must run through `tapid run` at the exact integrated commit on Windows 11 Pro. Record the OS build, architecture, Node/Rust versions, artifact digest, exact commands, exit codes, logs, and per-probe results. Cross-compilation and standalone runner tests are useful development checks but are not support evidence. Do not use Windows Server hosted runners as a substitute for the Windows 11 VM.

## Boundaries

This scope covers explicit root project scripts only. It does not enable dependency lifecycle scripts, claim general npm compatibility, or expand Windows install/replay support. Installation/replay and script execution remain separate evidence dimensions. Unsupported policy combinations continue to fail closed; there is no unsandboxed fallback.

## Current status

### Dependency toolchain authority inventory for #273

Authenticated Windows dependency builds remain disabled at
`dependency_toolchain_identity`. The following source audit starts the remaining
work in [#273](https://github.com/LimeTip/tapid/issues/273). It does not expand the
root-script support claim or complete #157. The LPAC evidence recorded in
[platform validation](platform-validation.md#private-windows-dependency-write-and-key-validation)
establishes sampled AAP denial, not a complete input boundary.

| Input or authority | Current treatment | Required before authenticating generated output |
| --- | --- | --- |
| All Application Packages, `S-1-15-2-1` | Launch opts out. Paired AccessCheck controls inspect the suspended child's actual token before resume. Existing native fixtures deny undeclared AAP-only reads in root and child. | Extend the same fixture to deny AAP-only executable launch and dynamic image loading, with successful host and approved-image controls. |
| All Restricted Application Packages, `S-1-15-2-2` | The LPAC check does not reject ARAP-only authority. Explicit target ACL grants add permissions; they do not subtract ambient grants elsewhere. | Characterize ARAP-only data, executable and dynamic DLL access in root and child. Any accessible bytes outside bound inputs prevent authentication. A System32 digest cannot cover arbitrary ARAP-readable paths. |
| Declared project, dependency and tool inputs | The CLI copies build sources and approved executable bytes into private staging. `Read` grants permit image execution; `ReadData` does not. `tool_identity` includes declared tool names, paths and digests. | Bind the actual staged snapshots and their access/order rules to the recipe, hold their identity through execution, and test adjacent data/DLL changes, replacement and extra-file insertion. A primary executable digest is insufficient. |
| Private writes | The CLI labels private dependency copies for low-integrity writes. General root-script grants have a separate existing-target contract. | Keep temporary output and writable input state separate. Prove later loads and descendants cannot turn writable state into an unbound host tool input. Existing private-write controls remain required. |
| AppContainer profile storage | Each execution creates a fresh named profile and deletes it after owned execution cleanup. The environment builder forces the host `LOCALAPPDATA` and `SYSTEMROOT` values. Fresh naming does not establish empty or inaccessible profile contents. | Inspect actual filesystem and registry storage under the execution token. Classify initial bytes, writable state and profile cleanup failures. Test cross-run contamination without touching a real user's data. |
| Registry | The sole enabled capability is `registryRead`, retained for socket-provider initialization. Token verification excludes other capabilities but does not enumerate readable registry values. | Use disposable registry fixtures with positive and negative controls. Account for readable values, key topology, WOW64 view, value types and registry-driven resolution. Do not equate absence of network capability SIDs with absence of registry inputs. |
| COM and other OS brokers | No `lpacCom` capability is granted. The current token checks do not establish a complete list of reachable endpoints or returned data. | Test representative activation and broker paths under the real token and descendants. Deny undeclared broker inputs or define and bind their finite inputs. Capability omission alone is not a behavioral verdict. |
| KnownDLLs and API sets | Native loader inputs are outside the application's static PE import list. The compatibility adapter excludes system modules from import patching. | Bind effective mappings, architecture and resolved system image bytes, or explicitly document which immutable OS state is trusted and enforce that assumption. Test a changed mapping or runtime byte outside the primary executable. |
| Side-by-side activation and DLL redirection | The launcher does not collect effective activation contexts, manifests or redirection state into a dependency recipe. | Include effective configuration and resolved inputs, including external manifests and dependent-image resolution. Changing configuration or resolution order must change identity or reject execution. |
| Dynamic loads and descendants | The adapter patches supported application imports after ordinary loads. System DLL internals, initialization-time launches, manually mapped images and dynamically resolved process APIs remain outside promised adaptation. LPAC/Job restrictions still apply. | Exercise absolute and searched DLL paths, changed search order, loads during initialization, native calls and descendant execution. Enforce the input boundary independently of these compatibility hooks. A load log or static import scan cannot prove that inaccessible paths remain inaccessible. |
| Backend, OS and compatibility code | The component is embedded, byte-verified and pinned against write/replacement. Receipts disclose its digest. Windows has no authenticated dependency system identity yet. | Bind backend executable/component bytes, OS build and architecture, loader configuration, runtime inputs and ordered declared tool snapshots to the recipe. Enforce consistency between identification and use. |
| Network and inherited authority | The suspended launch verifies package identity, capabilities and Job membership. No-network execution requires the WFP guard; stdio handles and explicit environment are separately constructed. | Retain guard, handle, descendant and cleanup controls. Network-disabled execution does not imply that filesystem, registry or broker inputs are bound. |

The new `windows_containment` characterization keeps the existing native execution
interface. It compares AAP and ARAP fixture grants on disposable
`tapid-test-support` projects. Both the root and a real child read an approved
file, load an approved private image, then attempt the undeclared file,
executable and DLL. Host controls verify the outside executable and DLL first;
successful DLL loads must resolve to the exact copied fixture path. The ARAP
case expects accessible inputs and separately requires the public
`dependency_toolchain_identity` API to reject authentication. These expectations
record the gap; they are not denial acceptance for ARAP. No native result is
claimed until Windows executes the changed tests.

Run the unignored controls through the existing Windows CI acceptance step, after
guard installation:

```text
node --experimental-strip-types scripts/dev.ts test -p tapid-runner --test windows_containment --locked windows_denies_ambient_all_application_packages -- --show-output
node --experimental-strip-types scripts/dev.ts test -p tapid-runner --test windows_containment --locked windows_lpac_records_unbound_restricted_package_inputs -- --show-output
```

Record exact source and artifact identities, Windows build/architecture, Rust
version, commands, successful positive controls and root/child verdicts under
the [platform evidence rules](platform-validation.md#evidence-record).
Compilation on another OS is not native validation. If a characterization
differs on a supported Windows build, investigate the actual token and fixture
ACLs before changing the expected access result.

Microsoft documents [LPAC launch and capability requirements](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)
and [AppContainer local storage](https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-getappcontainerfolderpath).
Its [DLL search-order documentation](https://learn.microsoft.com/en-us/windows/win32/dlls/dynamic-link-library-search-order)
describes redirection, API sets, side-by-side manifests, already loaded modules,
KnownDLLs and search-order configuration. These are the documented reasons to
inventory more than static imports; they are not evidence that Tapid has bounded
those inputs. No complete Windows dependency toolchain identity or new trusted
OS assumption is adopted here. Keep default authentication gated while any row
remains unbound, and retain explicit unsafe overrides without attestations.

### Standard-Node compatibility

The integrated Windows runner now owns standard-Node compatibility. Cargo builds a Rust DLL for the target architecture and embeds its bytes in Tapid, preserving the single-executable installer/release contract. A fresh request-owned copy is pinned against writes and replacement outside project write authority. The launcher verifies the AppContainer token and Job assignment before delegating finite exact ancestor metadata handles and installing the component while the application thread remains suspended. Child and grandchild compatibility uses the same held-handle preparation and preserves caller-requested suspension. Original Node bytes are unchanged.

Ancestor capabilities have FILE_READ_ATTRIBUTES only. They are ReadMetadata/ExactDirectory/BackendRuntime grants in the checked receipt and do not grant directory listing, file contents, writes, image execution, deletion, ownership or ACL editing. Startup checks sensitive DuplicateHandle upgrades for every capability and ReOpenFile upgrades for volume roots. Existing AppContainer ACLs continue to enforce ordinary project access. Kernel restrictions still apply to direct native calls and unhooked modules. No ancestor DACL mutation or host path/PID RPC service is used. Bindings retain CanonicalPath evidence and the existing trusted-host race limitation.

The component translates only libuv's generated pipe prefix to LOCAL and preserves other arguments and errors. Native realpath uses normalized NT paths plus the already bound volume identity; it does not substitute an unnormalized opened path. Startup resolves actual target system-module mappings, including cold-loader initialization, without assuming equal ASLR addresses. Import inspection is bounded, validates the full patch plan and original system-function addresses before modification, and performs no patching under loader lock. Failed initialization kills/drains the owned tree before grants are restored; unsupported requests never retry outside containment.

Native Windows 11 Home x64 build 26200 development tests pass unchanged Node 22.23.1 and official Node 24.21.0 for bidirectional IPC, root metadata, native realpath, default node:test isolation and grandchildren, with undeclared reads/writes denied. The actual integrated CLI passes the original news-site build and test. This is bounded development evidence, not exact-commit Pro/Server/arm64 acceptance, a complete mitigation-policy matrix, or complete network acceptance. The October 16 broker follow-up below adds standard-user Node networking; unsafe/missing write targets remain fail-closed. Per-script tapid.toml profiles remain the permission interface; proposed scoped CLI grants are a separate interface change.

### October 12 native handoff validation

An intermediate runtime-guard experiment required two IPC round trips before any trusted-Node project shell started. It rejected even ordinary otherwise-working scripts and was removed following the user's standard-runtime clarification. The recorded rejection controls did not count as news-site or positive CLI write acceptance. The metadata candidate was tested only on owned disposable directories and failed exact descendant ACL preservation; no shared ancestor or volume-root ACL was changed.

At that superseded intermediate revision, 177 runner library tests passed (2 existing ignored), all 22 containment tests, 3 review regressions and 7 doctests passed, and all 32 linker tests passed. Three selected-runtime containment cases and the CLI write case exercised rejection branches; these were not positive application acceptance. Those conditional acceptance branches have now been removed and the original success assertions restored. The exact metadata regression rejects before any ACL transaction, preserving legacy root/descendant ACL bytes and control bits.

Development validation on Windows 11 Home x64 build 26200, Rust 1.99.0 (GNU target), and Node 22.23.1 uses an uncommitted worktree based on PR #220 head `2f5746544392d3e51807169799c72e03a017adc8`. This is not clean-commit Windows 11 Pro acceptance or a replacement for exact-head hosted CI.

The updated implementation rejects each write target's own drive/share root, independently of the volumes containing home, SystemRoot, or temporary storage. It rejects explicit mandatory-integrity labels that deny low-integrity writes without lowering those labels. After all grants are installed, it checks requested write rights against a verified child token, checks existing files with kernel opens, and explicitly rolls back denial failures. Write-only directory creation rights use a token access check because opening/listing directories would require additional authority. The existing grant masks are shared with these checks; receipt bindings remain CanonicalPath.

Native development checks pass 172 runner library tests (2 existing ignored), all 22 containment tests, 3 review regressions, 7 runner doctests, and 32 linker tests. The CLI execution filter passes 13 tests and script planning passes 9. Windows Node shebang wrappers now execute the selected PATH Node instead of passing JavaScript files to Windows file associations; native command and PowerShell controls verify argument and exit propagation. Unsupported or overlong shebang declarations reject during planning, before shim writes. These checks do not establish full news-site compatibility.

A disposable news-site copy was installed with Tapid and replayed with `--frozen` and `--offline --frozen` without changing its lock hash. After fixing its generated Next.js wrapper, both `tapid run build` attempts fail on `EPERM` while reading metadata for `C:\`. Both original `node --test` attempts reach the configured Tapid timeout. Minimal controls isolate the latter to IPC: inherited-stdio spawning works, while `child_process.fork()` and the default Node test runner time out. Running the diagnostic test without process isolation succeeds, but that diagnostic does not count as acceptance of the original fixture. Official Node 24.21.0 also fails the IPC controls.

The [upstream libuv AppContainer issue](https://github.com/libuv/libuv/issues/5178) describes the matching named-pipe incompatibility. The checked official Node 22.23.3 and 24.21.0 vendored pipe implementations do not contain that fix. Broader ancestor metadata authority or an alternative execution/runtime design requires an explicit follow-up decision; this branch does not modify shared ancestor or volume-root ACLs. At that intermediate revision, `tapid run start --receipt-json` rejected network-enabled policy before spawn without an enforcement receipt. The October 16 broker implementation below supersedes that rejection; issue #157 acceptance remains incomplete.

The complete local workspace command encounters 18 CLI integration failures because this account lacks Windows symlink creation privilege. Other workspace crates, affected runner/linker checks, and focused CLI execution checks run separately. The Ubuntu-owned release-helper suite also has unrelated Windows path/symlink limitations; the affected workflow snapshot and mutation controls pass after correcting the rebased snapshot. These limitations must not be reported as passing full local or hosted lanes.

### Earlier native evidence and bounded implementation

The complete Windows scope above is **not supported yet**. The source branch implements existing ordinary declared project file/directory writes through AppContainer/Job execution. Earlier native Windows 11 build 26300 limited-account CLI acceptance used Node 22.6.0 and covers that earlier tested source. No integrity labels are lowered. Missing targets, reparse/hard-link topology, protected DACLs, blocking explicit integrity labels, volume/share roots and unavailable effective access reject before spawn. Exact-file writes permit content mutation, not replacement or sibling creation; declared subtree descendants permit rename/replacement/removal, but not deletion of the subtree root. Network-enabled policies still reject before spawn, even with writes configured. Receipts remain CanonicalPath with a trusted-host replacement/race assumption, not NativeObject. Restricted authority and ManagedTree lifecycle/resource evidence are separate: Restricted completion is best-effort only. Full news build/test and network/start acceptance, hosted exact-head CI, and the broader common matrix remain outstanding; #157 is not closed. Historical baseline results below describe earlier states, not the current source contract.

A baseline red run was recorded on Windows 11 Pro x64 build 26300 in Proxmox VM 126 with Node `v22.6.0`. The release `tapid.exe` was built from upstream commit `fc9fa6233e03c0fe9c183e516201d737e4b70280` (SHA-256 `397380dba2c7c7691171c16667c415b0e984ce851a9bfa0fb304f09b51df1cdb`). Running `tapid run test` against the checked-in #150 fixture files failed with exit code 1 before spawning Node:

```text
sandbox execution failed (unsupported-containment): sandbox containment is unavailable on windows: no platform execution backend is implemented; no process was started and no enforcement receipt was issued
```

This is only a baseline reproduction of the missing backend, not Windows support acceptance: it did not run the complete build/test/start workload or prove ManagedTree controls. The Windows runner has since been ported to the modular architecture, but it must still pass the exact CLI and native ManagedTree acceptance above before documentation or release claims change.

## Windows ACL restoration evidence boundary

The experimental Windows backend restores temporary AppContainer grants, but does not guarantee byte-identical security metadata for arbitrary legacy-inheritance trees. Windows `SetSecurityInfo`/`SetNamedSecurityInfo` can convert matching explicit ACEs to inherited ACEs and set `SE_DACL_AUTO_INHERITED` while preserving effective permissions. This is distinct from changing the security-significant `SE_DACL_PROTECTED` bit. Such a conversion can also occur in descendants when inheritable ACEs are reapplied.

Native restoration tests establish the current inheritance model on **new disposable fixture roots only**, before taking their baseline. Setup permits only setting the auto-inherited control bit and marking existing ACEs inherited; ACE order, identities, access masks, other flags, and protection must remain unchanged. The library regression additionally checks unchanged descendant ACE bytes during setup. After setup, cleanup comparisons remain exact: integration tests compare the complete `icacls` output, raw DACL bytes, and security-descriptor control bits without masking inheritance. Tests do not normalize shared temp directories, project ancestors, volume roots, or SystemRoot, and this test-only setup is not a production workaround or evidence of arbitrary-metadata restoration.

## Current Windows-write investigation (2026-10-05)

The native `Access is denied` failure in the declared-write path was traced to `WindowsFilesystemGrants::apply` skipping project-policy read grants whenever the canonical path was beneath `SystemRoot`. VM 126 uses `C:\Windows\SystemTemp` for temporary project roots, so the project-root read/traversal grant was skipped even though the writable subtree received its inherited write ACE. The fix now skips only backend-runtime read grants beneath `SystemRoot`; it does not enable Windows write support.

On Windows 11 VM 126, Rust 1.99.0 builds and the actual-child-token probe confirmed the original Win32 error 5, then passed existing-file writes, new-file create/reopen/write, and `cmd.exe` redirection after the project-root grant fix. Ancestor ACL inspection then found that the old parent-traversal path added a per-run ACE to shared SystemTemp even though direct opens of SystemTemp and the volume-root directory returned error 5. The child still performed all declared descendant operations after that ACE was removed. The implementation now avoids parent ACEs below SystemRoot and canonicalizes the ancestor/SystemRoot comparison, failing closed if either cannot be resolved; a regression test failed on the old behavior and passed after the change. During the successful probe, volume-root, Windows-directory, and SystemTemp ACLs stayed unchanged; normal cleanup restored all sampled ACLs exactly. The integrated existing-file write test passed earlier in a one-off diagnostic build with the write gate temporarily bypassed. The gate is restored, and its fail-closed integration test passed natively. The full Rust 1.99.0 native `tapid-runner` unit suite passed 111 tests with 1 diagnostic probe ignored, including a policy-split test where the actual AppContainer child wrote inside the declared subtree and left a read-only sibling unchanged. Normal-exit cleanup left no probe SID ACE on `C:\`, `C:\Windows`, or `C:\Windows\SystemTemp`.

Windows `tapid run` remains **not supported**. Native integration checks verified exact project-root DACL restoration after normal execution, output-limit termination, timeout, pre-spawn missing-executable failure, and graceful Ctrl+C cancellation; these use project read grants only and do not prove write-grant cleanup. Cancellation was tested five times on Windows 11 VM 126 and confirmed `Termination::Cancelled`, complete Job Object cleanup, and exact project DACL restoration. Denial checks for parent directories, undeclared temp/home paths, and reparse/link escapes; nonzero-exit and actual child-creation-failure cleanup; and full `examples/news-site-consumer` CLI acceptance remain pending. The positive write control has not been re-enabled. `cmd.exe` also warns that an extended (`\\?\`) current-directory path is unsupported and defaults to the Windows directory; verify relative-working-directory behavior during CLI acceptance.

### October 14 final review controls

Every delegated ancestor now receives child-token reopen checks for listing/data, execute, mutation, delete and security-edit rights. Only rights already covered by the resolved read/write grants are exempt; write masks reuse the filesystem grant implementation. Preparation rejects an unexpected successful reopen before project code resumes. The policy regression detects a non-root upgrade and distinguishes declared read authority from undeclared write/DACL authority. An actual native-addon probe finds a pinned non-root metadata handle and attempts a listing reopen. Windows 11 Home build 26200 denies that attempt even on the prior revision, so this is not evidence that the reported exploit was reproduced locally.

Compatibility launch hooks now cover application-module imports present at startup and normal later `LoadLibrary*` loads, including native addons and dependencies. A Rust N-API fixture compiled against the unchanged Node process launches a child through its own imported `CreateProcessW`; the child reads root metadata and forks an IPC descendant. That workload failed before the launch-hook change and passes with stock Node 22.23.3 and 24.21.0. Receipts disclose the application-import mechanism and its exclusions: Windows system DLL internals, launches during DLL initialization, manually mapped images and dynamically resolved launch APIs retain kernel containment but have no promised compatibility propagation.

Local verification passes 188 runner library tests (2 existing ignored), 24 containment tests, 3 review regressions, 7 doctests, 13 CLI execution tests and 9 planning tests. Runner Clippy, formatting and architecture checks pass. The disposable original news-site build and test also pass through the revised CLI with stock Node; no custom runtime or shared ancestor ACL change is used. Full cross-platform checks remain the hosted CI lane, since the local account still lacks the symlink privilege needed by unrelated installation tests.

### October 15 network investigation

The following investigation predates the October 16 networking implementation below.

On Windows 11 Home x64 build 26200 with Rust 1.99.0 MSVC, native Rust controls through the existing production launcher show that a no-network-capability AppContainer can bind a loopback listener, while its outbound connection to a reachable host listener times out. The same host positive control connects. The common matrix's blanket loopback-bind denial criterion is therefore still unmet; these results must not be presented as complete network isolation. Native Winsock fixtures require an explicitly allowlisted SystemRoot value: provider initialization failure 10106 without it is not a containment-denial result.

The new windows_network integration target verifies real host TCP payloads outside containment, denied loopback connections in both the root and a real descendant, checked network evidence, Job cleanup, and exact disposable project/runtime/file ACL restoration. It requires neither Node nor an external network service. Run it with:

~~~text
node --experimental-strip-types scripts/dev.ts test -p tapid-runner --test windows_network --locked
~~~

A separate disposable standard-user WSADuplicateSocketW experiment passes real payload controls for a connected stream, an unbound socket connected by the contained Rust child, and an unbound socket bound/listened by that child and reached by the host. It changes no firewall exemptions. This establishes a possible unprivileged transport, not an authenticated production broker, stock-Node compatibility, DNS/UDP/IPv6 support, or network-enabled CLI acceptance. The test-only mailbox and PID exchange are not a proposed security boundary. The design records the scratch artifacts, artifact digest and remaining proof obligations. All results are development evidence based on ca20a6100d283e7701bfca5b6e877f3c6e014512 plus uncommitted test changes, not exact-commit Windows 11 Pro VM126 acceptance.

### October 16 standard-user network broker

Explicit network=true requests with the selected standard Node component now use a private request-owned socket broker in Tapid. No administrator installation, UAC prompt, firewall exemption or network capability SID is required. Every recipient is checked through a held process object for the exact AppContainer SID and Job membership, with zero token capabilities. The broker accepts bounded IPv4/IPv6 TCP/UDP creation requests, retains each socket until materialization acknowledgement, and stops before ACL/profile restoration after the Job is empty. Malformed frames, wrong recipients, backpressure, transfer expiry and the 64-client bound have native controls.

The component adapts application socket imports, including Node Winsock ordinal imports and normal later native-addon loads, and provisions private channels for suspended descendants. Unsupported socket types return an error without invalidating later requests. Dynamic APIs and the pre-existing compatibility exclusions remain outside promised adaptation; kernel restrictions still apply. Declared and established receipt evidence disclose the component identity and compatibility exclusions, unrestricted IP authority, private standard-user broker and zero network SIDs. Network requests without the selected component still fail before spawn.

On Windows 11 Home x64 build 26200 with Rust 1.99.0 MSVC and unchanged Node 22.23.1, native controls pass root/descendant TCP payloads on IPv4/IPv6, UDP payloads, a controlled DNS resolver, native localhost lookup, and listener timeout and active-broker Ctrl+C cleanup with exact project/runtime ACL restoration. Real tapid run start controls serve a host HTTP request under checked-in policy and invocation flags, check the network receipt and complete Job cleanup, and preserve the configuration bytes. These fixtures use local endpoints only. Explicitly allowlist SystemRoot for Winsock provider initialization; Tapid does not add it to the child environment implicitly.

This is dirty-tree development evidence based on ca20a6100d283e7701bfca5b6e877f3c6e014512. The news-site dependencies are unavailable locally, so this follow-up does not claim a fresh news-site build/test/start run. Exact integrated-commit Windows 11 Pro VM126 acceptance, external connectivity controls and the common matrix local-bind denial criterion for network=false remain outstanding. Issue #157 is not fully accepted on this evidence.

Final local verification passes 246 runner tests (two existing ignored diagnostics), 21 CLI run tests, workspace Clippy, formatting, all capability crates, five nested integration tests and 11 offline news-fixture checks. The full workspace test attempt has 18 workspace-link failures with Windows error 1314 because this account lacks symlink privileges; no network test failed in that lane.

### October 17 native hostname resolution and consumer follow-up

A real external control exposed a missing implementation path: stock Node's `dns.lookup('registry.npmjs.org')` and HTTPS returned `ENOTFOUND` under `network=true`, while the same host workload resolved the name and returned HTTPS 200. The earlier controlled UDP resolver and `localhost` tests did not establish this path.

The existing authenticated per-process channel now carries bounded hostname/service requests and pointer-free IPv4/IPv6 results. Host lookup uses the Windows DNS namespace, asynchronous `GetAddrInfoExW`, a five-second timeout and cancellation. The verified application-import adapter covers `GetAddrInfoW` and `FreeAddrInfoW`, retains null versus empty input semantics and Winsock last-error reporting, and owns every result node separately so native callers can split and free result lists. No host pointer, token, arbitrary namespace provider or filesystem operation is delegated. The token retains zero capabilities and `network=false` creates no channel. Receipts disclose native DNS alongside the socket broker. Limits are 255 UTF-16 name units, 31 service units, 32 addresses and a 255-unit canonical name; oversized or unsupported results fail explicitly. Dynamic lookup APIs and the existing compatibility exclusions remain outside promised adaptation.

Native regressions cover malformed frames and address data, invalid-request sequence recovery, repeated and concurrent lookups, native canonical results, split-list freeing in both orders, freeing genuine system allocations, and descendants. Fresh review found and reproduced the split-list allocator and stale Winsock-error defects before they were fixed. Final review reports no unresolved important finding. `GetAddrInfoExCancel` documents immediate completion-event signaling on cancellation; owned buffers remain alive through that signal, even when underlying legacy provider work continues.

Local Windows 11 Home x64 build 26200, Rust 1.99.0 MSVC and unchanged Node 22.23.1 evidence now includes an isolated Tapid installation of the checked-in news-site fixture (26 packages), successful original `tapid run build`, both original fixture tests, and `tapid run start -- --hostname 127.0.0.1 --port 3001` returning `TAPID_NEWS_SITE_ACCEPTANCE_V1`. The policy file is byte-unchanged. Isolated-console Ctrl+C returns 130 with a `Cancelled` receipt and closes the listener. These original fixture profiles select Restricted, so their receipt correctly reports best-effort observed cleanup, not ManagedTree completion. Separate ManagedTree runner/CLI controls retain their complete-cleanup evidence.

The external positive control now resolves the hostname and receives HTTPS 200 through the CLI. The same no-network control fails resolution/connection; native local endpoint controls remain the automated denial evidence. External controls are manual acceptance, not network-dependent regression tests.

Final local verification passes 251 runner tests (two existing ignored diagnostics), 21 CLI run tests, all five nested integration tests, workspace Clippy, formatting, fixture formatting and architecture checks. The full workspace attempt again has 134 passing and 18 failing CLI tests, with Windows error 1314 for missing symlink privilege. The new code reuses the broker's held-process authentication, request sequencing, nonblocking channel owner, import address checks, suspended descendant setup and Job cleanup. It adds a DNS operation rather than a second service or identity mechanism.

This is development evidence from parent commit `57fb1a1008b45aa9a8b2ec29d573382d1c2bc250` plus the uncommitted DNS correction, not clean-commit Windows 11 Pro acceptance. Logs are under `target/issue157-dns-*` and `target/issue157-news-*`; readiness and start receipts are under `target/issue157-acceptance/`.

Strict bind denial is still unresolved. A fresh native ordinary-AppContainer probe binds IPv4 and IPv6 loopback. An LPAC experiment fails Winsock initialization with 10107 and therefore proves no denial. Opening WFP is permitted, but creating a valid uniquely named empty sublayer returns access denied (5); no WFP policy was installed. A privileged enforcement design needs a separate installation boundary and native verification. Import hooks cannot satisfy this kernel security requirement. Windows 11 Pro VM126 access and symlink-capable full workspace verification remain unavailable. Issue #157 remains open; this follow-up does not relax the denial criterion or claim the broader native assurance matrix complete.

### Administrator-installed network guard (installation acceptance pending)

The user selected this physical Windows device for current acceptance; VM126 remains historical evidence, not a prerequisite. The user also authorized building a one-time administrator-installed guard after the standard-user WFP probe returned access denied (5).

Run `tapid sandbox install` once from an administrator terminal. Setup copies the running executable into the OS-known Program Files directory with protected SYSTEM/administrator write rights and ordinary-user read/execute rights, and registers the auto-start LocalSystem service `TapidNetworkGuardV1`. `tapid sandbox status` is read-only. For a stopped service, a standard user without WFP enumeration access sees the retained-filter count as unknown (administrator required); unknown never means zero. `tapid sandbox uninstall` requires administrator rights and refuses active executions or retained denial filters, including after stopping the service. Installing a different build requires removing the idle old guard first. Source copying verifies the held file against the running process image with `NtQueryInformationProcess(ProcessImageFileMapping)` and fails closed if that native query is unsupported. Destination bytes are verified separately.

For `network=false`, preparation registers the held suspended root and its original Job before any project code resumes. The service duplicates only rights the caller already owns, verifies the pipe peer and caller token context, checks the root's exact AppContainer/Job and zero capabilities, and atomically publishes persistent IPv4/IPv6 resource-assignment BLOCK filters for that package SID. Client code verifies a held LocalSystem server against protected SCM configuration. Native import hooks are not the denial boundary. Missing/untrusted/unavailable guard setup returns UnsupportedContainment before project code runs. Explicit `network=true` continues to use the existing standard-user socket/DNS broker without this service.

The service holds each Job until a fresh kernel accounting query proves it empty. Caller loss terminates that caller-owned Job before filter cleanup. An unexpected service exit retains persistent denial; orphan filters are counted and never removed on a timer or by a blanket uninstall. The local pipe excludes remote clients and client server-instance rights, uses fixed frames and bounded nonblocking I/O, expires idle unleased connections, and bounds active connections and per-user leases. Privileged host or BFE tampering remains a trusted-host limitation.

The native TCP/UDP IPv4/IPv6 bind regression reproduces the old gap. Wire, source-image substitution, held-handle rights and pipe-recovery controls pass before installation. Native service/WFP acceptance, crash controls, full runner rerun and final consumer acceptance are pending administrator installation approval. The Windows CI workflow now provisions the guard before tests, runs an isolated metadata-removal/rollback control, and uninstalls the idle guard afterward; those native CI results remain pending. This section does not claim strict bind denial has been verified on the device yet.

The integrated dependency-hook branch retains LPAC startup and its exact enabled `registryRead` runtime capability. Broker recipients and guard registration share the same token-capability check: zero entries or that single non-network SID are accepted; additional, duplicate, disabled or network capability entries are rejected. Existing held-process, exact AppContainer/Job and WFP checks remain required. The merged native Windows acceptance must run at the integrated commit; cross-target compilation does not establish it.
