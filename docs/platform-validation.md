# Root-script platform validation

ADR 0005 CLI wiring is integrated and fails closed when no backend can prove every required restriction. No native Restricted or ManagedTree backend is implemented at the current exact HEAD, and no enforcement receipt can be produced. Existing consumer jobs and local macOS tests exercise package-manager behavior and fail-closed preflight, not native containment, descendant authority propagation, lifecycle ownership, or resource-limit enforcement.

A platform may be marked Restricted only after the Restricted probe set passes through `tapid run <SCRIPT> -- <ARGS...>` at the exact integrated commit. ManagedTree requires the Restricted probes plus the ManagedTree-only probes. A unit test of policy declarations, backend availability, compilation, a generated native profile, or a successful allowed operation is insufficient.

## Evidence record

For each operating system, architecture, backend, and assurance level, retain:

- the full 40-character commit from `git rev-parse HEAD`, with a clean tracked tree and the commit containing the runner backend, configuration parser, CLI wiring, probes, and documentation;
- the workflow URL and immutable run/job identifiers, attempt number, runner image/version, OS build or kernel version, architecture, shell/runtime versions, and Rust toolchain;
- the built `tapid` artifact digest and logs that identify the same commit;
- the exact checked-in `tapid.toml`, fixture scripts, commands, exit codes, stdout/stderr, enforcement receipt, and pass/fail result for every probe;
- evidence that human and machine-readable receipts agree about each dimension's request, mechanism, assurance level, declared/observed/enforced state, scope, and limitation;
- the backend identity, native primitive versions or feature probes, and any deprecation, path-binding, broker, delegation, or best-effort lifecycle limitation.

`CanonicalPath` evidence proves only fresh pathname resolution for setup and retains its documented host-race assumption. It must never be recorded as `NativeObject` unless the backend holds and revalidates a native object identity.

Do not update platform status from a run against a merge commit, rebuilt artifact, or fixture revision different from the recorded commit unless that exact revision is named as the evidence target. Re-run the applicable matrix after any change to the backend, policy compiler, process supervision, CLI wiring, fixture, probe assertion, or receipt schema.

## Common Restricted probes

Every filesystem and network category needs a positive control proving the fixture can perform an operation when granted and a negative control proving the same operation is denied when not granted. Negative probes must also confirm a nonzero result and an enforcement receipt; a crash, missing dependency, malformed command, or skipped test is not a containment pass.

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
- Restricted may use best-effort lifecycle supervision, but its receipt must identify which descendants were observed or controlled, which cleanup was attempted, and where races or escape uncertainty remain. A delayed surviving marker fails any claim of complete cleanup but does not by itself disprove authority propagation if the surviving process remains natively restricted.

### Restricted limits and lifecycle reporting

- Exercise each configured timeout, output, process, and memory limit and record whether it applies to the initial process, observed descendants, or a complete native tree.
- Do not describe a process-local or best-effort aggregate limit as tree-wide. If policy requires a scope the backend cannot establish, verify failure before spawn.
- Exercise normal completion, cancellation, timeout, and Tapid termination. Restricted cleanup may be incomplete, but the result must report that limitation and must not claim ManagedTree.

### Fail-closed startup and receipts

- Corrupt or remove the required backend primitive, request an unsupported combination, and use invalid or unknown `tapid.toml` fields. Verify no script or descendant marker is created.
- Verify failure to establish a required filesystem, network, environment, descriptor/handle, subprocess, or limit dimension aborts before untrusted code starts; partial setup must be torn down.
- Compare human and machine-readable output for the same run. Reject any receipt that labels requested, declared, or merely observed capability as enforced, reports `CanonicalPath` as `NativeObject`, or omits mechanism, scope, or limitation.

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

The planned Restricted backend is experimental Seatbelt applied through deprecated, path-based `sandbox-exec`. Before support can be claimed, probes must establish requested filesystem and network authority before spawn, restriction propagation to detached descendants, explicit environment/PATH and descriptor hygiene, and honest path-binding and lifecycle limitations. The profile generator or standalone `sandbox-exec` behavior is not integrated evidence. This backend is not implemented or validated.

Native ManagedTree remains unsupported. Process groups are escapable with `setsid` and `setpgid`; Darwin has not supported recursive `EVFILT_PROC` tracking through `NOTE_TRACK`, `NOTE_TRACKERR`, or `NOTE_CHILD` since macOS 10.5; and `NOTE_FORK` plus process-table or `p_puniqueid` scans retains a rapid double-fork/intermediate-exit race. An optional strict Linux VM through Virtualization.framework is a separate future backend that changes platform, startup, filesystem-sharing, and network semantics; it must not be reported as native macOS ManagedTree.

### Linux

The planned Restricted design combines Landlock filesystem rules, `no_new_privs`, seccomp, and explicit environment/descriptor construction. Enhancements depend on runtime kernel and feature probes. Record the Landlock ABI and handled access rights, seccomp policy, privilege transition, network mechanism, and unavailable features. A container or hosted runner that cannot establish a required dimension must fail before spawn.

ManagedTree additionally requires proven namespace ownership and cgroup delegation. Record namespace membership, cgroup version/controllers/delegation, assignment ordering, cleanup ownership, and tree-wide accounting. Running inside a container or cgroup does not itself prove Tapid owns the boundary.

### Windows

The planned authority design uses AppContainer or LPAC for filesystem/network isolation with explicit token, environment, and handle construction. Record the exact token capabilities, ACL or capability grants, network isolation state, and child inheritance. A brokered listen port requires native proof of endpoint scope, process identity, descendant behavior, and bypass resistance; application `--port` arguments are not proof.

ManagedTree additionally requires a non-breakaway Job Object assigned before untrusted execution, with complete cleanup and configured tree-wide limits. Probes must detect breakaway children, inherited handles, path variants, broker escapes, and assignment races. Incomplete AppContainer/LPAC or Job setup must fail before spawn.

## Current status

| Platform/backend | Restricted status | ManagedTree status | Native evidence |
|---|---|---|---|
| macOS 26 Seatbelt/`sandbox-exec` | Planned experimental backend; not implemented or validated | Native support unavailable | No integrated evidence recorded |
| Linux Landlock/`no_new_privs`/seccomp | Planned; runtime capability-dependent | Planned only with proven namespaces and cgroup delegation | No integrated evidence recorded |
| Windows AppContainer or LPAC plus Job Object | Planned; not implemented or validated | Planned only with non-breakaway pre-execution Job assignment and tree-wide limits | No integrated evidence recorded |
| Strict Linux VM through macOS Virtualization.framework | Optional future backend with Linux VM semantics | Future investigation | No implementation or evidence recorded |

Keep package-manager, installer, CLI preflight, Restricted enforcement, and ManagedTree evidence separate. A local result on one platform or assurance level is never evidence for another.
