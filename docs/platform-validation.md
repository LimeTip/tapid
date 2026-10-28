# Root-script platform validation

Consumer CI tests the latest release of every supported Node.js major (26, 24, and 22) on Linux, macOS, and Windows. The matrix specifies majors only with `check-latest: true`. Support follows Current, Active LTS, and Maintenance LTS status in the [official Node.js release lifecycle](https://nodejs.org/en/about/previous-releases), excluding end-of-life versions and prereleases. Update the matrix when release lifecycle status changes. This configuration does not establish execution evidence for the expanded matrix.

ADR 0005 CLI wiring, experimental macOS Restricted containment, and a Linux Restricted Landlock/seccomp backend are integrated. Native behavioral controls gate support, and accepted executions produce checked enforcement receipts. Linux ManagedTree and resource limits are implemented with delegated cgroup v2 and private PID/mount namespaces; exact-commit acceptance remains pending. macOS ManagedTree and resource-limit profiles remain unsupported. Windows read-only AppContainer/Job Object execution and graceful Ctrl+C have pre-integration native Windows 11 and hosted validation; this branch adds existing declared-target writes with limited-account native CLI acceptance, and standard-user Node networking via a private socket broker. Missing/unsafe targets and networking without the selected Node component fail closed. Native Home development controls cover TCP, UDP, IPv6, controlled and native hostname DNS, real CLI HTTP readiness and the original news-site build/test/start; the no-network local-bind criterion remains pending administrator-installed guard acceptance on this physical device. Exact merged-revision Windows verification remains pending; legacy inheritance bookkeeping and non-atomic ACL mutation limitations are documented in windows-tapid-run-scope.md. Targeted Ubuntu 24.04.5 x86_64 local-VM Restricted consumer and runner tests passed on code commit `3dd67f75c8d6f4187f8f1145f4a615939472e1c4` (kernel `6.8.0-142-generic`, `/bin/sh` dash `0.5.12-6ubuntu5`, Node `v22.23.3`, Rust build toolchain `1.97.1`; Tapid artifact SHA-256 `20d77eff052cb589a2c0ad002d9d47a6263c12f05171519b2c3d8f72e30ddcaa`). The VM was local-only, not registered as a GitHub runner. Separate GitHub Actions run [37115198876, attempt 1](https://github.com/LimeTip/tapid/actions/runs/37115198876) at the same code commit passed all hosted jobs and did not target the VM. This targeted local evidence does not complete the full common Restricted probe matrix below. Local dirty-tree results are development evidence, not a clean-commit platform acceptance record.

A platform may be marked Restricted only after the Restricted probe set passes through `tapid run <SCRIPT> -- <ARGS...>` at the exact integrated commit. ManagedTree requires the Restricted probes plus the ManagedTree-only probes. A unit test of policy declarations, backend availability, compilation, a generated native profile, or a successful allowed operation is insufficient.

## Evidence record

For each operating system, architecture, backend, and assurance level, retain:

- the full 40-character commit from `git rev-parse HEAD`, with a clean tracked tree and the commit containing the runner backend, configuration parser, CLI wiring, probes, and documentation;
- the workflow URL and immutable run/job identifiers, attempt number, runner image/version, OS build or kernel version, architecture, shell/runtime versions, and Rust toolchain;
- the built `tapid` artifact digest and logs that identify the same commit;
- the exact checked-in `tapid.toml`, fixture scripts, commands, exit codes, stdout/stderr, checked launch and completion evidence, and pass/fail result for every probe;
- evidence that human and machine-readable launch evidence agree about each dimension's request, mechanism, assurance level, declared/observed/enforced state, scope, and limitation, and that enforcement exactly matches the request;
- completion evidence limited to lifecycle and cleanup results, without re-confirming launch-only authority;
- the backend identity, native primitive versions or feature probes, and any deprecation, path-binding, broker, delegation, or best-effort lifecycle limitation.

`CanonicalPath` evidence proves only fresh pathname resolution for setup and retains its documented host-race assumption. It must never be recorded as `NativeObject` unless the backend holds and revalidates a native object identity.

Do not update platform status from a run against a merge commit, rebuilt artifact, or fixture revision different from the recorded commit unless that exact revision is named as the evidence target. Re-run the applicable matrix after any change to the backend, policy compiler, process supervision, CLI wiring, fixture, probe assertion, or receipt schema.

## Common Restricted probes

Every filesystem and network category that a backend claims to enforce needs a positive control proving the fixture can perform an operation when granted and a negative control proving the same operation is denied when not granted. If a requested dimension is unsupported, the acceptance test must instead prove failure before spawn with no child marker; that does not count as support for the dimension. Negative probes must also confirm a nonzero result and checked launch evidence matching the exact request; a crash, missing dependency, malformed command, or skipped test is not a containment pass.

### Filesystem

- **Positive:** read a declared project file and create, modify, and remove files in each declared `write` path.
- **Negative:** deny writes to an undeclared project path, a sibling or parent path, the user home, and an operating-system temporary path. Deny reads outside declared project/runtime paths. Repeat escape attempts through `..`, absolute paths, symlinks, and descendants.
- Confirm the minimal shell/runtime files are available without turning their parent trees into broad writable grants.
- Record each path grant's declared kind and binding evidence. Exercise path replacement races where the mechanism is path-based and report residual uncertainty rather than upgrading it to native-object enforcement.

### Network

- **Positive:** with `network = true`, bind and connect on loopback and attempt an external connection when the test environment permits it. The receipt must state that the current boolean grant is unrestricted networking.
- **Negative:** with `network = false`, deny loopback bind, loopback connect, external connect, and name resolution while a local control endpoint proves the test network is otherwise reachable.
- Pass `--hostname 127.0.0.1 --port 3001` to an application and verify those arguments do not alter the Tapid receipt. They are application behavior, not host/port policy.
- Future declared listen/connect scopes or brokered ports need separate positive, negative, identity, bypass, and descendant probes before they can be claimed.

### Environment, PATH, and inherited state

- **Positive:** list one benign variable in `environment` and verify its exact caller value reaches the script when present; verify the controlled `PATH` resolves the managed project executable and required shell/runtime.
- **Negative:** inject unique sentinel values into unlisted variables representing cloud credentials, package tokens, proxy variables, `HOME`, SSH/GPG/agent sockets, loader variables, and arbitrary secrets; verify neither the script nor descendants can observe them. Verify absent allowlisted variables are not invented.
- Enumerate inherited descriptors or handles so removing a variable does not leave its referenced credential channel open. Verify the backend closes or explicitly supplies every child descriptor/handle.

### Forwarded arguments

- Pass empty strings, spaces, quotes, Unicode, leading dashes, shell metacharacters, and multiple ordered values after `--`; verify the fixture receives the exact argument vector in order and that Tapid does not parse them as its own options or containment policy.
- Run an equivalent invocation without `--` that should be rejected by CLI parsing, proving the probe tests the documented separator contract rather than accidental shell behavior.

### Descendant authority propagation

- With `subprocess = true`, start the required platform shell, a child Node process, and descendants that detach, re-parent, or create a new session. Verify each retains the same filesystem, network, environment, and descriptor restrictions.
- With `subprocess = false`, verify an attempted child does not start when the backend claims that restriction.
- Restricted provides no cleanup guarantee. If the backend attempts best-effort lifecycle supervision or cleanup, completion evidence must identify which descendants were observed or controlled, what cleanup was attempted, and where races or escape uncertainty remain. A delayed surviving marker fails any claim of complete cleanup but does not by itself disprove launch-time authority propagation if the surviving process remains natively restricted.

### Restricted limits and lifecycle reporting

- Exercise each configured timeout, output, process, and memory limit and record whether it applies to the initial process, observed descendants, or a complete native tree.
- Do not describe a process-local or best-effort aggregate limit as tree-wide. If policy requires a scope the backend cannot establish, verify failure before spawn.
- Exercise normal completion, cancellation, timeout, and Tapid termination. Restricted completion evidence must distinguish no cleanup guarantee from best-effort cleanup actually attempted or observed, must report uncertainty, and must not claim ManagedTree or re-confirm launch-only authority.

### Fail-closed startup and receipts

- Corrupt or remove the required backend primitive, request an unsupported combination, and use invalid or unknown `tapid.toml` fields. Verify no script or descendant marker is created.
- Verify failure to establish a required filesystem, network, environment, descriptor/handle, subprocess, or limit dimension aborts before untrusted code starts; partial setup must be torn down.
- Compare human and machine-readable output for the same run. Reject launch evidence unless the exact requested enforcement is present. Reject any evidence that labels a requested, declared, or merely observed capability as enforced, reports `CanonicalPath` as `NativeObject`, omits mechanism, scope, or limitation, or uses completion results to re-confirm launch-only authority.

## Additional ManagedTree probes

ManagedTree must pass every common Restricted probe and all of the following:

- prove every descendant is assigned before it can execute outside a kernel- or VM-owned boundary; attempt detached, double-forked, rapidly re-parented, session-changing, and Windows breakaway children;
- verify complete cleanup and kill behavior after normal completion, cancellation, timeout, Tapid termination, and applicable supervisor crash or recovery scenarios;
- confirm membership through authoritative namespace, cgroup, Job, or VM state rather than PID scans or process-group inference;
- verify a delayed descendant cannot survive to write a marker after the supervisor reports completion;
- prove timeout and output accounting covers the complete tree and terminates it according to the documented contract;
- prove `max_processes` prevents the next process across the complete tree without an assignment race;
- prove `max_memory_bytes` accounts for and stops the complete tree at the configured boundary;
- request each unsupported ownership or tree-wide limit dimension and verify failure before spawn with no child marker.

## Unsandboxed-path probes

A future `--no-sandbox` path requires separate tests. It must be explicit and prominent, produce an unsandboxed outcome with no enforcement receipt, never replace failed Restricted or ManagedTree setup silently, and be rejected in unattended mode unless a separately approved authorization mechanism is present. The current CLI does not implement this path.

## Platform-specific gates

### macOS 26

The Restricted backend is experimental Seatbelt installed by deprecated `sandbox-exec`, with path-based parameter grants and a current-executable private helper. Native syscall probes also use private Seatbelt APIs. Startup probes sample generated deny-default filesystem/network controls, descendant writes, explicit environment, descriptor hygiene, and exec replacement. Each negative syscall control requires permission denial and its own successful positive control. They require no Ruby, Python, compiler, or network service. These samples are support gates, not an exhaustive proof of Seatbelt behavior. Broader development tests may use system Ruby and must fail rather than skip when it is absent. Native acceptance must still be rerun at each exact commit being claimed.

Native ManagedTree remains unsupported. Process groups are escapable with `setsid` and `setpgid`; Darwin has not supported recursive `EVFILT_PROC` tracking through `NOTE_TRACK`, `NOTE_TRACKERR`, or `NOTE_CHILD` since macOS 10.5; and `NOTE_FORK` plus process-table or `p_puniqueid` scans retains a rapid double-fork/intermediate-exit race. An optional strict Linux VM through Virtualization.framework is a separate future backend that changes platform, startup, filesystem-sharing, and network semantics; it must not be reported as native macOS ManagedTree.

### Linux

The implemented Restricted design combines Landlock filesystem rules, `no_new_privs`, seccomp, and explicit environment/descriptor construction. Runtime support depends on kernel feature probes. Record the Landlock ABI and handled access rights, seccomp policy, privilege transition, network mechanism, and unavailable features. On code commit `3dd67f75c8d6f4187f8f1145f4a615939472e1c4`, the targeted Ubuntu 24.04.5 x86_64 local-VM run passed all 99 runner-library tests and the Linux native Restricted consumer fixture with Node `v22.23.3`; the consumer verified install, lifecycle suppression, child output, exact argument forwarding, environment handling, and exit codes. A separate `network = false` loopback socket attempt returned `EPERM`. The exact Tapid CLI artifact SHA-256 was `20d77eff052cb589a2c0ad002d9d47a6263c12f05171519b2c3d8f72e30ddcaa`. The VM was local-only, not a GitHub runner. A system that cannot establish a required dimension must fail before spawn. The broader common Restricted probe matrix remains outstanding; this evidence does not establish ManagedTree or resource-limit support.

ManagedTree additionally requires proven namespace ownership and cgroup delegation. Record namespace membership, cgroup version/controllers/delegation, assignment ordering, cleanup ownership, and tree-wide accounting. Running inside a container or cgroup does not itself prove Tapid owns the boundary.

### Windows

The Windows backend creates a no-network-capability AppContainer, applies declared-target-only ACL grants, and assigns a suspended child to a non-breakaway Job before resume. Existing ordinary inherited/unlabeled limited-account project files/directories pass native Windows 11 build 26300 CLI mutation acceptance without integrity-label lowering. Missing targets, unsafe reparse/hard-link topology, protected descendants and unavailable host ACL authority reject before any grant or spawn. Exact-file writes do not permit replacement or sibling creation; subtree deletion rights apply only to descendants, never its root. Receipts preserve CanonicalPath and its trusted-host race assumption, not NativeObject. Explicitly medium-labeled targets and missing-directory materialization are not supported. Explicit Node network grants use the standard-user socket/DNS broker; grants without the selected Node compatibility component fail closed. `network = false` requires the administrator-installed guard; native guard acceptance remains pending. Restricted completion remains best-effort; ManagedTree separately proves Job-owned tree completion and configured resource dimensions. Legacy inheritance metadata and non-atomic external ACL-writer limitations remain; full news and broader common matrix acceptance is outstanding.

ManagedTree additionally requires a non-breakaway Job Object assigned before untrusted execution, with complete cleanup and configured tree-wide limits. The runner currently uses Job Object membership, kill-on-close, active-process and job-memory limits, and process-tree completion polling; native Windows 11 VM runner tests cover representative cases. Probes must still detect breakaway children, inherited handles, path variants, broker escapes, and assignment races. Incomplete AppContainer/LPAC or Job setup must fail before spawn.

## Current status

| Platform/backend | Restricted status | ManagedTree status | Native evidence |
|---|---|---|---|
| macOS 26 native Seatbelt | Experimental Restricted support; profiles with resource limits fail closed | Native support unavailable | Native macOS 26 behavioral suite and exact CLI HTTP acceptance |
| Linux Landlock/`no_new_privs`/seccomp | Implemented for Restricted; runtime capability-dependent; targeted Ubuntu 24.04.5 x86_64 local-VM tests pass on code commit `3dd67f75c8d6f4187f8f1145f4a615939472e1c4`; broader common probe matrix remains pending | Implemented with delegated cgroup v2 and private PID/mount namespaces; clean-commit acceptance remains pending | Exact-commit local VM: 99 runner tests and native Restricted consumer validation pass; hosted Linux checks pass; broader probe matrix not complete |
| Windows AppContainer plus Job Object | Read-only Node execution implemented and checked on pre-integration Windows 11 VM126 and hosted windows-latest; existing ordinary declared-target writes are supported; missing/unsafe targets and network grants without the selected Node compatibility component fail closed; explicit Node grants use the socket/DNS broker; `network = false` requires the administrator-installed guard, with native acceptance pending | Job ownership, descendant timeout/output controls and graceful Ctrl+C cleanup have targeted native tests; the complete common assurance matrix remains pending | Windows 11 containment suite 19 passed, 1 intentionally ignored before main integration; full hosted CI passed at `2d7d0e2d18a564d495035984152dddd4eec9e9a0`. Exact merged-revision acceptance remains required; see windows-tapid-run-scope.md for limitations |
| Strict Linux VM through macOS Virtualization.framework | Optional future backend with Linux VM semantics | Future investigation | No implementation or evidence recorded |

Keep package-manager, installer, CLI preflight, Restricted enforcement, and ManagedTree evidence separate. A local result on one platform or assurance level is never evidence for another.

PR #220 handoff checks on an uncommitted Windows 11 Home x64 build 26200 worktree pass the existing-target runner and CLI controls, including volume/share-root rejection, explicit integrity-label rejection, effective child-token write checks and rollback. Real news-site install/frozen/offline replay succeeds, but the original build/test scripts remain blocked by ancestor metadata access and Node IPC compatibility. This is development evidence, not complete Windows 11 Pro acceptance; see [the native handoff record](windows-tapid-run-scope.md#october-12-native-handoff-validation). At that historical PR #220 checkpoint, network/start was a prelaunch rejection; current explicit Node network grants use the socket/DNS broker described above.

The reserved-Node lifecycle regression launches a detached descendant with redirected stdio, returns a root receipt, waits for private-directory removal with a bounded retention control, then invokes bare `node` with hostile project `.bin/node` present. The hostile marker must remain absent and the byte-verified private runtime snapshot must execute. A separate benign-marker regression proves project writes cannot hard-link or mutate the snapshot or selected runtime, and that the two use distinct inodes. Subprocess-enabled receipts must disclose the retained snapshot with `cleanup_observed = false`; failed pre-exec launches must remove it, and subprocess-disabled successful cleanup requires native fork denial and observed root exit. Retained snapshots are never reused or deleted on a timer. Temporary-storage retention lasts until OS cleanup or host removal; removal during descendant survival ends the binding guarantee, and host writes or races after final validation remain outside Restricted containment.

## Linux ManagedTree development validation

Issue 155 adds a separate managed backend. Local uncommitted-tree tests use Linux x86_64 kernel 6.8.0-117-generic in an isolated container with explicit namespace privileges and delegated memory/pids controllers. `TAPID_REQUIRE_MANAGED_ASSERTIONS=1` requires real native execution of the managed tests rather than treating unavailable host setup as acceptance. All 132 runner-library tests passed, including real timeout/output/process/memory enforcement, read-only metadata denial, detached-descendant cleanup, caller-crash cleanup, clone3/Unix-socket escape controls, and rejection of writable kernel-control filesystems. The CLI native fixture passed generated-file creation, GCC compilation, credential non-inheritance, immutable source storage, authenticated offline/frozen replay, forged-output rejection, changed-policy rejection, and failed-hook rollback. The build used Rust 1.99.0 and a temporary SHA-256 optimization override; execution used Node 20.19.2 and GCC 14.2.0. These results are development evidence, not a clean-commit acceptance record or a completed common probe matrix. See [ADR 0008](adr/0008-dependency-lifecycle-derived-trees.md) and [dependency lifecycle scripts](dependency-lifecycle.md).

On a Linux host with the documented namespace privileges and cgroup delegation, require the native assertions with:

```text
TAPID_REQUIRE_MANAGED_ASSERTIONS=1 node --experimental-strip-types scripts/dev.ts test -p tapid-runner --lib --locked
TAPID_REQUIRE_MANAGED_ASSERTIONS=1 node --experimental-strip-types scripts/dev.ts test -p tapid --test cli --locked approved_dependency_lifecycle_builds_native_output_and_replays_exactly
```

## Dependency execution override development validation

The uncommitted override implementation has native macOS arm64 Darwin 27.0.0
development coverage. Five CLI lifecycle regressions pass, including independent
approval/containment overrides, real shell-generated output, JSON risk warnings,
exclusion from the authenticated cache, source-only reconstruction on a later
normal install, required implicit native-build rejection, and failed-hook
rollback. Two runner regressions verify controlled environment/PATH, null stdin,
root timeout and root output checks without enforcement receipts. These checks
do not establish ManagedTree containment or descendant cleanup for the unsafe
path. See [ADR 0009](adr/0009-explicit-dependency-execution-overrides.md).

The runner cross-compiles for `x86_64-pc-windows-msvc`. Full Windows CLI
cross-compilation is blocked locally by missing Windows C headers required by
`ring`. Native Windows execution remains an acceptance gate; the unignored CLI
regressions run in the existing Windows workspace test matrix. Neither
cross-compilation nor uncontained shell execution establishes contained Windows
dependency support. Native private-copy write containment and owner-only Windows lifecycle keys
now have focused acceptance evidence below. Integrated authenticated Windows
dependency builds remain gated on toolchain identity and CLI acceptance.

```text
node --experimental-strip-types scripts/dev.ts test -p tapid --test cli --locked dependency_lifecycle_
node --experimental-strip-types scripts/dev.ts test -p tapid-runner --lib --locked uncontained
```

## Windows dependency authority closure checkpoint (#273)

Authenticated dependency builds remain disabled on Windows at
`dependency_toolchain_identity`. The following inventory describes source
mechanisms and unresolved inputs, not native acceptance. No complete Windows
runtime identity or prepared execution authority exists yet.

The existing native Windows dependency acceptance step runs the runner library's
`authority_tests` with `--show-output` before the general workspace suite, alongside
the separate `windows_containment` target. It retains each command's exit status
and fails if any command fails. This ensures an earlier CLI failure in the
workspace suite cannot prevent the paired registry/provider/COM controls and
host-supervisor regressions from running. Their successful status validates
sampled controls; it does not resolve the inventory below or enable authentication.

| Input channel | Current mechanism / concrete input | Required closure and evidence |
| --- | --- | --- |
| All Application Packages | LPAC opt-out and actual suspended-token access checks; explicit per-profile project grants | The root/descendant Rust probe attempts an AAP-granted copied executable, reads, and absolute/searched DLL loads. Host controls validate the executable and DLL; approved copies load inside containment. Undeclared operations must fail with `PermissionDenied`. Native execution of these extended probes is pending. |
| All Restricted Application Packages | LPAC does not establish exclusion of this separate trustee | The shared isolated fixture grants `S-1-15-2-2` read/execute access and records each root/descendant read, launch, absolute DLL load and searched DLL load as allowed or denied. This is characterization, not a denial gate. Native observations are pending; deny or bind the complete accessible set before authentication. |
| Profile storage | `WindowsAppContainer::create` creates a unique profile and fails on creation error; explicit cleanup deletes it after execution | The native cross-run control derives each profile folder from the actual child token, writes and rereads a private sentinel, requires its removal after completion, and checks that a second execution gets a different folder and cannot read the previous sentinel. Native execution is pending. Full initial contents and deletion-failure controls remain unresolved. Unique names alone do not identify initial profile bytes or registry state. |
| Registry | Launch installs `registryRead`; actual-token validation permits that exact enabled capability | Investigate socket-provider initialization without it. Enumerate necessary values and referenced files; mutable unbounded reads prevent authentication. No registry-value identity is currently computed. |
| COM / brokers | Paired host/LPAC in-process ShellLink and disposable running Rust local-server activation probes; no complete activation or response inventory | Native observations are pending. The local-server control checks dispatch counts and exact registration revocation, but covers host SCM launch separately, but does not cover LPAC-triggered SCM launch or arbitrary brokers. Deny undeclared operations or bind each required operation's complete inputs; arbitrary host responses cannot be a trusted OS assumption. |
| Inherited handles | Process attribute handle list restricts inherited standard handles; compatibility delegates exact metadata handles and EOF stdin | Preserve the explicit handle list and test bypass paths. A metadata handle is not permission to consume arbitrary file bytes. |
| KnownDLLs / API sets | Windows loader mappings and system runtime are available to the process | Bind mappings, accessible runtime bytes, architecture and OS/backend identity. OS version alone is insufficient. |
| Side-by-side resolution | Loader may consult manifests and assembly configuration | Bind selected assemblies, manifests, candidates, absence states and precedence; reject unsupported mutable sources. No complete inventory is implemented. |
| Dynamic DLL loads | Native token/ACL checks apply; compatibility interception is incomplete | Add absolute, searched, altered-search, secondary-dependency and manual-load controls. Static imports and observed loads cannot establish closure. |
| Descendant executables | Suspended root is verified and assigned to a non-breakaway Job before resume; token restrictions propagate | Extended AAP fixture pairs approved root/descendant execution with undeclared executable denial. Dynamic process API and DLL-initialization bypass controls remain outstanding. |
| Declared tools / runtime snapshots | CLI verifies declared tool bytes; Windows system identity is rejected before authenticated lookup | Retain the same bounded authority for hashing and execution. Secondary-byte, resolution-order and mutation controls must precede enabling recipe reuse. |

Kernel enforcement and privileged-host integrity are trust assumptions. They do
not cover mutable user registry state, undeclared application bytes, or arbitrary
broker responses. The unresolved rows block runtime identity integration, not
further fixture development. The extended AAP control is owned by the existing
native Windows workspace test suite and requires its guard setup. Compilation
or a zero-test macOS invocation is not an enforcement receipt. This checkpoint
does not establish #157 project-script acceptance or #274 integrated adoption.

The nonignored `windows_inventories_all_restricted_application_packages_authority`
test emits `ARAP:<process>:<operation>:allowed|denied` observations through the
native workspace lane's `--show-output`. Its passing status validates the probe
and approved controls; it does **not** certify ARAP exclusion. Any allowed result
is an input outside the declared project/tool set and must be resolved before
prepared authority or recipe integration. The AAP test retains strict denial
assertions. Both tests reuse the Rust native addon fixture and resolve its export
without calling Node APIs. Searched loads change `SetDllDirectoryW` only inside
isolated helper processes and restore the default search path afterward.
Both root and descendant helpers also resolve `LoadLibraryW` from the already
loaded `kernel32.dll` using `GetProcAddress`, then attempt the same absolute DLL
load through that function pointer. Host and approved contained loads must
succeed through this path; AAP-only loads must fail with access denial. ARAP
results remain characterization and emit `resolved-load-library` observations.
The fixture borrows the kernel module handle, following the
[GetModuleHandleW ownership contract](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-getmodulehandlew),
and releases only successfully loaded fixture modules. This exercises dynamically resolved loader calls without
relying on compatibility interception; native Windows execution is still pending.
The same host, approved, root and descendant controls also call `LoadLibraryExW`
with `LOAD_WITH_ALTERED_SEARCH_PATH`, and separately with
`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`, using absolute
fixture paths. Both modes perform executable-image loads and validate the fixture
export before releasing the module. They retain strict AAP denial assertions and
ARAP characterization, with separate `altered-search-library` and
`restricted-search-library` observations. Native execution remains pending; these
flags do not establish closure of secondary dependencies or loader search order.
These sampled controls do not cover secondary DLL dependencies, manual mapping,
side-by-side assemblies or arbitrary broker inputs.

The ambient DLL controls also check `GetModuleFileNameW` against the expected
canonical fixture path before accepting a successful load. An export name alone
cannot distinguish two valid fixtures with the same export. Host, LPAC root and
direct descendant controls switch `SetDllDirectoryW` between two approved
subdirectories containing the same DLL basename, then switch back. Each load
must select the expected path and release its module before the next switch;
the parent requires both contained search-order observations. The undeclared DLL
shares that basename, so the subsequent AAP denial controls also reject reuse of
a previously approved module when loading the undeclared path. These checks reuse
the existing Rust DLL fixture and explicit project grant. They characterize
resolution changes, without claiming held-object identity, complete loader closure
or recipe binding. Native Windows execution remains pending.

`windows_profile_storage_is_fresh_and_not_readable_by_the_next_execution` uses
two executions of a private Rust fixture through the existing runner interface.
The fixture queries `TokenAppContainerSid` and `GetAppContainerFolderPath` rather
than trusting an environment variable for the storage location. It requires own
profile write/read success and prior-sentinel absence or access denial, while
the host checks sentinel removal after kernel-owned cleanup. The sentinel name
is fixed only within each fresh OS-owned profile; no existing user profile or
credentials are read. This is a sampled storage control, not an inventory of all
profile initial bytes, registry state, or cross-profile broker authority. Native
Windows execution with guard setup is required before claiming these assertions
hold.

`windows_socket_provider_initialization_with_registry_read_capability` adds a
paired host/contained Rust control for the current `registryRead` launch. It
requires Winsock 2.2 initialization and probes IPv4/IPv6 TCP socket creation,
without binding, connecting, or resolving a hostname. Host socket creation must
succeed; contained creation is recorded as successful or permission denied, and
other provider errors fail the control. Socket creation alone does not establish
network access or denial. Native results are pending. This baseline does not
enumerate readable registry values or justify removing `registryRead`.

The nonignored runner library control
`lpac_provider_initialization_compares_registry_read_and_capability_free_tokens`
adds the capability-free comparison through the same private suspended-launch
implementation. Production launch entry points still require `registryRead`.
The control copies the Rust test executable into a disposable `TempProject`,
proves host socket creation, applies the existing explicit filesystem grants,
and checks the actual suspended token has exactly one or zero capabilities.
Both variants retain LPAC access verification and Job assignment before resume,
bounded output/time, empty-Job confirmation, grant restoration and profile cleanup.
It reuses the baseline's IPv4/IPv6 provider fixture; only the capability-free
variant additionally records `WSAEPROVIDERFAILEDINIT` as
`socket-provider-unavailable`. That diagnostic is a compatibility obstacle,
not successful socket initialization or an authority-denial acceptance claim.
Unexpected failures still fail the control. No bind, connection or DNS operation
is performed, and this low-level probe does not issue a containment receipt or
exercise the network guard/broker. Native Windows observations remain pending.
Run through `scripts/dev.ts test -p tapid-runner --lib --locked
lpac_provider_initialization_compares_registry_read_and_capability_free_tokens -- --show-output`
on Windows. A successful compile or a zero-test invocation on another platform
cannot decide whether the capability is removable. Disposable registry-value
controls, broker closure and complete runtime binding remain implementation work.

The library capability comparison also creates a disposable DWORD input in
`HKEY_USERS/<host-token-user-SID>/Software/TapidAuthority-<TempProject-name>`.
The host helper must read the expected value before either contained launch.
Both token variants query that same absolute host-hive path, avoiding ambiguous
AppContainer `HKCU` redirection, and emit `registry:host-input:readable`,
`access-denied`, or `unavailable`. Unexpected errors or incorrect values fail.
The fixture refuses to modify a preexisting key, closes query handles, and
requires deletion of its exact key and a host absence check after both Jobs
are empty. A drop fallback attempts exact-key removal on assertion failure.
It neither enumerates existing registry values nor reads credentials or changes
parent ACLs. These are sampled observations under inherited key permissions;
an unavailable result does not establish denial of all host registry inputs,
and a readable result identifies an input that must be denied or bound. Native
observations remain pending. This control does not justify capability removal,
complete registry identity, broker closure, or authenticated Windows builds.

The same disposable helper now samples in-process `CLSID_ShellLink` activation
with `IID_IUnknown`, after the existing registry and provider probes. The host
must activate successfully. Each LPAC variant reports `com:shell-link:activated`,
`access-denied`, or `class-unavailable`; other HRESULTs fail the control. A class
registration failure is recorded separately from access denial. Each successful
COM initialization is balanced on the same helper thread, and each returned
interface is released before COM teardown. No shell methods, shortcut writes,
UI, remote activation or existing user documents are used.

This follows Microsoft's [ShellLink activation example](https://learn.microsoft.com/en-us/windows/win32/shell/links)
and [CoCreateInstance contract](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-cocreateinstance).
Activation success identifies an additional loader/configuration channel to
deny or bind; it does not demonstrate that later object methods succeed.
In-process activation cannot establish local-server or broker closure. An
isolated Rust local-server fixture now extends the same capability comparison.
The test process registers a fresh GUID with `CoRegisterClassObject`, local-server
context only and `REGCLS_MULTI_SEPARATE`. Separate helper processes request
`IID_IUnknown`; host activation must succeed and increment the Rust factory's
server-side creation count. Each LPAC variant records activation, access denial,
or class unavailability, and its observation must match the creation-count delta.
This samples cross-process COM dispatch under the host's default COM security;
each root helper also launches the same isolated Rust executable directly as a
descendant, with inherited controlled environment and output handles. Host root
and descendant activation must both succeed. Each LPAC run requires separate
root and descendant observations, and their combined successful activation count
must match the server-side creation delta. The outer timeout and Job supervision
cover the contained descendant, and cleanup still requires an empty Job before
revocation. This checks a direct descendant, not arbitrary broker closure or
detached process topology. Descendant waits reuse the host supervisor's bounded
native wait, termination and reaping, without creating capture files from inside
the LPAC. Native execution remains required.
No custom security descriptor, elevation, shell action, or persistent registry
entry is installed. The inert factory uses atomic reference accounting and exposes
no host operations. Helpers retain the existing bounded waits/output supervision.
After both helpers and Jobs exit, normal cleanup checks revocation of the exact
registration cookie; drop attempts revocation on assertion failure and balances
COM initialization on the registering thread. A fresh bounded host helper must
then observe class unavailability with no further server creation. This uses Microsoft's
[running EXE server contract](https://learn.microsoft.com/en-us/windows/win32/com/registering-a-running-exe-server).
It does not test SCM launch from registry registration, arbitrary brokers, or
response/input binding. Native paired observations and authority closure remain
required before identity work can claim closure. No native results exist yet.

`standalone_com_server_activates_and_revokes_its_supplied_class` adds an
independently running server process using that same Rust factory. A test-only
Windows CRT initializer dispatches its exact private argument prefix before
libtest parses COM's `-Embedding` argument. Production binaries contain no such
dispatch. The parent supplies a fresh GUID and disposable directory; create-new
readiness evidence follows registration, and a stop file requests exact-cookie
revocation. The server has a forty-second internal deadline and the existing
bounded host supervisor owns termination and reaping. A separate client must
activate the class, the server must report exactly one dispatch, and a fresh
client must observe class unavailability after server exit. Native execution is
pending. This is a prerequisite for a
[LocalServer32 command](https://learn.microsoft.com/en-us/windows/win32/com/localserver32),
not an SCM-launched control: that test still starts the server directly.

`scm_com_server_activates_and_removes_its_exact_registration` adds a separate
host-positive SCM launch control. It opens the existing per-user CLSID parent,
creates only a fresh GUID key and its `LocalServer32` child, and installs the
existing Windows serializer's quoted Rust test-executable command. An attempted
collision must fail without replacing the original command; subsequent host
root and descendant activation must still reach the original server exactly
twice. No direct server
spawn occurs in this control. The server publishes readiness and its PID in the
disposable directory. The parent retains a process handle, requests stop, waits
at most five seconds for exit, and checks both the exit code and final dispatch
count. Unwinding attempts termination through that held handle; the server's
internal deadline also bounds attempts that fail before readiness. This is
trusted-fixture supervision, not Job ownership or broker containment.

Cleanup deletes only the exact two newly created registry keys, child first,
checks their absence, and requires class unavailability from a fresh client.
It neither creates nor deletes shared registry ancestors. A missing per-user
CLSID parent or an execution context that ignores per-user COM registration
fails the host-positive control rather than falling back to machine registration.
The same verified suspended-launch helper now runs registryRead and capability-free
LPAC clients against the already running SCM-launched class, including direct
descendants. This does not test an LPAC client starting an inactive server. Each
run requires separate root and descendant observations. The final server dispatch
count must equal the two host activations plus all reported contained activations;
class unavailability and access denial remain characterization, not broker closure.
The forty-second server deadline covers the bounded host and paired client waits.
Native Windows execution, complete broker authority closure and recipe identity
remain required.

The capability comparison's host-positive helper has a ten-second native process
wait. Timeout terminates the helper and requires termination observation within
five seconds before returning failure. File-backed stdout/stderr avoid waiting
for EOF on handles retained by an activated server; each returned stream is
limited to 16 KiB. This is test supervision of a trusted Rust fixture, not a
tree-containment guarantee or a live output/disk quota. The nonignored
`host_authority_probe_timeout_terminates_and_reaps_helper` control exercises the
stalled-helper path. The nonignored
`host_authority_probe_preserves_output_and_rejects_each_oversized_stream` control
pairs successful stdout/stderr capture with separate stdout-only and stderr-only
overflow helpers, using direct native stream writes rather than libtest capture.
Native Windows execution of these controls remains required.

## Private Windows dependency write and key validation

The issue #155 branch adds protected owner-only Windows lifecycle keys and validates low-integrity write targets. The CLI labels only its private dependency copies. General execution also supports existing ordinary declared write targets through the main branch topology and effective-access checks. Native regressions require overwrite and reopen of an existing file, creation and reopen in the requested working directory, denied reads/writes outside authority, exact DACL restoration, hard-link rejection, and key/attestation forgery rejection. Cross-compilation and local Unix tests are development evidence. The focused Windows acceptance step and full native workspace suite must pass at the claimed commit. Toolchain identity and integrated contained dependency acceptance remain outstanding. See [ADR 0010](adr/0010-private-windows-dependency-write-trees.md).

Native Windows CI run [38094689980](https://github.com/LimeTip/tapid/actions/runs/38094689980) on code commit `6c30fc20d462c6295ed5808705e5686f42d1e76e` passed all three lifecycle-key probes and all 25 containment tests with Node explicitly configured, followed by the complete Windows workspace, integration, and installer job. The hard-link regression exposed label propagation changing external inheritance metadata before rejection; preparation now validates the entire tree before changing any label. The junction and creation/reopen probes pass, including outside-path denial and exact DACL restoration. This is targeted component acceptance, not completion of the common assurance matrix or support for default authenticated Windows dependency builds.

An additional negative control at commit `15c82f428ab742b00a177c2101861897abef016a` exposed ambient All Application Packages access. In [CI run 38126867693](https://github.com/LimeTip/tapid/actions/runs/38126867693), the hook read an undeclared fixture file granted only to that package group. The other 25 containment tests passed. This failure prevents treating the regular AppContainer's explicit grants as a complete input boundary. The correction opts into Less-Privileged AppContainer launch, checks the actual suspended child token with paired native access controls, and grants only the registryRead runtime capability needed for socket-provider initialization, with no network capabilities. Native dependency acceptance passed on code commit `606ff9bdc62e7d3335acb3621bd843f8804a7748` in [CI run 38129112216](https://github.com/LimeTip/tapid/actions/runs/38129112216). All 28 containment tests and all three lifecycle-key probes passed. The root and descendant controls use a private copy of the Rust test executable, require successful approved reads, and require permission-denied reads of the undeclared AAP fixture. The full Windows workspace, integration, and installer validation steps also passed. LPAC alone does not establish complete toolchain identity. Track the remaining authority/identity work in [#273](https://github.com/LimeTip/tapid/issues/273), integrated Windows install acceptance in [#274](https://github.com/LimeTip/tapid/issues/274), macOS feasibility in [#275](https://github.com/LimeTip/tapid/issues/275), and the resulting platform-contract decision in [#276](https://github.com/LimeTip/tapid/issues/276).

The post-8cf2c84 Windows implementation embeds a Cargo-built stock-Node compatibility DLL and delegates finite exact read-attributes-only ancestor handles without shared ACL mutation. Integrated Node 22/24 IPC, native realpath, default test isolation and grandchild controls pass on Home build 26200; the real news-site CLI build/test pass. Receipts now disclose component identity and delegated metadata grants. Broader Windows and exact-head CI acceptance remain separate; see the updated Windows scope and standard-Node design.

Hosted Windows Server testing exposed a denied `NUL` open while libuv prepares ignored stdin. The adapter now reuses a pinned copy of Tapid's existing closed stdin pipe for that exact read-only stdio request. It verifies EOF before initialization and propagates the non-inherited handle to suspended descendants. Live pipes, ordinary opens, and null-device writes are excluded; no system device ACL or filesystem grant changes. A forced-denial regression checks the fallback locally; hosted acceptance remains required.
