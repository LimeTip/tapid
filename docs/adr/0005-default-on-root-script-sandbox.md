# ADR 0005: Default-on containment for root scripts

## Status

Accepted, with macOS 26 unsupported

## Context

`tapid run` executes an explicitly selected root `package.json` script through the platform shell. The initial compatibility implementation inherited the complete environment and provided no filesystem, network, resource, or descendant-process containment. That behavior made ordinary Node.js workflows possible, but it exposed ambient credentials and the user's wider filesystem to project scripts.

Root scripts are user-invoked project code, not dependency lifecycle scripts. They nevertheless cross an execution boundary and can be supplied or modified by an untrusted repository. Argument quoting, static analysis, process groups, and policy labels do not provide a sandbox.

Containment quality and available primitives differ substantially between operating systems. Tapid must not silently weaken a requested guarantee or report declarations as enforcement.

## Decision

`tapid run` uses a default-on, fail-closed execution sandbox.

A checked-in `tapid.toml` may define defaults and per-script permissions. Configuration can grant project-relative reads and writes, network access, selected environment variables, subprocess use, and bounded resource limits. Unknown fields, invalid paths, invalid environment names, unsupported combinations, and unrepresentable requirements are rejected before a shell starts. Project configuration cannot disable sandboxing.

The default profile:

- permits reading the project and the minimum runtime files required by the selected shell and executable;
- denies project writes unless declared;
- denies network access unless declared;
- removes ambient environment variables except a documented safe runtime baseline and explicitly declared names;
- permits subprocess execution because npm-compatible scripts execute through a platform shell, while constraining descendants to the platform containment boundary;
- applies configured wall-clock, output, process-count, memory, and other representable limits;
- fails before execution if every required guarantee cannot be enforced.

An explicit `--no-sandbox` CLI escape may be provided for compatibility. It cannot be selected by project configuration, must emit a prominent warning, must be represented in machine-readable output, must never produce enforced-capability evidence, and is rejected in unattended mode.

Platform backends are independent security boundaries:

- macOS 26 has no public, unprivileged primitive that can provide race-free ownership of an arbitrary descendant tree. Seatbelt restrictions inherit across ordinary fork and exec, but process groups can be escaped with `setsid` or `setpgid`, and per-process `NOTE_FORK` or `p_puniqueid` scanning retains a rapid double-fork/intermediate-exit race. Tapid therefore reports required containment as unsupported and starts no script on macOS 26. A future backend may be reconsidered only with a kernel-maintained descendant boundary, such as the restricted Endpoint Security descendant API introduced in macOS 27, or a separately reviewed VM boundary.
- Linux uses Landlock for filesystem access, `no_new_privs` and seccomp or a network namespace for network restrictions, explicit descriptor and environment construction, process supervision, and resource limits. Unsupported kernel or namespace requirements fail closed.
- Windows uses AppContainer for filesystem and network isolation, a non-breakaway Job Object for descendant and resource control, explicit token/environment/handle construction, and race-free assignment before untrusted execution. Incomplete AppContainer or Job setup fails closed.

The execution result includes an enforcement receipt derived from the backend that actually established restrictions. It distinguishes requested, declared, observed, and enforced capabilities. Availability flags alone are not enforcement evidence.

No platform is described as supported until positive and negative runtime probes pass on that platform at the exact integrated commit.

## Consequences

- Existing projects may need a `tapid.toml` permission entry before build, test, or development scripts can write files or use the network.
- `tapid run dev` commonly requires project writes and network binding, so those permissions must be explicit.
- Root scripts no longer inherit arbitrary credentials, agent sockets, proxy variables, or the full user environment by default.
- Process execution moves behind the focused `tapid-runner` capability. The CLI remains responsible for parsing, file discovery, interaction, and rendering.
- The implementation must include adversarial tests for filesystem escape, network denial, environment-secret removal, descendant inheritance and cleanup, resource limits, argument fidelity, and fail-closed startup.
- macOS 26 cannot run root scripts under this contract. This preserves the fail-closed guarantee but prevents `tapid run`, including development servers, until a stronger lifecycle boundary is selected and verified.
- Perfect containment is not claimed. Each backend reports only the dimensions it actually enforces.

## Rejected alternatives

### Preserve unsandboxed compatibility as the default

Rejected because explicit invocation does not justify exposing ambient credentials and the wider host filesystem without a visible decision.

### Fail open when containment is unavailable

Rejected because the command would appear sandboxed while executing with ambient authority.

### Treat process groups or Job Objects as a sandbox

Rejected because lifecycle containment alone does not restrict filesystem, network, environment, or inherited handles.

### Parse or rewrite package scripts instead of using OS controls

Rejected because npm-compatible scripts are opaque shell programs. Parsing shell text cannot establish a security boundary.

### Put native profiles directly in project configuration

Rejected because native policy formats are platform-specific and would make equivalent project intent non-portable. Tapid compiles a platform-neutral permission document into native enforcement.

## Verification

The decision is enforced by:

- parser tests for strict, deterministic per-script configuration;
- preflight tests proving unsupported required guarantees do not spawn a child;
- platform runtime probes that assert both permitted and denied behavior;
- environment and inherited-handle tests;
- descendant cleanup, timeout, output, process, and memory limit tests;
- human and machine-readable enforcement receipt equivalence;
- Linux, macOS, and Windows CI evidence tied to the exact commit;
- documentation checks that reject unsupported or overstated sandbox claims.
