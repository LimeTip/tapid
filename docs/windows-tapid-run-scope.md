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

Windows `tapid run` is **not supported yet**. The existing read-only prototype and compile checks do not satisfy this scope. The implementation must be ported to the current modular runner architecture, then pass the exact CLI and native ManagedTree acceptance above before documentation or release claims change.