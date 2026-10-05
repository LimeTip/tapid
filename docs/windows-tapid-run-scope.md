# Windows `tapid run` support scope

## Goal

Make the existing `tapid run <SCRIPT> -- <ARGS...>` workflow usable on Windows for real Node.js project scripts, with the same explicit policy and fail-closed behavior as the supported runner contract. Tapid selects the project script and Node runtime; Node executes JavaScript. A runner-only prototype is not completion.

## Initial target

- Windows 11 Pro x64, validated on the Proxmox Windows 11 VM.
- Node.js 22, matching the Windows target in issue #157 and the checked-in `examples/news-site-consumer` workload.
- No Windows Server 2025 validation or support claim. Other Windows editions/builds remain unsupported until independently validated.

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

Windows `tapid run` is **not supported yet**. The existing read-only prototype and compile checks do not satisfy this scope.

A baseline red run was recorded on Windows 11 Pro x64 build 26300 in Proxmox VM 126 with Node `v22.6.0`. The release `tapid.exe` was built from upstream commit `fc9fa6233e03c0fe9c183e516201d737e4b70280` (SHA-256 `397380dba2c7c7691171c16667c415b0e984ce851a9bfa0fb304f09b51df1cdb`). Running `tapid run test` against the checked-in #150 fixture files failed with exit code 1 before spawning Node:

```text
sandbox execution failed (unsupported-containment): sandbox containment is unavailable on windows: no platform execution backend is implemented; no process was started and no enforcement receipt was issued
```

This is only a baseline reproduction of the missing backend, not Windows support acceptance: it did not run the complete build/test/start workload or prove ManagedTree controls. The Windows runner has since been ported to the modular architecture, but it must still pass the exact CLI and native ManagedTree acceptance above before documentation or release claims change.

## Current Windows-write investigation (2026-10-05)

The native `Access is denied` failure in the declared-write path was traced to `WindowsFilesystemGrants::apply` skipping project-policy read grants whenever the canonical path was beneath `SystemRoot`. VM 126 uses `C:\Windows\SystemTemp` for temporary project roots, so the project-root read/traversal grant was skipped even though the writable subtree received its inherited write ACE. The fix now skips only backend-runtime read grants beneath `SystemRoot`; it does not enable Windows write support.

On Windows 11 VM 126, Rust 1.99.0 builds and the actual-child-token probe confirmed the original Win32 error 5, then passed existing-file writes, new-file create/reopen/write, and `cmd.exe` redirection after the project-root grant fix. Ancestor ACL inspection then found that the old parent-traversal path added a per-run ACE to shared SystemTemp even though direct opens of SystemTemp and the volume-root directory returned error 5. The child still performed all declared descendant operations after that ACE was removed. The implementation now avoids parent ACEs below SystemRoot and canonicalizes the ancestor/SystemRoot comparison, failing closed if either cannot be resolved; a regression test failed on the old behavior and passed after the change. During the successful probe, volume-root, Windows-directory, and SystemTemp ACLs stayed unchanged; normal cleanup restored all sampled ACLs exactly. The integrated existing-file write test passed earlier in a one-off diagnostic build with the write gate temporarily bypassed. The gate is restored, and its fail-closed integration test passed natively. The full Rust 1.99.0 native `tapid-runner` unit suite passed 110 tests with 1 diagnostic probe ignored. Normal-exit cleanup left no probe SID ACE on `C:\`, `C:\Windows`, or `C:\Windows\SystemTemp`.

Windows `tapid run` remains **not supported**. Outside-grant denial, failure/timeout/cancellation cleanup, and full `examples/news-site-consumer` CLI acceptance are still pending. `cmd.exe` also warns that an extended (`\\?\`) current-directory path is unsupported and defaults to the Windows directory; verify relative-working-directory behavior during CLI acceptance.