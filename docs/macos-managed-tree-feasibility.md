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
| `cleanup` | Double-fork/session-changing leaf after normal exit, injected error, cancellation, timeout, worker SIGKILL and controller SIGKILL | Unsandboxed counterexamples to root-exit/tree-exit equivalence; supervisor failure explicitly unsupported. Harness root kills are not candidate cleanup. |
| `limits` | Fixed time, combined output, process and touched-memory inputs; one or three leaves, ordinary or session-changing with redirected streams | Sixteen workload controls, not enforcement. No configured aggregate candidate exists; every limit verdict is unsupported. |
| `authority` | Paired synthetic outside-file, symlink/traversal, TCP/UDP/Unix socket and environment controls, repeated through a child; public Restricted request | Positive fixture must work before interpreting denial. Setup rejection is unsupported, never a successful negative authority control. |
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

New research records use framed TOML (one document after each
`--- research case ---` separator). Fields distinguish setup, positive control,
requested/available/observed/enforced properties, threshold units, candidate
cleanup and harness rescue. Verdicts are `harness-failure`, `counterexample`,
`unsupported` or `unverified`; this observational harness has no supported/pass
verdict and cannot construct production receipts. A cleanup error takes
precedence over a counterexample. Missing markers never establish emptiness.

Cleanup leaves have 600 ms finite work; holding workers have 1500 ms finite work.
Readiness deadlines are two seconds, delayed-marker observation is bounded to
three seconds. Cancellation and controller termination kill only a held direct
child handle. Controller-death runs additionally allow the finite holding worker
to finish before removing fixtures; this wait is not authoritative cleanup.
These controls establish root/descendant separation, not candidate lifecycle.

Limit leaves allocate and touch at most 2 MiB, produce at most 8192 bytes on each
stream, and hold work for 200 ms. The controller launches at most three leaves,
waits at most three seconds and uses a direct-child kill/wait rescue guard.
Concurrent startup is requested, not proven simultaneous aggregate residency.
Memory acknowledgements report bytes explicitly touched, not RSS, committed
memory accounting or an enforced ceiling. Ordinary output is captured in one
shared file and checked for exactly 16384 bytes per output leaf; session-changing
controls redirect streams to null and acknowledge completion separately.
Requested comparison thresholds are 50 ms, 4096 combined bytes, two processes
and 4 MiB. No rlimit or candidate limit is installed by this command.

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
"$PROBE" cleanup
"$PROBE" limits
"$PROBE" authority
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

## Historical development evidence and gaps

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

## Recovered-workspace continuation

The historical evidence above is preserved unchanged. Current continuation
results and exact source scope are recorded separately in
[the recovered-run index](macos-managed-tree-evidence/recovered-run/index.json).
They are development evidence on macOS 27.0.1 build 26A434, not macOS 26
acceptance or a clean-commit receipt. Historical disk exhaustion is not a current
blocker: the continuation observed approximately 408 GiB available.

New implementation adds private structured records, acknowledged cleanup/failure
fixtures, bounded accounting workloads and paired root/child authority controls.
It extends the recovered probe and reuses public `execute`, request/policy
construction and shared temporary fixtures. XPC retains its separate packaging
and identity contract; no policy compiler or production backend is copied.

Six cleanup diagnostic controls observed delayed descendant writes after root
exit or termination; the supervisor case remains unsupported. Sixteen bounded
accounting workloads completed, including session-changing leaves with output
redirected. Those are working inputs for candidate evaluation, not measured
aggregate enforcement or proof of kill-on-owner-death.

The authority positive fixture exercises generated-file read/write, outside-file
read/write, symlink and parent traversal, loopback TCP connection, UDP bind, Unix
socket connection and an explicitly constructed synthetic environment in root
and child. Its initial Unix socket path exceeded Darwin's pathname limit; a
short shared-fixture label corrected that setup defect. The Restricted request
still rejects native profile installation before payload execution. Thus no
negative authority control or descendant containment was established.

The XPC bridge compiled with warnings denied, the bundle signed and verified,
and effective App Sandbox entitlements were captured again. Connection still
failed before worker acknowledgement. `/usr/bin/log show` returned
`Cannot run while sandboxed`, preventing startup diagnostics in this execution
environment. This does not identify a signing entitlement requirement and is
not evidence that App Sandbox/XPC is impossible. Apple's
[XPC service guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html)
was freshly retrieved to recheck bundle structure, signing and service startup;
it supplies service semantics, not authoritative descendant-tree semantics.

The final continuation example has seven passing tests; the focused production
rejection regression runs one test and passes. Formatting, workspace Clippy
with warnings denied, architecture checks and the separate integration workspace
pass (integration targets: one consumer test and four contract tests). The full
workspace command stops at CLI integration: 271 pass and five fail, each reporting
native profile installation failure. Earlier completed suites report 282, eight
and six passes. A separate runner run reports 152 passes and 25 failures,
including profile installation rejection and downstream support/receipt
expectations; the retained log includes every assertion. These are failed test
runs, not a complete passing full-lane receipt. No test, audit policy or
containment gate was weakened. Native macOS validation remains necessary even
if portable validation later succeeds in the engine's Linux worker.

Existing runner controls
`inherited_regular_connected_and_listening_descriptors_are_closed` and
`inherited_authority_does_not_cross_fork_exec_setsid_or_double_fork` were included
in that runner attempt; both fail during native setup. Reuse those fixtures
rather than duplicating their private sanitation logic in the example.
Remaining independently executable work includes a controlled authenticated
broker fixture, documented helper-route inventory,
rapid-reparent repetitions, native rlimit comparison controls and XPC
malformed/missing-service/timeout teardown coverage. Root/child authority
controls also need independent per-operation records and detached variants;
the current sequential payload stops on the first mismatch. Candidate-specific
ownership/accounting and negative authority evidence require a runnable native
candidate. macOS 26 remains a separate platform requirement. No evaluated
candidate yet establishes ManagedTree, and issue #275 remains incomplete.

## Broker, reparenting, RSS and transport continuation

This continuation extends the private research example; production containment,
public interfaces and strict rejection are unchanged. The preceding recovered-run
results remain historical. Current commands, exit codes, environment observations,
source hashes, binary hashes and raw logs are indexed in
[continuation-run/index.json](macos-managed-tree-evidence/continuation-run/index.json).
It is dirty-workspace development evidence, not clean-revision acceptance.

Additional commands after resolving `PROBE` from compiler-artifact metadata:

```sh
"$PROBE" broker
"$PROBE" rapid-reparent 8
"$PROBE" rlimits
```

`broker` runs a local Unix-socket fixture with four fixed 33-byte requests.
It checks `getpeereid` and a random per-run 32-byte capability, denies a wrong
capability, an ungranted operation and a truncated frame, and permits one fixed
synthetic write into a separate temporary project. Requests accept no paths.
The server has a three-second accept deadline, 250 ms stream timeouts and a
finite request count; the harness always joins it. These are same-process
synthetic clients, all with the same UID. They test the broker's protocol, not
identity separation between a sandboxed worker and a trusted broker. A capability
holder is intentionally authorized; UID alone is insufficient. No real service,
credential or privileged helper is contacted. The result is unverified for
containment even when all protocol controls complete.

`rapid-reparent` accepts 1–16 repetitions, rejects invalid counts before spawn,
and reuses the existing double-fork/exec and setsid leaf. Each repetition waits
for acknowledgement and a delayed marker after intermediate parents exit.
Eight repetitions completed in this run. These reproduce surviving descendants;
they do not measure a race-free ownership boundary or prove assignment before
untrusted execution. Marker evidence and finite self-expiry remain diagnostics.

`rlimits` compares three sequential fresh processes with no limit against three
fresh processes requesting `RLIMIT_RSS = 1048576` bytes. Each successful workload
touches 2097152 bytes and holds it for 200 ms. Limit changes occur only inside
the disposable child, with readback required after successful setup. This host
rejects the RSS setup with `EINVAL`; the explicit setup-stage result is unsupported,
not a memory enforcement result. The no-limit controls complete. The controller
records setup separately and does not change the test runner's or account's limits.
This does not implement CPU, per-UID process limits or inherited/concurrent RSS
comparisons, and it never equates touched bytes with resident-memory accounting.

The XPC bridge now sends a hello exchange and requires a typed acknowledgement
before dispatching the authority payload. `xpc-transport` runs four anonymous
endpoint controls through the packaged bridge, without registering a service:
a successful two-exchange round trip, malformed acknowledgement, withheld reply
and late reply after timeout. Rust requires results `[15, -1, -2, -2]`.
The callback owns its captured state and semaphore through timeout/cancellation;
it never writes through a pointer into the caller's returned stack. The first
silent fixture accidentally dropped its request, causing XPC's interruption reply;
retaining it through the deadline corrected that fixture. Current transport controls
pass. They do not authenticate or sandbox the anonymous endpoint. Run them with
the rebuilt bridge adjacent to the executable, as in the packaging instructions:

```sh
"$PROBE_APP/Contents/MacOS/Probe" xpc-transport
"$PROBE_APP/Contents/MacOS/Probe" xpc
```

### Helper-route inventory

| Route | Documented interface / bounded evaluation | Remaining uncertainty |
| --- | --- | --- |
| Bundled application XPC | Current signed research bundle and separate worker sandbox; anonymous transport controls pass | Named worker startup and its authority controls still require an execution-capable native environment; transport success alone proves no authority restriction. |
| Broader-authority Unix broker | Synthetic capability/UID fixture above | Separate, detached and sandboxed callers remain untested; no statement about system brokers follows. |
| Global Mach / NSXPC service | Apple documents connecting to advertised LaunchAgent/LaunchDaemon services with [`NSXPCConnection`](https://developer.apple.com/documentation/foundation/nsxpcconnection/init%28machservicename%3Aoptions%3A%29?language=objc). Its [sandbox exception reference](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/AppSandboxTemporaryExceptionEntitlements.html) documents global Mach lookup exceptions. | No arbitrary installed service was contacted, and no lookup exception was added. Effective denial of system-service requests remains unverified. These references do not establish every service's caller authorization. |
| Privileged helper / service registration | [`SMAppService`](https://developer.apple.com/documentation/servicemanagement/smappservice) controls bundled login items, agents and daemons. | No helper was installed, registered or invoked; no privileged fixture is available. Availability of this API supplies neither authorization nor a ManagedTree boundary. |
| Inherited descriptors and synthetic environment | Existing production descriptor controls and the probe's explicit synthetic environment | Native containment setup still prevents the negative controls. Do not duplicate the production descriptor sanitizer or treat clearing environment alone as descriptor isolation. |

The Apple helper references above were retrieved for this continuation. This is
an interface inventory, not an exhaustive audit of installed helpers or a claim
that every host service is safe. Testing unidentified real privileged services
would exceed the disposable fixture scope.

Independent coding remains: separate/detached broker clients, per-operation
and detached authority records, additional CPU/process/inherited-limit comparisons,
and missing-service transport controls. Native candidate membership, cleanup,
aggregate accounting and negative authority tests require successful candidate
setup. macOS 26 evidence remains unavailable. These gaps keep #275 incomplete;
no weaker mechanism is accepted as ManagedTree and no production worker is enabled.

The current run is macOS 27.0.1 build 26A434, Darwin 27.0.0 arm64, SDK 26.5,
Rust 1.99.0, UID 501. Ten example tests and the one focused strict-rejection test
pass. Workspace formatting, Clippy with warnings denied, architecture and the
separate integration workspace pass (one consumer and four contract tests).
Workspace tests stop at CLI integration with 271 passes and five failures after
282, eight and six passes in earlier suites. The runner suite reports 152 passes
and 25 failures. The CLI failures explicitly report native probe profile
installation failure; the runner log retains its setup and downstream receipt
assertions. These are failed runs, not passing native acceptance. Native probe
commands `rejection`, `authority` and named `xpc` exit 2; positive marker/profile
setup, contained negative authority and worker acknowledgement remain unavailable.
The local bundle compiles, signs and verifies, but those checks do not resolve
startup. Linux isolated validation can check portable code only. No validation
command is configured in this task's engine handoff, and no source-matched
isolated receipt is available. Remaining independent coding keeps this at a
partial implementation checkpoint; macOS 26 and capable native execution remain
later evidence requirements.
