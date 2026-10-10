# ADR 0009: Explicit dependency execution overrides

Status: Accepted

## Decision

Keep ManagedTree mandatory for contained dependency builds. Native macOS lacks
the required complete descendant boundary and tree-wide limits. Windows project
writes and authenticated lifecycle storage remain unsupported. Do not describe
uncontained execution as contained platform compatibility.

Provide separate invocation-only CLI overrides for dependency-hook approval and
containment. Neither project configuration nor a failed support probe selects an
override. Source verification and locked-version selection remain mandatory.
Frozen and offline installs never execute overridden hooks.

Reuse the checked lifecycle policy parser for invocation-local approvals and the
runner's validated request/environment construction for explicit uncontained
execution. That execution returns termination only, without an enforcement
receipt or descendant-cleanup claim. Warn before execution, including JSON mode.

Keep every bypassed output out of the authenticated derived cache. Retain private
build stages through transactional materialization, then attempt to remove them. Existing
store attestations remain reserved for exactly approved, contained builds.

## Consequences

Developers can deliberately execute dependency hooks on unsupported hosts without
switching package managers. Unsafe execution can modify host files, read host
credentials, leave descendants, and defeat rollback assumptions. It is never an
automatic fallback. Normal installation remains fail closed for approved hooks
whose required containment is unavailable.

Contained Windows support requires native writable-filesystem validation and
secure store-local key handling. Contained macOS support remains a separate
backend problem. Neither is established by adding these overrides.
