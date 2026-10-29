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

### Issue 156 acceptance coverage

The `linux-managed-tree` CI job targets Ubuntu 24.04 x86_64 with externally
supplied Node 22. It requires `TAPID_REQUIRE_MANAGED_ASSERTIONS=1` and
`TAPID_REQUIRE_NODE_ASSERTIONS=1`; unavailable prerequisites fail the job.
A transient systemd unit owns the delegation and uses systemd 254 or newer's
`DelegateSubgroup=driver` to place its trusted driver in a leaf before execution.
The launch disables systemd argument environment expansion with
`--expand-environment=no`, leaving the complete program's PID, parameter and
command substitutions for Bash to evaluate in the driver.
The harness logs raw unified membership and unit identity before validation,
corroborates the systemd-reported delegation parent with the driver path, and
rejects malformed paths or membership mismatches before controller writes. It
reads parent and driver PID membership in the supervisor with Bash built-ins,
labels each observation, and reports parent occupancy separately from missing
supervisor membership. It validates the empty delegation parent, records cgroup
types and controllers,
enables memory/pids for sibling boundaries, and
actually attempts private PID/mount namespace creation. It does not remount the
host cgroup filesystem or change host-wide namespace policy. The runner still
performs its own request-specific checks before target execution. Logs retain
the checkout SHA, kernel/architecture, runtime/toolchain versions, controller
observations, and test results. This configured job is **not** native acceptance
evidence until it executes successfully on the candidate source.

An always-run evidence step copies only allowlisted acceptance files into a
separate upload directory, using held descriptors and rejecting symlinks,
nonregular files and hardlinks. It does not traverse the installed project.
It prints bounded command diagnostics before upload and preserves the native
step's failure status. Synthetic orchestration may declare its expected Node
major; the native entry point and default acceptance still require Node 22.
Readiness failures remain primary even when cancellation also fails; summaries
retain the secondary cleanup diagnostic without claiming successful cleanup.

The ManagedTree fixture retains its exact policy, commands, selected Node version and
binary fingerprints, logs and checked result files. It also retains host-side
metadata for the installed `next` shim: link, canonical
project-contained target, executable mode, recognized Node shebang and applicable
host mount options. Inspection executes no application fallback and rejects
resolved out-of-project targets. Incomplete inspection fails acceptance after
script execution and checked cleanup, preserving any earlier command failure
as the primary error. These observations precede containment and do not
establish the private mount namespace's permissions. The CLI invocation probe
installs an offline package through the verified install path and invokes its
Node-backed bin by command name before the existing outside-secret controls.
After a failed reference build, a separate diagnostic reuses the build script's
ManagedTree policy, selected runtime and memory-statistics option. The selected
Node executes an inline program, without loading a generated project file or
reading a project report file. A tagged JSON record is read from a descriptor-checked
regular diagnostic log capped at 32 KiB; malformed, missing, duplicate or oversized
records remain diagnostic errors. It records
runtime/search paths, cwd and numeric uid/gid, independent lexical project
ancestors and relative/absolute path observations, stat/lstat modes and device/inode
identities, separate read/execute accessibility, and loading results
for `next --version` by command name and absolute shim path. Application output
and unrelated environment values are not retained. Observations are limited to 16 KiB and
diagnostic log prefixes to 4 KiB in `summary.json`; inaccessible observations
remain explicit errors. The isolated manifest is restored afterward. Diagnostic
success never accepts the failed build or replaces its primary error. Native
execution of this diagnostic is required to attribute the reference-tool denial.
The fixture's separate candidate policy uses 180 seconds, 4 MiB combined output,
256 kernel tasks and 2 GiB cgroup memory.
These are provisional ceilings, not native measurements or support evidence.
The fixture runs in a separate delegated parent so CI can retain memory.peak
and pids.peak when exposed by the kernel. These inclusive measurements also
charge orchestration and Tapid; absence of pids.peak leaves task headroom
measurement pending. Command logs retain elapsed time and output bytes.
The build explicitly opts into read-only private process memory statistics; the
private-procfs denial probe remains required. No new dependency-hook permission
or Windows claim follows from reference-application success.

The current acceptance map is below. Runner names refer to
`crates/tapid-runner/src/linux_restricted/tests.rs`; the CLI invocation probe is
in `crates/tapid-cli/tests/cli_cases/run_permissions.rs`. Presence in this table
does not imply a non-skipped native pass.

| Criterion | Existing or added probe | Remaining acceptance work |
| --- | --- | --- |
| Positive execution, Node identity, local tool lookup, argument order, empty/quoted/Unicode/metacharacter arguments, exit codes | `linux_managed_tree_cli_preserves_invocation_environment_and_checked_results` | Execute required lane; extend local-tool collision controls if changing PATH precedence |
| Working directory, allowlisted and absent values, synthetic secret non-inheritance, Node options/proxy/agent filtering, descendant writes and denied symlink reads | Same CLI probe; existing Restricted permission tests remain separate | Execute required lane; native loader controls remain separate |
| Detached/re-parented authority and descriptor hygiene | `managed_tree_propagates_authority_to_detached_reparented_targets`: granted reads/writes, denied absolute/parent/sibling/symlink reads and writes, granted/denied loopback sockets, and an intentionally inherited denied-data descriptor; descendant observes namespace-init parent and its own session | Execute required lane; failed probes are not enforcement evidence |
| Timeout and detached descendant termination | `managed_tree_enforces_timeout_and_cleans_detached_descendants` | Execute required lane |
| Normal/nonzero root exit and session-changing descendant cleanup | `managed_tree_preserves_exit_codes_and_cleans_descendants_on_root_exit` | Execute required lane; SIGINT and supervisor crash are separate probes |
| Combined output and task ceilings | `managed_tree_enforces_output_and_process_limits`; `managed_tree_bounds_forwarded_combined_output_and_cleans_writers` externally verifies 256 non-UTF-8 bytes per stream within the ceiling, then a combined 512-byte ceiling during overflow without assuming scheduler-dependent stream shares; both runs require complete cleanup and no delayed descendant marker | Execute required lane |
| Unread caller output | `managed_tree_completion_is_bounded_when_parent_stdout_is_unread`: retain an unread caller pipe while a real target emits 1 MiB, require bounded failure with KernelOwnedComplete | Execute required lane; output remains buffered until tree cleanup |
| Cgroup memory ceiling | `managed_tree_enforces_memory_limit_with_rust_workload` | Execute required lane; workload commits real pages and no longer requires Python |
| Read-only metadata and kernel-control grant rejection | `managed_tree_denies_metadata_mutation_outside_write_grants`, `managed_tree_rejects_kernel_control_filesystem_write_grants` | Execute required lane |
| Private procfs compatibility | `managed_tree_memory_stats_opt_in_uses_read_only_private_procfs` | Execute required lane, then establish fixture build compatibility |
| Caller and supervisor crash | `managed_tree_caller_crash_kills_double_forked_descendants`, `managed_tree_supervisor_crash_kills_double_forked_descendants`: retain the kernel cgroup events object, require populated=0, and reject delayed descendant markers; supervisor identity is held by pidfd | Execute required lane |
| CLI cancellation | `linux_managed_tree_cli_sigint_cancels_and_completes_kernel_cleanup`: signal only Tapid while a detached Node descendant ignores SIGINT; require a descendant-held FIFO handshake and live-writer negative control before signalling, bounded EOF after exit, exit 130, Cancelled, KernelOwnedComplete and no delayed marker | Execute required lane; POSIX signal-scope tests do not establish Linux tree cleanup |
| Failed target exec | `managed_tree_failed_target_exec_reports_spawn_and_kernel_cleanup`: executable script with absent interpreter | Execute required lane; require structured Spawn and KernelOwnedComplete |
| Clone3/cgroup reassignment and Unix socket controls | `managed_filter_denies_clone3_and_reconnectable_unix_socketpairs` | Full target/descendant positive and negative network controls |
| Missing prerequisites | `managed_tree_requires_private_launcher_with_otherwise_valid_limits`; `managed_tree_rejects_independently_denied_kernel_prerequisites_before_target` first executes an identical valid request, then independently denies Landlock, pidfds, namespace creation, mount setup, seccomp installation, delegation-directory creation, controller/interface opens and launcher exec through inherited kernel filters | Execute required lane; kernel denial injection covers unavailable operations, not every host provisioning failure |
| Reference fixture build/test/start and cancellation | `scripts/check-managed-news-site.ts` applies `tapid.linux-managed.toml` to an isolated fixture, checks build/test results and HTTP marker, cancels start, separately verifies complete cleanup and connection refusal; `tests/news_site_managed_test.ts` covers orchestration failures using a synthetic CLI | Execute required lane and measure native workload headroom before treating candidate limits as final |

The delegated native reference workload uses a unique root-owned mode-0700
`/tmp/tapid-managed-news.*` directory. It does not run beneath the hosted
runner's private home: after setup capabilities are removed, uid 0 must still
have ordinary traversal permission on every absolute ancestor. The privileged
`managed_tree_absolute_tools_require_traversable_fixture_ancestors` control
compares relative and absolute access below a disposable foreign-owned ancestor,
then checks absolute tool execution with root-owned ancestors while retaining
sibling-secret and package-write denials. It requires the delegated root lane;
an ordinary account or non-Linux run does not establish this behavior.

An always-run collector copies only allowlisted, single-link regular evidence
files from held descriptors into a separate upload directory. It never copies
the installed project. Successful collection precedes removal of the owned
temporary workload; collection failures retain the workload for diagnosis.
When `pids.peak` is unavailable, the lane samples hierarchical `pids.current`
every 10 ms for at most 150000 samples and reports the observed maximum as a
lower bound. This is an actual task-count observation, not an exact peak or
proof that short-lived spikes had equivalent headroom. Native acceptance and
support remain subject to the complete reference-workload result.


Reconciliation of the supplied historical PR #176 concerns: current ManagedTree
capture has a finite combined ceiling (at most 16 MiB), and the existing
Restricted news-site job uses project-local TMPDIR. The new native job attempts
host delegation and namespace operations directly instead of importing the old
container/remount design. Successful target execution and fixture compatibility
remain unverified. No support claim, dependency-hook gate, or Windows evidence is
changed by this coverage inventory.

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

## Private Windows dependency write and key validation

The issue #155 branch adds protected owner-only Windows lifecycle keys and validates low-integrity write targets. The CLI labels only its private dependency copies. General execution also supports existing ordinary declared write targets through the main branch topology and effective-access checks. Native regressions require overwrite and reopen of an existing file, creation and reopen in the requested working directory, denied reads/writes outside authority, exact DACL restoration, hard-link rejection, and key/attestation forgery rejection. Cross-compilation and local Unix tests are development evidence. The focused Windows acceptance step and full native workspace suite must pass at the claimed commit. Toolchain identity and integrated contained dependency acceptance remain outstanding. See [ADR 0010](adr/0010-private-windows-dependency-write-trees.md).

Native Windows CI run [38094689980](https://github.com/LimeTip/tapid/actions/runs/38094689980) on code commit `6c30fc20d462c6295ed5808705e5686f42d1e76e` passed all three lifecycle-key probes and all 25 containment tests with Node explicitly configured, followed by the complete Windows workspace, integration, and installer job. The hard-link regression exposed label propagation changing external inheritance metadata before rejection; preparation now validates the entire tree before changing any label. The junction and creation/reopen probes pass, including outside-path denial and exact DACL restoration. This is targeted component acceptance, not completion of the common assurance matrix or support for default authenticated Windows dependency builds.

An additional negative control at commit `15c82f428ab742b00a177c2101861897abef016a` exposed ambient All Application Packages access. In [CI run 38126867693](https://github.com/LimeTip/tapid/actions/runs/38126867693), the hook read an undeclared fixture file granted only to that package group. The other 25 containment tests passed. This failure prevents treating the regular AppContainer's explicit grants as a complete input boundary. The correction opts into Less-Privileged AppContainer launch, checks the actual suspended child token with paired native access controls, and grants only the registryRead runtime capability needed for socket-provider initialization, with no network capabilities. Native dependency acceptance passed on code commit `606ff9bdc62e7d3335acb3621bd843f8804a7748` in [CI run 38129112216](https://github.com/LimeTip/tapid/actions/runs/38129112216). All 28 containment tests and all three lifecycle-key probes passed. The root and descendant controls use a private copy of the Rust test executable, require successful approved reads, and require permission-denied reads of the undeclared AAP fixture. The full Windows workspace, integration, and installer validation steps also passed. LPAC alone does not establish complete toolchain identity. Track the remaining authority/identity work in [#273](https://github.com/LimeTip/tapid/issues/273), integrated Windows install acceptance in [#274](https://github.com/LimeTip/tapid/issues/274), macOS feasibility in [#275](https://github.com/LimeTip/tapid/issues/275), and the resulting platform-contract decision in [#276](https://github.com/LimeTip/tapid/issues/276).

The post-8cf2c84 Windows implementation embeds a Cargo-built stock-Node compatibility DLL and delegates finite exact read-attributes-only ancestor handles without shared ACL mutation. Integrated Node 22/24 IPC, native realpath, default test isolation and grandchild controls pass on Home build 26200; the real news-site CLI build/test pass. Receipts now disclose component identity and delegated metadata grants. Broader Windows and exact-head CI acceptance remain separate; see the updated Windows scope and standard-Node design.

Hosted Windows Server testing exposed a denied `NUL` open while libuv prepares ignored stdin. The adapter now reuses a pinned copy of Tapid's existing closed stdin pipe for that exact read-only stdio request. It verifies EOF before initialization and propagates the non-inherited handle to suspended descendants. Live pipes, ordinary opens, and null-device writes are excluded; no system device ACL or filesystem grant changes. A forced-denial regression checks the fallback locally; hosted acceptance remains required.
