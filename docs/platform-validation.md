# Root-script platform validation

ADR 0005 platform backends and CLI wiring are pending. Existing Ubuntu and Windows consumer jobs exercise the pre-ADR runner path; local macOS tests exercise only the current checkout. None is evidence that default-on containment, minimal environment construction, descendant control, or resource limits are enforced.

No macOS, Linux, or Windows backend may be marked supported until the complete probe set below passes through `tapid run <SCRIPT> -- <ARGS...>` at the exact integrated commit. A unit test of policy declarations, backend availability check, compilation result, or successful allowed operation is insufficient by itself.

## Evidence record

For each operating system and architecture, retain:

- the full 40-character commit from `git rev-parse HEAD`, with a clean tracked tree and the commit containing the runner backend, configuration parser, CLI wiring, probes, and documentation;
- the workflow URL and immutable run/job identifiers, attempt number, runner image/version, OS build or kernel version, architecture, shell/runtime versions, and Rust toolchain;
- the built `tapid` artifact digest and logs that identify the same commit;
- the exact checked-in `tapid.toml`, fixture scripts, commands, exit codes, stdout/stderr, enforcement receipt, and pass/fail result for every probe;
- evidence that the human and machine-readable receipts agree about requested, declared, observed, and enforced dimensions.

Do not update platform status from a run against a merge commit, rebuilt artifact, or fixture revision different from the recorded commit unless that exact revision is named as the evidence target. Re-run the matrix after any change to the backend, policy compiler, process supervision, CLI wiring, fixture, or probe assertion.

## Required runtime probes

Every category needs a positive control proving the fixture can perform the operation when granted and a negative control proving the same operation is denied when not granted. Negative probes must also confirm a nonzero result and an enforcement receipt; a crash, missing dependency, malformed command, or skipped test is not a containment pass.

### Filesystem

- **Positive:** read a declared project file and create, modify, and remove files in each declared `write` path.
- **Negative:** deny writes to an undeclared project path, a sibling/parent path, the user home, and an operating-system temporary path. Deny reads outside declared project/runtime paths. Repeat escape attempts through `..`, absolute paths, symlinks, and descendants.
- Confirm the minimal shell/runtime files are available without turning their parent trees into broad writable grants.

### Network

- **Positive:** with `network = true`, bind a loopback server and connect to it from an allowed descendant.
- **Negative:** with `network = false`, deny loopback bind, loopback connect, external connect, and name resolution while a local control endpoint proves the test network is otherwise reachable.
- The current portable setting is boolean. A successful loopback use with `network = true` must not be reported as loopback-only enforcement; outbound access is granted too.

### Environment and inherited state

- **Positive:** list one benign variable in `environment` and verify its exact caller value reaches the script when present; verify the controlled `PATH` resolves the managed project executable and required shell/runtime.
- **Negative:** inject unique sentinel values into unlisted variables representing cloud credentials, package tokens, proxy variables, `HOME`, SSH/GPG/agent sockets, and arbitrary secrets; verify neither the script nor descendants can observe them. Verify absent allowlisted variables are not invented.
- Check inherited descriptors or handles separately so removing a variable does not leave its referenced credential channel open.

### Forwarded arguments

- Pass empty strings, spaces, quotes, Unicode, leading dashes, shell metacharacters, and multiple ordered values after `--`; verify the fixture receives the exact argument vector in order and that Tapid does not parse them as its own options.
- Run an equivalent invocation without `--` that should be rejected by CLI parsing, proving the probe is testing the documented separator contract rather than accidental shell behavior.

### Descendants and subprocess policy

- **Positive:** with `subprocess = true`, start the required platform shell and a child Node process; verify the child retains the same filesystem, network, environment, and resource boundary.
- **Negative:** with `subprocess = false`, verify an attempted child does not start. Attempt detached, double-forked, re-parented, or Windows breakaway descendants and verify they cannot escape restrictions or survive normal completion, cancellation, timeout, or Tapid termination.
- Confirm cleanup by PID/Job membership and by the absence of a delayed descendant marker after the supervisor exits.

### Limits

- **Timeout:** a command finishing below `timeout_seconds` succeeds; one exceeding it is terminated with all descendants and a stable limit reason.
- **Output:** output below `max_output_bytes` succeeds; output crossing the limit through stdout, stderr, and descendants is bounded, terminated according to contract, and reported without unbounded buffering.
- **Processes:** a tree at or below `max_processes` succeeds; an attempt to create the next process is denied or terminates according to the backend contract, without a race that permits escape.
- **Memory:** a process tree below `max_memory_bytes` succeeds; a controlled allocation crossing it is stopped and reported. Record whether the backend accounts for the whole tree and do not claim more than the receipt proves.

### Fail-closed startup and receipts

- Corrupt or remove the required backend primitive, request an unsupported combination, and use invalid or unknown `tapid.toml` fields. Verify no script or descendant marker is created.
- Verify failure to establish filesystem, network, process, environment/handle, or resource controls aborts before untrusted code starts; partial setup must be torn down.
- Compare human and machine-readable output for the same run and reject any receipt that labels a requested, declared, or merely observed capability as enforced.

## Platform-specific gates

### macOS

The initial design uses a generated Seatbelt profile through Apple's deprecated `sandbox-exec`, plus explicit environment/descriptor construction, process-group supervision, and representable resource limits. The job must run a behavioral startup probe before untrusted code. Record the backend as deprecated. If `sandbox-exec` is absent or behavior differs from the probe, required containment is unavailable and execution must fail closed; there is no uncontained fallback.

### Linux

The proposed design combines Landlock filesystem rules, `no_new_privs`, seccomp or a network namespace, explicit environment/descriptor construction, process supervision, and resource controls. Record kernel and feature availability and exercise the actual selected combination. Containers or hosted runners that cannot establish every requested dimension must fail closed. This backend remains pending until the complete exact-commit matrix passes.

### Windows

The proposed design uses AppContainer for filesystem/network isolation, a non-breakaway Job Object for descendants and resources, explicit token/environment/handle construction, and assignment before untrusted execution. Probes must detect breakaway children, inherited handles, path variants, and assignment races. Incomplete AppContainer or Job setup must fail closed. This backend remains pending until the complete exact-commit matrix passes.

## Current status

| Backend | ADR 0005 status | Evidence |
|---|---|---|
| macOS Seatbelt (`sandbox-exec`, deprecated) | Pending implementation and integrated verification | None recorded |
| Linux Landlock plus network/process controls | Pending implementation and integrated verification | None recorded |
| Windows AppContainer plus Job Object | Pending implementation and integrated verification | None recorded |

Keep package-manager, installer, and pre-ADR consumer evidence separate from this table. A local result on one platform is never evidence for another.
