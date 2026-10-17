# Working on execution containment

Follow the root instructions and read the relevant sections of [ADR 0005](../../docs/adr/0005-default-on-root-script-sandbox.md) and [platform validation](../../docs/platform-validation.md) before changing containment, supervision, grants, or enforcement receipts. Keep detailed platform status and probe requirements in those documents rather than copying them here.

## Code Review Rules

- Flag execution that starts before required authority or configured limits can be enforced. Unsupported requests must fail before untrusted spawn; preserve the existing request validation and checked evidence construction rather than adding an unsandboxed fallback.
- Keep requested, declared, observed, and enforced controls distinct. Launch evidence must match the exact request; completion evidence covers lifecycle and cleanup, not fresh confirmation of launch authority. Pathname resolution must not be presented as held native-object identity.
- Restricted does not guarantee descendant cleanup. ManagedTree additionally requires the documented descendant ownership, complete cleanup boundary, and tree-wide limit enforcement. Do not label process groups, PID scans, or best-effort cleanup as ManagedTree.

Use positive and negative native controls for changed enforcement claims, including descendant and bypass paths where applicable. Compilation, policy declarations, skipped probes, and allowed operations alone do not establish containment support. Include affected CLI consumers in verification when request construction or receipts change; report unavailable platform checks without upgrading support claims.
