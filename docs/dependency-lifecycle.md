# Dependency lifecycle scripts

Installation denies dependency scripts by default. Tapid reports skipped hooks because packages with native builds or generated files may be unusable. Root package scripts still run only through `tapid run`.

If hook discovery fails for a package with no approvals, Tapid warns and leaves its scripts unexecuted. Discovery failure is fatal when any approval names that package. Package metadata must still pass normal installation validation.

Approve each dependency's `preinstall`, `install`, or `postinstall` hook in a checked-in `tapid.lifecycle.toml`. Approval requires the exact package name, version, registry-declared archive SHA-512 integrity, and SHA-256 of the exact script bytes. An approval that names the hook but has a different version, archive, or script fails installation. `prepare`, `prepublish`, `preprepare`, and `postprepare` cannot be approved. A package with `binding.gyp` and no explicit `preinstall` or `install` has the implicit `install` command `node-gyp rebuild`.

The policy uses the following fields. Replace the example digests with verified values before use.

```toml
schema = 1
[[approvals]]
package = "native-demo"
version = "1.0.0"
archive-digest = "sha512-<canonical padded base64 SHA-512>"
hook = "install"
script-digest = "sha256-<64 lowercase hexadecimal characters>"
system-toolchain = true
read = ["."]
write = ["build"]
network = false
environment = { TMPDIR = "build" }
timeout-seconds = 30
max-output-bytes = 65536
max-processes = 64
max-memory-bytes = 536870912
tools = [
  { name = "sh", path = "/bin/sh", digest = "sha256-<64 lowercase hexadecimal characters>" },
  { name = "node", path = "/usr/bin/node", digest = "sha256-<64 lowercase hexadecimal characters>" },
]
```

Every approval must explicitly acknowledge `system-toolchain = true`. Linux execution can read and execute the backend's system runtime directories, including `/usr/bin`, libraries, headers, certificates, `/usr/share/nodejs` for distribution-provided Node builtins, and basic character devices. This permits native compilers and their helper programs. It does not grant reads of the caller's home or writes to system directories. Tool entries pin bytes copied into a private executable directory. `sh` is required; declare `node` when the hook uses Node. PATH contains that private directory, approved dependency shims when enabled, and `/usr/bin`, never the caller's PATH. The backend fingerprints the accessible system toolchain, kernel, and Tapid executable as well as declared tools. Toolchain changes invalidate generated-output reuse. Unreadable system files and directories the caller cannot search are fingerprinted by ownership, mode, size, and modification/change timestamps. Changes to readability or recorded metadata invalidate reuse. Searchable directories that cannot be listed remain fatal because known child files may still be readable. Other inspection errors also remain fatal.

Read and write paths are relative to a private copy of the dependency. Missing write directories are created there. Scripts cannot write the verified source store, active `node_modules`, project manifests, lockfiles, or other packages. The environment contains only literal policy values and Tapid's constructed PATH. Caller variables and credentials are not inherited. Enabled networking permits IPv4/IPv6 sockets; host Unix sockets remain denied and local stream socketpairs support runtime IPC. Credential names, loader injection variables, HOME, and PATH cannot be approved.

Optional `dependencies = true` grants read access to a private materialization of the resolved dependency graph. Approved packages build after their dependencies. Cyclic graphs and graphs deeper than 1024 packages fail closed when approvals are present. Live workspace packages cannot be exposed to hooks; use verified published dependencies. Optional `process-memory-stats = true` exposes the backend's read-only private procfs view. It does not expose host processes. Seccomp still denies ptrace, process_vm access, and pidfd acquisition; procfs visibility is restricted to the private PID namespace.

## Execution and outputs

The first execution backend is Linux ManagedTree. It requires Landlock ABI 3 or newer, seccomp, util-linux `unshare`, private PID and mount namespaces, and a writable delegated cgroup v2 parent with the memory and pids controllers enabled. `cgroup.kill` and swap controls must be available. Tapid does not configure host delegation. Missing prerequisites fail before dependency code runs. macOS and Windows approved execution remain unavailable and fail closed.

The supervisor joins a fresh limited cgroup before it can fork or run dependency code. A private PID namespace owns descendants, including detached and double-forked processes. Parent-death signals tie namespace init to the trusted supervisor and the supervisor to Tapid. A read-only filesystem view protects metadata as well as file contents; only declared write objects receive writable mounts. ManagedTree rejects writes to cgroup, procfs, and sysfs control filesystems by native filesystem identity. The helper drops setup capabilities and installs Landlock and seccomp before executing the hook. ManagedTree denies clone3, including descriptor-based cgroup reassignment; libc can fall back to legacy clone for threads.

Timeout and combined stdout/stderr limits apply to the whole attempt. The output ceiling must be at most 16 MiB; output is buffered until cleanup. Process limits count kernel tasks, including runtime threads and trusted setup processes. Memory uses cgroup accounting, with swap disabled. Limit breaches have distinct termination results. Cleanup kills the cgroup and waits for authoritative emptiness. Uninterruptible kernel work can delay cleanup. Abrupt caller termination kills descendants through kernel parent-death ownership but can leave empty cgroup and filesystem-view directories for host cleanup. Host administrators and concurrent host filesystem mutation are outside this boundary.

Hooks execute in `preinstall`, `install`, `postinstall` order using commands from the original verified source. Each successful output must satisfy archive entry, path, and size limits and contain no symlinks or special files. Derived trees use separate store identities. The source archive and source tree remain unchanged. Store publication, lockfile replacement, and project activation use the existing rollback transaction.

Lockfile schema 8 records each derived hook's recipe key, verified tree digest, and store-local HMAC attestation while preserving source archive and tree identities. The owner-only `.tapid-lifecycle-key` authenticates the recipe and output together. Ordinary ingested trees and forged lockfile records cannot substitute for an authenticated execution result. The key is store infrastructure; loss or corruption invalidates replay and requires rebuilding approved outputs. It can remain after a failed activation, while verified tree publication still rolls back. Installs without generated outputs retain schema 7. Recipe keys bind the complete checked-in policy, original archive, exact script, toolchain, source graph, prior hook outputs, and already-built dependency outputs.

An online install can reuse matching verified outputs or build missing outputs. Offline and frozen installs never execute hooks. They require matching policy and toolchain identities, original source trees, and every referenced derived tree; missing, revoked, changed, or corrupt prerequisites fail without changing the active installation. Regenerate online after reviewing a changed policy. Approval is permission to execute arbitrary dependency code within the declared boundary, not evidence that its output is correct.
