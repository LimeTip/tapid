# ADR 0005: Default-on containment for root scripts

## Status

Accepted; amended to separate Restricted authority containment from ManagedTree lifecycle ownership

No native backend is implemented or validated at the current exact HEAD. The integrated command therefore still fails closed before spawning a root script.

## Decision history

The original decision required filesystem, network, environment, descendant ownership, cleanup, and tree-wide resource controls as one indivisible sandbox contract. That strict position prevented a partial backend from being presented as complete containment and correctly exposed the lack of a race-free descendant boundary on macOS 26.

This amendment preserves that original contract as the **ManagedTree** assurance level, including for legacy profiles that omit an assurance selector. It adds **Restricted** as an explicit target for compatible checked-in root-script profiles, separating authority containment from complete lifecycle ownership without silently weakening existing strict profiles. The amendment does not reinterpret existing fail-before-spawn evidence as native enforcement and does not make any platform supported.

## Context

`tapid run` executes an explicitly selected root `package.json` script through the platform shell. The initial compatibility implementation inherited the complete environment and provided no filesystem, network, resource, or descendant-process containment. That behavior made ordinary Node.js workflows possible, but it exposed ambient credentials and the user's wider filesystem to project scripts.

Root scripts are user-invoked project code, not dependency lifecycle scripts. They nevertheless cross an execution boundary and can be supplied or modified by an untrusted repository. Argument quoting, static analysis, process groups, policy labels, and command arguments such as `--hostname` or `--port` do not provide a sandbox.

Authority containment and lifecycle ownership are different guarantees:

- **Authority containment** limits which filesystem, network, environment, descriptor, and process-creation capabilities the script and its descendants can exercise.
- **Lifecycle ownership** gives Tapid a race-free, kernel- or VM-owned boundary for the complete descendant tree so cleanup, termination, and tree-wide limits are complete rather than best effort.

Containment quality and available primitives differ substantially between operating systems. Tapid must not silently weaken a requested guarantee or report declarations as enforcement.

## Decision

`tapid run` remains default-on and fail-closed. In the approved target schema, a compatible checked-in profile selects **Restricted** with `assurance = "restricted"`. If `assurance` is omitted, the profile remains **ManagedTree**, preserving the original strict contract for existing configuration. The schema correction is pending and this ADR does not claim it is implemented at the current exact HEAD.

The desired invocation remains:

```text
tapid run <SCRIPT> -- <ARGS...>
```

For example:

```text
tapid run dev -- --hostname 127.0.0.1 --port 3001
```

Values after `--` are opaque script arguments. They do not grant or restrict authority. In particular, `--hostname 127.0.0.1` and `--port 3001` may affect application behavior but do not make `network = true` loopback-only or constrain a broker or sandbox.

A checked-in `tapid.toml` may define defaults and exact per-script permissions. The target `assurance` selector is resolved with the profile; `assurance = "restricted"` is explicit, while omission resolves to ManagedTree. Configuration can grant project-relative reads and writes, network access, selected environment variables, subprocess use, and bounded resource limits. Unknown fields, invalid paths, invalid environment names, unsupported combinations, and unrepresentable required dimensions are rejected before a shell starts. Project configuration cannot disable containment.

The portable `network` field remains boolean. `network = false` requests network denial. `network = true` grants unrestricted networking, including listen and connect behavior; it is not a host, port, protocol, loopback, ingress, or egress policy. Declared listen/connect scopes and brokered-port policy require future schema and native enforcement work.

### Restricted assurance

Restricted requires all of the following before untrusted execution starts:

- an explicit child environment and controlled `PATH`, without ambient credential, proxy, agent-socket, or loader-variable inheritance;
- descriptor or handle hygiene so removing environment variables does not leave equivalent inherited authority;
- requested filesystem and network authority established by the native backend before spawn;
- authority restrictions that propagate to descendants, including descendants that detach, re-parent, or create new sessions;
- rejection before spawn when a required Restricted dimension cannot be established.

Restricted does **not** claim race-free ownership of every descendant or a complete kill and cleanup boundary. It provides no cleanup guarantee. A backend may attempt best-effort cleanup, but completion evidence must say what was actually attempted or observed and where races or escape uncertainty remain; it must distinguish that evidence from having no cleanup guarantee. Checked launch evidence must exactly match the requested authority and report the mechanism, assurance level, enforcement scope, and limitations. Completion evidence covers lifecycle and cleanup only: it cannot re-confirm authority established before launch or label best-effort discovery as ManagedTree ownership.

Path evidence is similarly scoped. `CanonicalPath` means the path was freshly resolved for setup and retains the documented host-race limitation. It is not `NativeObject`; only a backend that holds and revalidates a native object identity may report `NativeObject`.

### ManagedTree assurance

ManagedTree includes every Restricted requirement and additionally requires:

- a race-free, kernel- or VM-owned boundary containing all descendants before they can execute outside it;
- a complete cleanup and kill boundary across normal completion, cancellation, timeout, Tapid termination, and relevant supervisor failure modes;
- configured timeout, output, process-count, and memory semantics enforced and accounted across the complete tree;
- failure before spawn when any required dimension or configured tree-wide limit cannot be established.

Process groups, PID scans, lineage observation, or best-effort cleanup cannot satisfy ManagedTree. A Job Object contributes lifecycle ownership on Windows but is not by itself filesystem, network, environment, or handle containment.

### Explicit unsandboxed execution

A future `--no-sandbox` escape is planned only for trusted interactive projects. It must be explicit and noisy, cannot be selected by project configuration, must not silently replace failed Restricted or ManagedTree setup, and must produce an unsandboxed outcome rather than an enforcement receipt. Unattended use is rejected unless a separate, explicit unattended authorization design is approved and implemented. Neither the CLI escape nor that authorization exists at the current exact HEAD.

### Platform direction

Platform backends are independent security boundaries and remain planned rather than implemented:

- **macOS 26 Restricted:** an experimental Seatbelt profile applied through the deprecated, path-based `sandbox-exec` interface is the planned first backend. It may restrict filesystem and network authority and propagate those restrictions to descendants, but it provides no cleanup guarantee. Any best-effort cleanup actually attempted or observed must be reported separately with deprecation, path-binding, and lifecycle limitations. It is not implemented or validated.
- **macOS 26 ManagedTree:** native ManagedTree is unsupported. Process groups are escapable with `setsid` or `setpgid`, and public process-lineage scanning retains a rapid double-fork/intermediate-exit race. A future strict Linux VM hosted through Virtualization.framework is a separate backend that changes platform and operational semantics; it must not be described as native macOS containment.
- **Linux Restricted:** the planned backend combines Landlock, `no_new_privs`, seccomp, and explicit environment/descriptor construction, selecting only enhancements proven available at runtime. A requested dimension that the active kernel or host cannot establish fails before spawn.
- **Linux ManagedTree:** support requires proven namespace ownership and cgroup delegation for descendants, cleanup, and configured tree-wide limits. Their presence must be probed rather than inferred from running on Linux or in a container.
- **Windows Restricted and ManagedTree:** the planned authority boundary is AppContainer or LPAC with explicit environment and handle construction. ManagedTree additionally uses a non-breakaway Job Object assigned before untrusted execution. Any brokered listen port or network exception requires native proof that the configured scope, identity, and descendant behavior match the receipt.

### Receipts and support claims

Checked launch evidence is derived from the backend that actually established restrictions. For every dimension it reports the request, mechanism, assurance level, enforcement state, scope, and limitation, and it is accepted only when the exact requested enforcement is present. Requested, declared, observed, and enforced states remain distinct. Availability flags, successful allowed operations, canonical paths, or policy declarations are not enforcement evidence. Post-execution completion evidence reports lifecycle and cleanup results only and cannot re-attest launch-only authority.

No platform or assurance level is described as supported until positive and negative runtime probes pass through the integrated `tapid run` path at the exact commit being claimed. Current fail-before-spawn behavior proves only that there is no silent uncontained fallback.

## Consequences

- Existing projects may need a `tapid.toml` permission entry before build, test, or development scripts can write files or use the network.
- Existing profiles that omit `assurance` retain ManagedTree semantics. New profiles that need the less strict lifecycle contract opt into Restricted explicitly.
- `tapid run dev` commonly requires project writes and unrestricted networking under the current boolean schema. Application bind arguments do not narrow that grant.
- Root scripts do not inherit arbitrary credentials, agent sockets, proxy variables, or the full user environment when a future Restricted or ManagedTree backend runs them.
- Restricted can become useful on platforms where authority restrictions propagate but complete lifecycle ownership is unavailable, without overstating cleanup or resource guarantees.
- ManagedTree retains the original strict descendant and tree-wide resource contract for automation or policies that require it.
- Process execution remains behind the focused `tapid-runner` capability. The CLI owns parsing, file discovery, interaction, and rendering.
- Dependency lifecycle scripts remain disabled by default and are not made eligible by a root-script profile. Root scripts run only through explicit selection.
- Perfect containment is not claimed. Each backend and receipt reports only the dimensions and scopes it actually enforces.
- At the current exact HEAD, no native backend exists, `--no-sandbox` does not exist, and `tapid run` still refuses to spawn root scripts.

## Rejected alternatives

### Erase the original strict lifecycle decision

Rejected. Its reasoning remains valid for ManagedTree and for callers that require complete descendant cleanup and tree-wide limits.

### Silently reinterpret existing profiles as Restricted

Rejected because existing profiles were written against the original strict descendant and tree-wide limit contract. Restricted is useful where authority propagates but complete lifecycle ownership is unavailable, but selecting it must be explicit.

### Preserve unsandboxed compatibility as the default

Rejected because explicit invocation does not justify exposing ambient credentials and the wider host filesystem without a visible decision.

### Fail open when containment is unavailable

Rejected because the command would appear restricted while executing with ambient authority.

### Treat process groups or Job Objects as a sandbox

Rejected because lifecycle containment alone does not restrict filesystem, network, environment, or inherited handles.

### Parse or rewrite package scripts instead of using OS controls

Rejected because npm-compatible scripts are opaque shell programs. Parsing shell text cannot establish a security boundary.

### Put native profiles directly in project configuration

Rejected because native policy formats are platform-specific and would make equivalent project intent non-portable. Tapid compiles a platform-neutral permission document into native enforcement.

## Staged implementation and verification

1. Preserve the current checked configuration, exact root-script selection, argument forwarding, controlled environment/PATH construction, and fail-before-spawn behavior.
2. Implement the assurance schema so `assurance = "restricted"` opts in explicitly and omission remains ManagedTree, then add one native Restricted backend behind private runner adapters. Prove pre-spawn filesystem/network enforcement, descendant propagation, environment and descriptor hygiene, and honest lifecycle/limit scope before enabling execution.
3. Integrate that backend through the exact `tapid run <SCRIPT> -- <ARGS...>` path and retain exact-commit positive and negative evidence. Do not infer support from a standalone probe.
4. Add ManagedTree only on platforms where race-free ownership, complete cleanup, and configured tree-wide limits are proven. Unsupported required dimensions continue to fail before spawn.
5. Design narrower listen/connect policy, an explicit ManagedTree spelling beyond legacy omission, or `--no-sandbox` as separate schema and CLI changes with their own review and evidence.

Verification includes:

- parser tests for strict, deterministic per-script configuration;
- preflight tests proving unsupported required dimensions do not spawn a child;
- platform runtime probes for both permitted and denied filesystem and network behavior;
- environment and inherited descriptor/handle tests;
- descendant authority-propagation probes for Restricted;
- descendant escape, cleanup, timeout, output, process, and memory probes for ManagedTree;
- human and machine-readable launch and completion evidence equivalence at each assurance level, without using completion to re-attest launch authority;
- exact-commit platform evidence through the integrated CLI path;
- documentation checks that reject unsupported or overstated sandbox claims.
