# CLI workflow scope and ownership

This ledger records the bounded coordination slice of [#351](https://github.com/LimeTip/tapid/issues/351). It does not implement the child backlog or close the tracking issue. Detailed current behavior remains in the [compatibility matrix](compatibility.md), [CLI README](../crates/tapid-cli/README.md), and [lifecycle contract](dependency-lifecycle.md).

Inspected on 2026-10-28 at source revision `75ea37760158ce8d95eb94c0ac736359185d784a`, after base synchronization. Retrieved #351 and all twelve child bodies, and listed open PRs using GitHub's dedicated pull-request endpoint. All twelve children were open. PR discovery is not an exhaustive diff review; PR descriptions and reported checks are not checkout validation receipts. The earlier audit at `13c875ee4b1e754d8f4e1d62061aefd194f816e4` is historical evidence, not this baseline.

Evidence fields are independent:

- **Planned:** the linked child owns the bounded outcome and its full acceptance criteria.
- **Source:** the baseline below is present at the inspected revision; partial foundations do not deliver the planned workflow.
- **Executed:** no runtime or interoperability suite was run for this prose-only reconciliation. Nearby tests are inspection references, not newly successful execution evidence.
- **Production:** pending for every entry. No release/platform tuple is promoted; [#158](https://github.com/LimeTip/tapid/issues/158) and the [production adoption gate](production-adoption.md) retain that authority.

These evidence definitions, inspection date, revision and gaps apply to every workflow below. Open draft PRs are competing work to reconcile before implementation, not dependencies on their merge. No assignee or deadline is inferred.

## Twelve workflow owners

### Install profiles — [#339](https://github.com/LimeTip/tapid/issues/339)

**Source:** [install arguments](../crates/tapid-cli/src/commands/install.rs) expose offline, frozen and preview behavior, but no dependency-kind profile; [manifest kinds](../crates/tapid-manifest/src/model.rs) already distinguish dependency sections. [CI](../crates/tapid-cli/src/commands/ci.rs) uses exact locked installation. **Disposition:** planned; draft [PR #400](https://github.com/LimeTip/tapid/pull/400) is open, not delivered here.

**Outcome and dependencies:** separate complete selections from materialization, define explicit dev/optional inclusion, overlapping declarations, peers and workspace scope; never silently infer a mode from NODE_ENV. Reuse resolution, lock and activation ownership; imported formats must supply sufficient edge facts or fail before mutation. **Required evidence:** full/production/full transitions, shared required paths, aliases, peers, workspaces, cold CI, replay and crash recovery; omitted-only artifacts/hooks must be skipped before fetching/execution. Execution and release/platform evidence remain pending.

### Lock-only edits — [#340](https://github.com/LimeTip/tapid/issues/340)

**Source:** [mutation orchestration](../crates/tapid-cli/src/application/lifecycle.rs) and [previews](../crates/tapid-cli/src/online/preview.rs) exist; [install arguments](../crates/tapid-cli/src/commands/install.rs) have no lock-only write mode. **Disposition:** planned; draft [PR #401](https://github.com/LimeTip/tapid/pull/401) is open.

**Outcome and dependencies:** reuse compatible locked preferences, save policies, stale-input checks and [project recovery](../crates/tapid-cli/src/filesystem/lifecycle_journal.rs). Define supported schemas and pending tree receipts honestly; make manifest/lock writes recoverable together without hooks or node_modules activation. **Required evidence:** stale inputs, targeted updates, copied sources, workspace edits, failure preservation, interrupted writes and later CI/offline hydration; document metadata, artifact and store effects. Runtime and production evidence remain pending.

### Exact prefetch — [#341](https://github.com/LimeTip/tapid/issues/341)

**Source:** [locked hydration](../crates/tapid-cli/src/online/locked.rs) already verifies pinned artifacts; [cache commands](../crates/tapid-cli/src/commands/cache.rs) inspect/evict rather than provide hydrate-only preparation. **Disposition:** planned; draft [PR #402](https://github.com/LimeTip/tapid/pull/402) is open.

**Outcome and dependencies:** reuse hydration, routing, archive validation and verified-store publication without resolution, hooks or activation. Settle target/schema support, imported/copied inputs and whether receipt-only lock writes are permitted. Source cache does not supply authenticated lifecycle outputs or cold-CI keys/toolchains ([#274](https://github.com/LimeTip/tapid/issues/274)). **Required evidence:** subsequent offline/frozen installation, integrity/origin failures, warm/cold counts, contention/interruption and unchanged project layout; share actual counters with [#268](https://github.com/LimeTip/tapid/issues/268). Execution and support remain pending.

### Runtime requirements — [#342](https://github.com/LimeTip/tapid/issues/342)

**Source:** [manifest model](../crates/tapid-manifest/src/model.rs) preserves extra fields rather than providing a typed engines contract; [registry models](../crates/tapid-registry-client/src/models.rs) and [run runtime selection](../crates/tapid-cli/src/commands/run.rs) are separate concerns. **Disposition:** planned; draft [PR #403](https://github.com/LimeTip/tapid/pull/403) is open.

**Outcome and dependencies:** model root/member/package engines in their owning capabilities, reuse semver, define warnings versus strict preflight and packageManager/devEngines subsets. Missing runtime is unknown; do not inspect by executing project-local Node or trigger Corepack/downloads. Inspection under [#299](https://github.com/LimeTip/tapid/issues/299) is not enforcement. **Required evidence:** malformed/prerelease ranges, absent/multiple runtimes, native/imported locks, workspaces, offline facts and strict rejection before hooks/activation with no mutation. Execution and support remain pending.

### Optional failures — [#343](https://github.com/LimeTip/tapid/issues/343)

**Source:** [resolution](../crates/tapid-cli/src/online/resolution.rs), [platform filtering](../crates/tapid-cli/src/online/platform.rs) and [nearby tests](../crates/tapid-cli/src/online/tests.rs) cover existing optional aliases/routes and peer facts. The compatibility matrix explicitly excludes complete optional-failure semantics. **Disposition:** planned; draft [PR #404](https://github.com/LimeTip/tapid/pull/404) is open.

**Outcome and dependencies:** inventory existing tests first; distinguish metadata, platform, download, archive, integrity, unsupported-spec and hook failures. Required reachability wins; identity/integrity/containment failures cannot silently become success. Persist accepted omissions and target/profile facts for deterministic replay and inspection, coordinating peers [#306](https://github.com/LimeTip/tapid/issues/306), hooks and graph ownership. **Required evidence:** diamonds, nested branches, aliases/peers, workspace/native/imported inputs, warm/cold replay, rollback/recovery and pinned npm comparison with deliberate differences. Execution and support remain pending.

### Targeted overrides — [#344](https://github.com/LimeTip/tapid/issues/344)

**Source:** [manifest parsing](../crates/tapid-manifest/src/parse.rs) and [online interpretation](../crates/tapid-cli/src/online/resolution.rs) support flat rules; [tests](../crates/tapid-cli/src/online/tests.rs) reject version-qualified selectors and conflicting direct overrides. Preserved text is not supported interpretation. **Disposition:** planned; draft [PR #405](https://github.com/LimeTip/tapid/pull/405) is open.

**Outcome and dependencies:** one bounded parent/version selector model across resolution, previews and edits, with explicit precedence and direct restrictions. Preserve aliases, registry/source identity, peers and optional edges; coordinate [#238](https://github.com/LimeTip/tapid/issues/238), peer/workspace and graph owners. **Required evidence:** competing parents, scoped aliases, duplicate versions, conflicting selectors, peers/workspaces, unchanged frozen replay and explicit imported/native re-resolution boundaries. Execution and support remain pending.

### Local directories — [#345](https://github.com/LimeTip/tapid/issues/345)

**Source:** [copied sources](../crates/tapid-cli/src/online/copied.rs) and [workspace discovery](../crates/tapid-manifest/src/workspace.rs) provide distinct foundations. [Compatibility](compatibility.md) excludes live directories and copied declarations in members/transitive metadata. **Disposition:** planned; draft [PR #406](https://github.com/LimeTip/tapid/pull/406) is open.

**Outcome and dependencies:** evaluate snapshots versus explicit links, preserve mutable-source identity, and reuse archive/store/linker/transactions rather than bypass verification. Specify contained paths, external policy, symlink/reparse races, cycles, bins and lifecycle restrictions with existing source/workspace owners. **Required evidence:** changed same-version content, member declarations, unsafe paths, Node imports/bins, offline/frozen replay and native-platform rollback. No implicit preparation hooks or new script authority. Execution and support remain pending.

### Effective/corporate configuration — [#346](https://github.com/LimeTip/tapid/issues/346)

**Source:** [registry configuration](../crates/tapid-cli/src/registry.rs), [routing](../crates/tapid-cli/src/online/routing.rs) and the [transport contract](../crates/tapid-registry-client/README.md) provide project configuration, environment-token references, bounded requests and exact-origin credentials. Full npmrc is unsupported; standalone tools do not read caller project configuration. **Disposition:** planned; draft [PR #407](https://github.com/LimeTip/tapid/pull/407) is open.

**Outcome and dependencies:** explicit layer precedence, redacted provenance, bounded npmrc decision and proxy/custom-CA behavior using existing transport. Never bypass TLS or forward credentials to another origin; hosted identity [#314](https://github.com/LimeTip/tapid/issues/314) is separate. **Required evidence:** fake-environment/local-transport precedence, hostile configuration, secret redaction, proxy/origin mismatch, CA failures, offline limits and headless provisioning. Execution and support remain pending.

### Multi-member scripts — [#347](https://github.com/LimeTip/tapid/issues/347)

**Source:** [run command](../crates/tapid-cli/src/commands/run.rs) selects one exact workspace member and retains root policy; [planning](../crates/tapid-cli/src/run.rs) and [planning tests](../crates/tapid-cli/tests/run_planning.rs) own execution preparation. **Disposition:** planned; no directly titled implementation PR was identified in the open-PR listing. Selection PRs [#373](https://github.com/LimeTip/tapid/pull/373) and [#374](https://github.com/LimeTip/tapid/pull/374) overlap foundations, not scheduler acceptance.

**Outcome and dependencies:** build on [#229](https://github.com/LimeTip/tapid/issues/229)/[#231](https://github.com/LimeTip/tapid/issues/231), define filters/root inclusion, missing scripts, topological order and cycles. Start deterministically sequential; parallelism requires demonstrated need and bounded semantics. **Required evidence:** per-member cwd and checked runtime/containment, trusted results [#199](https://github.com/LimeTip/tapid/issues/199), ordering/failure policy, cancellation and actual descendant-termination limits on supported backends. Execution and support remain pending.

### npm-compatible pack — [#348](https://github.com/LimeTip/tapid/issues/348)

**Source:** [tapid-publish interface](../crates/tapid-publish/src/lib.rs) emits `TAPID-PACK-1`; its [README](../crates/tapid-publish/README.md) documents captured-byte binding, limits and concurrent-writer limitations. This is not an npm tarball, and the [command tree](../crates/tapid-cli/src/commands/mod.rs) has no pack command. **Disposition:** planned; draft [PR #408](https://github.com/LimeTip/tapid/pull/408) is open.

**Outcome and dependencies:** extend packing ownership with explicit output and preview, npm layout, files/ignore/default exclusions, executable modes and path rules. Reuse archive safety [#234](https://github.com/LimeTip/tapid/issues/234). **Required evidence:** bounded captured-byte inventory/digests, unsafe paths/output failures, stable-source limitations and pinned offline npm inspect/install interoperability. Never implicitly run prepack/prepare/postpack or publish; automatic secret detection is not promised. Execution and support remain pending.

### Generated completions — [#349](https://github.com/LimeTip/tapid/issues/349)

**Source:** the maintained [Clap definitions](../crates/tapid-cli/src/commands/mod.rs) generate help but contain no completion command. **Disposition:** planned; both draft [PR #366](https://github.com/LimeTip/tapid/pull/366) and draft [PR #409](https://github.com/LimeTip/tapid/pull/409) are open. Reconcile competing implementations before adding work; neither is delivery at this revision.

**Outcome and dependencies:** generate static shell scripts offline from Clap without another inventory, project/config/store reads, dynamic registry calls, profile editing or shell execution. Coordinate discovery [#195](https://github.com/LimeTip/tapid/issues/195) and PATH [#37](https://github.com/LimeTip/tapid/issues/37). **Required evidence:** advertised Bash/Zsh/Fish/PowerShell subset, nested commands/aliases/flags, UTF-8 and output failures, machine-mode interaction, manual installation and no project/home mutation. Execution and support remain pending.

### Approved rebuild — [#350](https://github.com/LimeTip/tapid/issues/350)

**Source:** [install orchestration](../crates/tapid-cli/src/application/install.rs) and the [lifecycle contract](dependency-lifecycle.md) own default-denied hooks, exact approvals and authenticated derived outputs. There is no explicit rebuild command in the maintained command tree. **Disposition:** planned; draft [PR #410](https://github.com/LimeTip/tapid/pull/410) is open.

**Outcome and dependencies:** first test whether existing install/update can safely rebuild selected unchanged locked instances; add syntax only if needed. Reuse approvals, toolchain identity and verified outputs, with [#273](https://github.com/LimeTip/tapid/issues/273), [#274](https://github.com/LimeTip/tapid/issues/274), [#275](https://github.com/LimeTip/tapid/issues/275) and [#276](https://github.com/LimeTip/tapid/issues/276) platform gates. Inspection [#310](https://github.com/LimeTip/tapid/issues/310) and inactive export [#291](https://github.com/LimeTip/tapid/issues/291) grant no execution. **Required evidence:** selected/dependant order, cycles, imported restrictions, offline/cold authentication, changed toolchains, stale approvals, failure/cancellation/crash preservation and one recoverable activation. Unsafe invocation outputs stay separate from reusable approved outputs. Execution and support remain pending.

## Fourteen product decisions

These dispositions record the selected scope, not new feature promises. A later accepted missing workflow needs a bounded issue owner before implementation.

| Category | Disposition, rationale and limits | Ownership and concrete revisit trigger |
| --- | --- | --- |
| Dependency patches | Defer: exact source/patch digests, derived identity, licence handling and approval invalidation need a coherent contract. Keep original verified sources immutable; never edit store trees ad hoc. | [#300](https://github.com/LimeTip/tapid/issues/300)/[#315](https://github.com/LimeTip/tapid/issues/315); revisit a pinned consumer that needs a checked-in patch and offline replay. |
| Deduplication | Defer graph-changing dedupe until identity-aware measurements show duplicate cost. Physical materialization optimizations do not authorize graph changes; preserve peer/registry/platform identity. | [#241](https://github.com/LimeTip/tapid/issues/241)/[#242](https://github.com/LimeTip/tapid/issues/242) retain physical optimization; revisit representative measured duplicate graphs. |
| Additional migrations | Retain bounded npm v3 import; defer pnpm/Yarn/npm v1–v2. Never silently approximate missing graph/source/peer information. | [#151](https://github.com/LimeTip/tapid/issues/151), with [#307](https://github.com/LimeTip/tapid/issues/307); revisit pinned pilot fixtures demonstrating representability and demand. |
| Cross-target installs and portable locks | Defer until OS/CPU/libc selection and multi-target graph representation are defined. Host locks are not universal; never execute target hooks on an incompatible host. | #339/#341/#343 and #274 above; revisit an explicit cross-target consumer with target fixtures. |
| Bundled dependencies | Defer broader claims: archive integrity alone cannot supply graph identities for package-contained node_modules. Define preserve/validate/reject before support. | #234/#300 and #151 above; revisit a pinned pilot inventory of bundledDependencies/bundleDependencies. |
| Extended workspace/source syntax | Retain documented forms; defer recursive globs, catalogs, additional workspace protocols, transitive/member copied sources, authenticated Git, submodules and preparation hooks. A directory workflow does not accept them all. | #345 and workspace/source owners; revisit each reproduced unsupported declaration with a bounded owner. |
| Registry search | Defer discovery beyond exact metadata; no safety inference from search results. Private-registry disclosure, endpoint support and bounded pagination need explicit rules. | #299 above; revisit demonstrated discovery demand not met by exact inspection. |
| Ephemeral remote execution/dlx | Defer until local binaries and standalone tools are evaluated. Exclude implicit remote fallback from missing local commands. Future execution requires explicit intent, pinned identity, approval/containment and cleanup. | [#298](https://github.com/LimeTip/tapid/issues/298); revisit a pinned workflow those local tools cannot satisfy. |
| Publication | Defer npm-compatible and native-registry publication as separate contracts. Local packaging grants no publication authority. Auth/2FA/OIDC, scope, exact preview, immutable identity, retries/unknown outcomes and explicit authorization remain design work. | #348 is packaging only; [#178](https://github.com/LimeTip/tapid/issues/178) Rust release work is unrelated. Revisit an explicit registry-specific publishing requirement and create a bounded owner. |
| Inventory exports | Defer CycloneDX/SPDX/licence reports until a validated offline graph/source projection is sufficient. Partial graphs remain partial; declared licence/provenance is not verified legal/security evidence. | #300 above; revisit a concrete offline export consumer and required fields. |
| New-release selection delay | Defer until missing timestamps, exceptions, cached/offline behavior and locked-version retention have explicit outcomes. Registry timestamps are not safety evidence. | #315 above; revisit a concrete minimum-age/cooldown policy use case. |
| CLI accessibility/automation | Accept a bounded gap inventory, not an exhaustive audit or new implementation here. Inspect NO_COLOR/terminal controls, non-TTY output, cancellation exits, help/aliases and output channels; file only reproduced defects. | [#195](https://github.com/LimeTip/tapid/issues/195)/[#199](https://github.com/LimeTip/tapid/issues/199)/[#200](https://github.com/LimeTip/tapid/issues/200)/[#201](https://github.com/LimeTip/tapid/issues/201); act on reproducible failures. Completions stay #349. |
| Cache portability and pruning | Retain explicit eviction. Defer authenticated export/import and reachability GC; source/derived identity, host/key restrictions, active leases and recovery require design. Cache clean is not GC. | #341 and [#308](https://github.com/LimeTip/tapid/issues/308); revisit measured disk needs after exact prefetch. |
| Runtime breadth | Retain documented Node-backed scope; defer Deno/Bun claims. Runtime management, application deployment and registry hosting remain separate product choices. | #351 coordinates scope; revisit pinned Deno/Bun consumers with explicit accepted subsets and bounded owners. |

## Existing ownership and sequencing

Preserve the tracking issue's ownership groups rather than duplicate them:

- Integrity/recovery: [#234](https://github.com/LimeTip/tapid/issues/234), [#237](https://github.com/LimeTip/tapid/issues/237), [#252](https://github.com/LimeTip/tapid/issues/252), and competing-store verification owners. Open draft [PR #376](https://github.com/LimeTip/tapid/pull/376) is relevant competing work, not proof of verification.
- Peers/workspaces: [#306](https://github.com/LimeTip/tapid/issues/306), [#230](https://github.com/LimeTip/tapid/issues/230), [#229](https://github.com/LimeTip/tapid/issues/229), [#231](https://github.com/LimeTip/tapid/issues/231).
- Adoption/migration: [#151](https://github.com/LimeTip/tapid/issues/151), [#196](https://github.com/LimeTip/tapid/issues/196), [#307](https://github.com/LimeTip/tapid/issues/307), [#166](https://github.com/LimeTip/tapid/issues/166).
- Inspection/machine contracts: [#189](https://github.com/LimeTip/tapid/issues/189), #195/#199/#200/#201 above, [#267](https://github.com/LimeTip/tapid/issues/267), [#268](https://github.com/LimeTip/tapid/issues/268), [#290](https://github.com/LimeTip/tapid/issues/290), #298/#299/#300/#310 above.
- Lifecycle/platform assurance: [#155](https://github.com/LimeTip/tapid/issues/155), [#156](https://github.com/LimeTip/tapid/issues/156), [#157](https://github.com/LimeTip/tapid/issues/157), #273–#276 above. Rebuild is not another approval engine.
- Distribution/support/performance/onboarding: [#3](https://github.com/LimeTip/tapid/issues/3), #37 above, [#115](https://github.com/LimeTip/tapid/issues/115), [#117](https://github.com/LimeTip/tapid/issues/117), [#123](https://github.com/LimeTip/tapid/issues/123), [#124](https://github.com/LimeTip/tapid/issues/124), [#126](https://github.com/LimeTip/tapid/issues/126), #158/#308 above, [#309](https://github.com/LimeTip/tapid/issues/309).
- JSR stays [#165](https://github.com/LimeTip/tapid/issues/165). Security evidence/audit/hosted integration/policy stay [#312](https://github.com/LimeTip/tapid/issues/312), [#313](https://github.com/LimeTip/tapid/issues/313), [#314](https://github.com/LimeTip/tapid/issues/314), [#315](https://github.com/LimeTip/tapid/issues/315). Local package management gains no hosted-service prerequisite.

Measured/deferred batches [#194](https://github.com/LimeTip/tapid/issues/194), saved plans [#197](https://github.com/LimeTip/tapid/issues/197), histories [#198](https://github.com/LimeTip/tapid/issues/198), adapters [#202](https://github.com/LimeTip/tapid/issues/202), policy export #291 and constraints [#301](https://github.com/LimeTip/tapid/issues/301) retain their owners. AI adoption stays with [#203](https://github.com/LimeTip/tapid/issues/203), portable consumer workflow [#352](https://github.com/LimeTip/tapid/issues/352), focused queries [#353](https://github.com/LimeTip/tapid/issues/353), and reproducible sandbox/CI recipes [#354](https://github.com/LimeTip/tapid/issues/354); no separate agent resolver or authority model is selected.

Continue [#311](https://github.com/LimeTip/tapid/issues/311)'s integrity/recovery lane and the first pinned #307 pilot. Use reproduced pilot failures to order #339/#342/#343 alongside peer/workspace blockers. Follow with #340/#341 on existing resolution/hydration/transaction paths and #344 for concrete maintenance cases. Let pilot demand select #345/#346; #350 follows platform gates. #347–#349 remain independently deliverable developer/library-author workflows, not universal production-install prerequisites.

## Acceptance evidence still required

This ledger supplies ownership and decisions, not exact release/platform acceptance. Each child still needs its own source-matched execution evidence, commands/results, supported runtime/platform tuple and limitations. A merged PR alone does not supply that record. Pilot evidence stays under #307; lifecycle/platform evidence stays under #273–#276; production promotion stays solely under #158 and the production adoption gate. Do not close #351 from this inventory alone or infer exhaustive future compatibility.

The next independent implementation slice is selected from an existing child after reconciling its current PR and reproducing its first behavior test. No child implementation is authorized by this documentation change. Local validation of this slice follows the [prose-only lane](testing.md): review links, source/issue references, command claims, twelve-workflow/fourteen-decision coverage and support consistency. Runtime success is not claimed.
