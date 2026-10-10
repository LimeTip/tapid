# ADR 0008: Approved dependency hooks and derived trees

Status: Accepted for implementation

## Context

Verified extraction alone cannot make dependencies with native builds or generated files usable. ADR 0005 Restricted execution does not own detached descendants or enforce tree-wide resource limits. Running installation hooks under Restricted would weaken the required installation boundary.

## Decision

Dependency hooks remain denied unless checked-in policy approves an exact name, version, registry-declared archive integrity, hook, and script digest. Only preinstall, install, and postinstall are supported. Every approval declares filesystem authority, literal environment values, network policy, pinned tools, explicit system-toolchain use, and all four resource ceilings.

Add Linux ManagedTree execution using assignment to a limited delegated cgroup before fork, private PID/mount namespaces, parent-death ownership, a read-only filesystem view with held writable objects, capability removal, Landlock, and seccomp. Kernel cgroup accounting enforces task and memory ceilings. The supervisor enforces timeout and bounded combined output, kills the cgroup, and requires observed emptiness before returning completion. Backends without these mechanisms reject execution.

Run hooks against private source copies. Keep verified source store entries immutable. Validate and stage each derived tree through the existing store transaction. Schema 9 records derivation keys, output digests, and store-local HMAC attestations separately from source identity. Owner-only key material distinguishes executed outputs from ordinary ingested trees and prevents forged lockfiles from reassigning unrelated cached trees. Bind recipes to policy, source, script, toolchain, graph, and earlier outputs. Offline/frozen replay verifies exact prerequisites and never executes hooks.

## Consequences

Linux needs explicit cgroup delegation and namespace privileges; ordinary restricted hosts can install source-only packages but cannot build approved dependencies. macOS and Windows execution remains unavailable. Managed output is buffered and limited to 16 MiB. Kernel tasks, including setup/runtime threads, count against the process ceiling. Cleanup can wait for uninterruptible kernel work.

System compiler/runtime directories are an explicit authority grant. Readable bytes enter the toolchain identity; unreadable files and unsearchable directories contribute ownership, mode, size, and modification/change timestamps because the hook cannot read their contents either. Searchable directories that cannot be listed and other inspection errors fail closed. Host administrators and concurrent host filesystem mutation remain outside the boundary. Native validation on an uncommitted tree is development evidence; it does not replace exact-commit platform acceptance probes. See [dependency lifecycle scripts](../dependency-lifecycle.md) for policy and replay behavior.
