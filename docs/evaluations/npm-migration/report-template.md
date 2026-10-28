# Application migration trial: APPLICATION

Status: not executed. Replace placeholders with observed evidence; never infer success from a prior phase or report.

Record source URL, immutable revision, license, selection rationale, documented prerequisites, Tapid revision/binary SHA-256, Node/npm versions, OS/architecture, isolated checkout/store setup and original manifest/lock hashes. Include rejected candidates and reasons.

| Check | Command and cwd | Exit/readiness | Evidence and limits |
| --- | --- | --- | --- |
| npm baseline install | | | Normal lifecycle behavior |
| npm build/test/start | | | Bound duration and readiness criterion |
| Local compatibility inspection | | | Explicitly list unchecked areas |
| Offline import and lock verification | | | Unsupported pointer, identity, field/reason |
| Frozen hydration | | | Exact pinned downloads; hooks disabled |
| Warm offline-frozen replay | | | Same isolated store |
| Semantic graph comparison | | | Separate receipts and platform differences |
| Tapid-tree build/test/start | | | Same runtime/tools as baseline |
| Independent dependency add | | | Exact target and intended changes |
| Independent named update | | | Exact target and intended changes |
| Activated-tree rollback | | | Restored matching root/member state, npm ci, repeated check |

For every executed command retain arguments, cwd, environment variable names (no secrets), exit/signal, bounded output, start/readiness deadline and outcome. Attach bounded machine-readable graph reports. For unsupported import retain non-mutation evidence for manifests, both locks, store and installed trees. Dependent unexecuted checks must say why.

For maintenance retain pre/post semantic selections and requiring edges; explain each intended direct and necessary transitive change and check all unrelated compatible selections. Record native peer/member conversion limits without fallback resolution. Rejected import recovery is not activated-tree rollback.

Conclude with separate import, parity, application, add, update and rollback results and linked remaining issues. A successful import does not certify hook execution, dependency maintenance, production support or deployment. Tapid manages packages; the application's runtime and deployment tools retain their roles.
