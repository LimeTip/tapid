# Windows project-script networking without administrator setup

## Status and intent

Implemented design for the networking portion of issue [#157](https://github.com/LimeTip/tapid/issues/157), authorized on October 16. The standard-user broker and contained Node adapter now pass local native TCP/UDP/IPv6/DNS and real CLI HTTP controls. See docs/windows-tapid-run-scope.md for bounded evidence and remaining issue acceptance. The earlier spike below is historical feasibility evidence.

The user prefers a standard-user workflow. Administrator setup is acceptable only if necessary; requiring a person at the device to approve elevation would obstruct unattended agent workflows. The selected candidate therefore runs entirely as the invoking user, without a machine-wide service, firewall exemption, UAC prompt, custom Node build, or uncontained retry.

## Existing contract

- Preserve the Rust request/policy/grant pipeline, profile ceilings, invocation permission provenance, and the public CLI interface.
- The boolean network grant means unrestricted networking. This work does not introduce host/port scopes; application arguments are not authority limits.
- Keep AppContainer authority and Job ownership independent of compatibility hooks. Bypassing a hook must not obtain authority outside the effective policy.
- Keep Restricted and ManagedTree receipts distinct. Preserve CanonicalPath and trusted-host replacement assumptions.
- Keep standard Node unchanged. Reuse the embedded, byte-pinned compatibility component, suspended initialization, normal native-addon import coverage, and descendant setup.
- Dependency-hook attestation and complete toolchain identity remain with #273/#274. Network changes must coordinate with that boundary without claiming those issues complete.

## What native investigation established

Development environment: Windows 11 Home x64 build 26200; Rust 1.99.0 MSVC; ordinary non-administrator user. Source baseline: ca20a6100d283e7701bfca5b6e877f3c6e014512, plus the described disposable probe. This is not clean-commit Windows 11 Pro VM126 acceptance.

1. The existing network-enabled prelaunch rejection test passes. The existing native ManagedTree execution test also passes outside this session's filesystem sandbox. Inside that sandbox, profile creation fails with HRESULT 0x80070002; that is not a networking result.
2. A native probe through the production launcher can bind a loopback listener under network=false, while connecting to a reachable host listener times out. The identical host positive control connects. The blanket deny-loopback-bind acceptance item is therefore not satisfied. This does not prove inbound reachability, DNS denial, external-network denial, or universal network isolation.
3. An explicit SystemRoot value is needed by these native Winsock fixtures. Without it, the host positive control fails provider initialization with WSA error 10106. Such a failure must never count as containment denial. The new permanent native TCP denial test explicitly allowlists SystemRoot and checks the positive control first.
4. With no AppContainer network capabilities, an ordinary host process can use WSADuplicateSocketW to delegate a TCP socket to its contained Rust child. Real payload controls pass for an already-connected stream, an unbound socket connected by the child, and an unbound socket bound/listened by the child and reached by the host. No firewall exemptions are changed.

The last result supports a standard-user candidate. It does not establish production authentication, native-addon coverage, IPv6, UDP, DNS, external connectivity, cancellation, or compatibility with every Winsock provider or mitigation policy.

The scratch spike intentionally transfers authority outside the production request API. Its existing network=false receipt does not describe that extra test-only authority and is not network-policy acceptance. Its filesystem mailbox and caller-supplied PID are test scaffolding, never the production channel design.

Local evidence artifacts (generated and untracked): target/windows-socket-delegation-spike.rs, target/windows-socket-delegation-spike.exe, target/windows-socket-probe.log. Executable SHA-256: 75fc89018817403243c7bb8f9283aadccdee2ea83b8627eebc8c73814e4ded9e.

Replay the already-built artifact from the repository root in a normal user session:

~~~powershell
.\target\windows-socket-delegation-spike.exe --exact socket_delegation_feasibility --nocapture
~~~

The earlier standalone custom-launcher experiment failed process setup with error 203 and produced no networking evidence. The successful spike uses the production execute path instead.

## Selected candidate

### Authority and launch

For network=false, retain the no-network-capability AppContainer and create no broker channel. Retain explicit denial controls. Preventing local bind itself remains an unmet acceptance item and a blocker for declaring issue #157 complete. Broker development may proceed independently, but this design does not waive that item. Resolving it requires either native enforcement evidence or an explicitly reviewed change to the network-denial contract; isolated bind is not silently reclassified as a denial pass.

For network=true, prepare the network authority before initial project execution. Retain zero network capability SIDs and use the per-execution socket broker for network authority, including loopback. Verify the exact AppContainer identity, Job and zero capability set before transfer. If capabilities, broker setup, or compatibility initialization cannot be established, tear down the owned tree and fail before project spawn.

The broker runs inside the invoking Tapid process at the invoking user's privilege level. It creates only the IP sockets authorized by the effective network grant; it does not impersonate clients or gain administrator authority. The zero-capability token and broker delegation are disclosed in launch evidence. Token inspection alone is insufficient: test files and objects whose ACLs grant only a proposed network capability SID, outside declared filesystem/runtime authority. If adding that SID broadens non-network authority, reject that candidate and retain the no-capability broker path. Do not trade filesystem confinement for DNS or network compatibility.

### Private channel and recipient identity

Extend tapid-runner's existing Windows lifecycle and embedded-component initialization; keep implementation modules private. Prepare private request/reply pipe handles and duplicate the child endpoints into the verified suspended child. Handles are non-inheritable except for explicit verified delegation. Do not publish a globally addressable socket service or put broker capabilities in environment variables, command lines, or project files.

Each connection is bound by the host to a held process object, the execution's exact AppContainer SID, and its existing Job. The client cannot select a recipient by supplying a PID. WSADuplicateSocketW receives the PID derived from the held, validated recipient object. Revalidate that it is live immediately before transfer; acknowledge socket materialization before dropping the host's pending descriptor.

Child registration uses the existing suspended descendant setup. A parent can identify its newly created child by a handle in the parent's own handle table. The broker obtains a reduced-rights held process handle through that authenticated parent's channel, verifies process object type, exact AppContainer SID and Job membership, and creates a private channel for that child before it resumes. Reject host processes, other executions, stale handles, and children whose required initialization cannot be installed. The prototype mailbox/PID exchange must not survive into production.

### Socket requests and bounds

Use a versioned fixed-size native protocol with explicit request IDs and checked lengths. The first supported request class creates ordinary IPv4/IPv6 TCP or UDP sockets for the verified recipient. Validate family, type, protocol, flags and provider selection independently of the compatibility component. Reject raw sockets, unrelated protocol providers, AF_UNIX/filesystem endpoints, arbitrary handle operations, arbitrary paths and host process creation.

Support one pending request per private client channel and at most 64 live client channels per execution. Exhaustion produces an explicit resource/setup error; it never widens authority or silently falls back to host execution. Keep pending host socket ownership until acknowledgement or teardown. Host descriptors, channels and client registrations must have explicit owners and unwind safely on every failed transition.

The component adapts supported WSASocket calls through this channel and preserves normal Winsock error/overlapped/non-inheritance semantics. A version mismatch or unavailable broker returns an error. Dynamically resolved APIs or modules outside the existing hook coverage remain kernel-contained; their compatibility is not promised until tested. Do not claim that the hooks themselves enforce network denial.

### Environment and DNS

Preserve explicit environment allowlisting. Do not silently copy SystemRoot, proxy settings, credentials or the ambient environment. Initial supported configurations may explicitly allowlist SystemRoot and document that prerequisite. Before production support, prove the standard Node workload and native resolver work with the exact controlled environment. Provider initialization failure, DNS failure, a skipped test, or a missing runtime is not network-denial evidence.

Network=true must retain its unrestricted-network meaning, including name resolution; local TCP alone is not enough to enable that policy. Test UDP/IPv6 and controlled resolver behavior before claiming the corresponding configuration. Any required resolver mediation must be specified and reviewed as a separate broker operation rather than added implicitly to socket creation.

### Lifetime, evidence and failures

Tie broker ownership to WindowsExecutionLifecycle and the existing verified-empty-Job cleanup boundary. Keep channels serviced while owned children need them; cancellation stops new grants, terminates/drains the Job, then closes pending transfers/channels before ACL/profile restoration. Broker failure cannot trigger an uncontained retry. Preserve Restricted best-effort versus ManagedTree complete-cleanup reporting.

Receipts must identify the broker mechanism, its per-execution authority, actual token capabilities, component identity, deliberate delegated channels, coverage exclusions and exact policy. Completion evidence reports observed lifecycle cleanup only. Do not enable network=true until both positive and denial controls pass through the real CLI on the claimed native target.

## Implementation sequence

1. Preserve the native root/descendant TCP denial regression and document the newly observed bind and environment limitations. This is the current reviewable change.
2. Implement the bounded broker protocol, private channel ownership and recipient registration with wrong-token, wrong-Job, wrong-object, stale-handle, malformed-frame and exhaustion tests. Keep the production network gate closed.
3. Integrate socket creation into suspended root/descendant initialization and application/native-addon import coverage. Use unchanged official Node for bidirectional TCP, IPv6/UDP, DNS, IPC and error controls. Keep the production gate closed until the effective policy can be demonstrated.
4. Verify cancellation, broken channels, failed initialization and full Job cleanup; verify no-network invocations receive neither capability SIDs nor broker handles. Add explicit receipts and diagnostics.
5. Enable only natively verified configurations, then run exact-commit news-site build/test/start on Windows 11 Pro VM126, recording runtime versions, artifact hashes, readiness, argv/output/exits and cleanup. Publish the supported matrix at that level only.

## Alternatives

Microsoft documents separate loopback exceptions, including a continuously running administrator tool for inbound server access. Changing global exemptions or depending on that tool is not the selected unattended workflow. A privileged installed helper remains a fallback to reconsider only if the standard-user broker cannot satisfy compatibility and authority controls; it is not installed or requested by this design.

References: [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation), [loopback restrictions and exemptions](https://learn.microsoft.com/en-us/windows/apps/develop/communication/interprocess-communication), [WSADuplicateSocketW](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsaduplicatesocketw), [WSASocketW](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsasocketw).


## Verification of the current change

The affected runner suite passes with TAPID_TEST_NODE explicitly set to the selected Node executable: 193 library tests (two existing ignored diagnostics), 24 containment tests, four windows_network tests (including two shared ACL-fixture tests and its helper), three review regressions, and seven doctests. Total: 231 passed, zero failed, two ignored. Runner Clippy with all targets and warnings denied, formatting, and whitespace checks pass. The first suite invocation omitted the mandatory Node prerequisite and failed its Node-dependent tests; the corrected invocation passes. Logs are retained locally in target/issue-157-runner-tests-with-node.log and target/issue-157-clippy.log. Node-dependent checks used Node 22.23.1; the socket-delegation spike itself uses Rust only.

A fresh Windows design review found no serious correctness gap in the native root/descendant TCP-denial regression. Its consequential question was the unmet loopback-bind criterion; this design explicitly retains that criterion as a blocker for closing #157 rather than waiving it.

## October 16 implementation record

The implementation retains zero capability SIDs for every policy, rejecting the optional capability-SID approach above. It reuses the existing suspended-root/descendant initialization, application import checks, LoadLibrary hooks, Job cleanup and explicit environment construction. The shared fixed wire codec is compiled into both the host and embedded component. A single nonblocking owner thread services at most 64 client channels; socket transfers expire after ten seconds without acknowledgement. Native adversarial controls cover wrong Job/SID/object and stale targets, malformed acknowledgements, backpressure, expiry and exhaustion.

A fresh security review found a local invalid-socket sequence consumption bug. A native addon regression failed before the fix and passes after validation was moved before sequence allocation; valid sockets and descendant registration still work after rejection. The existing addon compiler was reused through shared test support. The host TCP positive control now explicitly restores accepted sockets to blocking mode before bounded reads, avoiding Windows inheritance of listener nonblocking mode.

The initial verification section above records the pre-implementation baseline only. Current logs are target/issue157-cli-run.log, target/issue157-workspace-clippy.log and target/issue157-workspace-tests.log. Local HTTP tests and controlled DNS use no external service. The standard-user implementation needs no administrator helper, but exact-commit Pro VM126 consumer acceptance and the no-network local-bind criterion still prevent closing the full issue.
