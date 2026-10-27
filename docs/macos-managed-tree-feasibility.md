# Native macOS ManagedTree investigation

Research for [#275](https://github.com/LimeTip/tapid/issues/275), informing
[#276](https://github.com/LimeTip/tapid/issues/276). **Incomplete development
investigation; no candidate establishes ManagedTree.** This document does not
change ADR 0005, ADR 0008, dependency approval, or platform support.

## Executable scope

The disposable Rust example is
`crates/tapid-runner/examples/macos_managed_tree_probe.rs`. Its private modules
keep research observations out of production enforcement receipts. It reuses
public request construction and `execute`, existing dependencies, `TempProject`,
and `TempHome`. No production runner implementation changes.

Commands:

| Command | Behavior | Interpretation |
| --- | --- | --- |
| `rejection` | Generated-file Restricted positive control, followed independently by ManagedTree and each configured limit through the public API | Requires `UnsupportedContainment` and absent target marker for each unsupported request. Any positive fixture failure makes the entire command exit 2. |
| `lineage` | Fresh worker/child processes; ordinary execution, `setsid`, `setpgid`, and double-fork/exec; worker exits before a delayed child write | Explicitly unsandboxed diagnostic, no authority or cleanup guarantee. No candidate kills or process-group inference. |
| `xpc` | Connect to the embedded service, perform paired filesystem controls | Requires service reply bits 15: acknowledgement, generated-file read/write, permission-denied outside read and write. Missing files, arbitrary errors and missing service do not satisfy denial. |
| `worker ordinary\|setsid\|setpgid\|double-fork` | Private research fixture, launches one acknowledged child then exits | One payload with 300 ms delay; double-fork adds two immediately exiting intermediates. |
| `child TRANSITION MILLISECONDS` | Private research fixture; delay bounded to 1–2000 ms | Creates only relative synthetic marker files. Finite work, **not** a kernel-enforced expiry deadline. |

Malformed arguments exit 2. Non-macOS `rejection` returns unsupported; macOS-only
commands are unavailable. Workers clear the child environment, close input, and
redirect output before spawning the diagnostic child. Readiness is published by
rename. Fixture cleanup and finite child work are harness measures, never
candidate-provided cleanup. PID/PPID/PGID/SID values are diagnostic only.

The XPC C bridge is limited to documented SDK messaging and Clang blocks. Rust
performs the filesystem controls, result checks and synthetic fixture creation.
The client has a three-second reply deadline; a timeout returns a setup failure.
No arbitrary dependency script or external service is dispatched. The minimal
bundle is not a production identity or broker-authentication design. App Sandbox
container provisioning is performed by macOS and requires an appropriately
isolated native test account; `TempHome` alone does not relocate that OS state.
Do not use real secrets or grant this worker access to user data.

A lineage run rejects an already-present marker at worker exit as inconclusive.
The double-fork fixture allocates argv before forking and uses only fork, execv
and _exit in child branches until a fresh Rust payload image starts. A successful
intermediate exit does not end the readiness deadline.

## Candidate ledger

| Candidate | Documented or installed evidence | Missing contract / current disposition |
| --- | --- | --- |
| App Sandbox / bundled XPC | [Apple XPC guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html): service bundle, code signing, separate sandbox and launchd lifecycle; [App Sandbox](https://developer.apple.com/documentation/security/app-sandbox) | Signing and bundle verification observed. Service launch/authority controls unverified. These documents do not establish pre-execution ownership of every descendant, authoritative emptiness, kill-on-client-death or tree-wide process/memory admission. |
| Seatbelt Restricted comparison | Existing production prelaunch probes and checked public API | Positive profile installation unavailable in this sandbox. Unsupported dimensions rejected. Deprecated/private mechanisms; no ManagedTree ownership. |
| launchd / process groups | [Apple launchd manual](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5) scopes abandoned-process cleanup to group membership and resource keys to rlimits | Session/group-changing controls are implemented; launchd job installation and adversarial job runs remain unverified. Group membership cannot substitute for authoritative tree ownership. |
| kqueue | Installed SDK `sys/event.h`, line 356: NOTE_TRACK/NOTE_TRACKERR/NOTE_CHILD unsupported since 10.5 | Recursive process discovery cannot establish assignment before execution. No polling ownership claim. |
| rlimits | [Apple setrlimit manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/setrlimit.2.html) | Per-process/per-user scope cannot be relabelled tree accounting. Native paired aggregate-limit experiments remain unimplemented. |
| Coalitions | [Apple XNU header](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/coalition.h); `sys/coalition.h` absent from installed public SDK | Diagnostic-only. No supported third-party deployment, entitlement, assignment or owner-death contract established. |
| Non-native | No candidate selected | Linux VM product backend excluded. Remote workers and macOS guests require separate product/trust decisions. |

The Apple XPC guide was retrieved during this implementation. Other ledger links
identify references for continued investigation; they are not claims that every
current primitive contract has been verified. Source presence does not imply a
supported public interface. No candidate has an authoritative boundary ID.

## Reproduction

From the repository root, run Cargo sequentially. All Cargo commands use:

```sh
PATH=/Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin:$PATH CARGO_TARGET_DIR=target /Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin/node --experimental-strip-types scripts/dev.ts test -p tapid-runner --lib --locked managed_tree_and_every_resource_limit_fail_closed
PATH=/Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin:$PATH CARGO_TARGET_DIR=target /Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin/node --experimental-strip-types scripts/dev.ts test -p tapid-runner --example macos_managed_tree_probe --locked
PATH=/Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin:$PATH CARGO_TARGET_DIR=target /Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin/node --experimental-strip-types scripts/dev.ts build -p tapid-runner --example macos_managed_tree_probe --locked --message-format=json > target/probe-build.jsonl
```

Resolve the compiler artifact rather than assuming a Cargo output path:

```sh
PROBE=$(/Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin/node -e 'const fs=require("node:fs"); const rows=fs.readFileSync("target/probe-build.jsonl","utf8").trim().split("\n").map(JSON.parse); console.log(rows.find(x=>x.reason==="compiler-artifact"&&x.target.name==="macos_managed_tree_probe"&&x.executable).executable)')
"$PROBE" rejection
"$PROBE" lineage
```

For the XPC experiment use a disposable native account with execution-enabled
scratch and container provisioning. These are research-local bundles, with no
LaunchAgent/LaunchDaemon registration or privileged helper installation:

```sh
PROBE_FIXTURES=crates/tapid-runner/examples/macos_managed_tree_probe/xpc
PROBE_APP=target/macos-managed-tree-research/Probe.app
PROBE_SERVICE="$PROBE_APP/Contents/XPCServices/org.tapid.research.worker.xpc"
mkdir -p "$PROBE_APP/Contents/MacOS" "$PROBE_SERVICE/Contents/MacOS"
cp "$PROBE" "$PROBE_APP/Contents/MacOS/Probe"
cp "$PROBE" "$PROBE_SERVICE/Contents/MacOS/Worker"
cp "$PROBE_FIXTURES/app.plist" "$PROBE_APP/Contents/Info.plist"
cp "$PROBE_FIXTURES/worker.plist" "$PROBE_SERVICE/Contents/Info.plist"
/usr/bin/clang -Wall -Wextra -Werror -fblocks -dynamiclib "$PROBE_FIXTURES/bridge.c" -o "$PROBE_APP/Contents/MacOS/libprobe.dylib"
cp "$PROBE_APP/Contents/MacOS/libprobe.dylib" "$PROBE_SERVICE/Contents/MacOS/libprobe.dylib"
/usr/bin/codesign --force --sign - "$PROBE_APP/Contents/MacOS/libprobe.dylib"
/usr/bin/codesign --force --sign - "$PROBE_SERVICE/Contents/MacOS/libprobe.dylib"
/usr/bin/codesign --force --sign - --entitlements "$PROBE_FIXTURES/entitlements.plist" "$PROBE_SERVICE"
/usr/bin/codesign --force --sign - "$PROBE_APP"
/usr/bin/codesign -d --entitlements - "$PROBE_SERVICE"
/usr/bin/codesign --verify --deep --strict "$PROBE_APP"
"$PROBE_APP/Contents/MacOS/Probe" xpc
```

Ad hoc signing succeeded on the development host. This does not establish
Developer ID distribution, notarization, production service availability or a
need for restricted entitlements. The single declared entitlement is
`com.apple.security.app-sandbox=true`. Any packaging, signature, connection or
fixture error is unverified setup, not expected authority denial.

## Development evidence and gaps

Base revision: `3d9461fbeef33ab66c35a63fa1cf4b2d06437677`; new uncommitted research
files. Host: macOS **27.0.1**, build **26A434**, Darwin **27.0.0**
`xnu-13432.1.9~1/RELEASE_ARM64_T8103`, arm64, UID 501. SDK 26.5;
Apple Clang 21.0.0 (`clang-2100.1.1.101`); Rust 1.99.0
(`b940084d7 2026-09-28`), Cargo 1.99.0 (`5f94df478 2026-08-27`).
These are not macOS 26 results or clean-revision acceptance.

Raw result artifacts, command exits and source/binary digests are indexed under
[macOS research evidence](macos-managed-tree-evidence/index.json).

| Area | Current result | Missing work |
| --- | --- | --- |
| Production rejection | Existing focused test passed, 1 test. Probe observed five unsupported rejections without markers; positive Seatbelt control failed profile installation, so probe exits 2. | Working native positive control; absent markers alone are insufficient. |
| Ownership | Unsandboxed ordinary, setsid, setpgid and double-fork descendants acknowledged and produced delayed markers after worker exit. | Kernel assignment, adversarial rapid-reparent repetitions, authoritative membership, separately sandboxed descendants. |
| Cleanup | Diagnostic worker normal-exit survivor observations only; no candidate cleanup. | Error, cancellation, timeout, controller SIGKILL, worker/supervisor failure, authoritative emptiness and residual-resource accounting. |
| Aggregate limits | Unsupported production requests verified as rejections. | Candidate tree-wide time/output/task/memory controls, concurrent detached workloads, admission/overshoot semantics. |
| Authority | XPC generated-file and outside synthetic-file controls implemented. Bundle signs/verifies; connection fails before acknowledgement. | Successful XPC controls; TCP/UDP/Unix sockets, descendant authority, traversal/symlinks, descriptors, synthetic environment, authenticated broker and helper routes. |
| Usability | Diagnostic elapsed times recorded; signing/compilation exercised. | Cold/warm sandboxed startup, successful native build/generated-file control, sharing/toolchain cost and installation/cleanup accounting. |
| Harness | Malformed CLI and bounded-child configuration tests pass. | Classification coverage, service-unavailable/malformed-identity controls and complete result schema. |

No workload-limit value in this probe is an enforced ManagedTree ceiling. The
lineage delay is a bounded workload parameter; the rejection probe records
seconds, combined bytes, process count and memory bytes independently.

The current evidence supports preserving the strict gate and continuing the
investigation. It does **not** prove that every native design is impossible.
The conflict for #276 remains: documented sandbox authority and service
lifecycle do not themselves supply non-escapable pre-execution descendant
ownership, complete failure cleanup and aggregate admission/accounting. Choosing
weaker guarantees would be a platform-contract decision outside this probe.

## Repository validation

- Focused production rejection regression: 1 passed, nonzero filter confirmed.
- Example tests: 2 passed (malformed dispatch; bounded child configuration).
- Workspace/all-target/all-feature Clippy with warnings denied: passed.
- Workspace formatting and architecture: passed; architecture reports existing
  source-size review recommendations, not defects introduced by this probe.
- Runner all-target attempt: library ran 177 tests, 152 passed and 25 failed;
  example phase was not reached, so it was run separately. Native failures
  include profile-installation rejection and cascading expectations of support.
- Workspace/all-feature and separate integration tests were attempted. Build/link
  failed with ENOSPC. Removing generated worktree output and retrying with
  `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`
  still exhausted space. The subsequent focused runner/example build succeeded.
  These are failed validation attempts, not passing test receipts.
- A diagnostic `sandbox-exec -p '(version 1)(allow default)' /usr/bin/true`
  exits 71 with `sandbox_apply: Operation not permitted`. This isolates a native
  setup denial; the allow-default profile is solely an environment diagnostic.

Full test attempts and the runner suite precede the final double-fork fixture
addition; they provide no completed source-matched full-lane acceptance. The
final example tests, Clippy and native diagnostics are separately indexed.
No audit policy or test expectation was changed. Compilation cannot establish
macOS authority or lifecycle guarantees. An offline Linux validation worker can
complete portable repository checks with adequate storage, but cannot establish
macOS 26 or 27 behavior.

Next implement an explicit result matrix and the remaining bounded adversarial
workloads, diagnose XPC connection setup using native launch logs in an isolated
account, and run paired authority controls. Missing native authority does not
block implementation of those fixtures. macOS 26 acceptance and full native
matrix remain external execution requirements.
