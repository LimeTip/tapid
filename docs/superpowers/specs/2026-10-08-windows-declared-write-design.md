# Windows declared-write support: existing targets

## Approved scope

Follow merged PR #208 (`58cd976ec7234e0ba0c2120a3a354a854a2826b2`) with safe support for existing explicitly declared project write targets. The user approved existing-target writes first, then addressing networking/start. Tapid invokes Node; it is not a runtime. Issue #157 remains open until its complete integrated acceptance passes.

## Alternatives and decision

1. Existing-target support: reuse current AppContainer/Job/ACL lifecycle, reject missing targets. Selected as the smallest independently verifiable slice.
2. Secure missing-directory materialization: desirable later, but requires component-level binding, ownership and rollback semantics; deferred.
3. Private copied execution workspace: changes cwd, dependency/output identity and publication semantics; out of scope.

## Supported contract

Support only explicitly declared existing ordinary files/directories within the project. Preserve exact grant path, access, kind, source, and CanonicalPath binding receipts. CanonicalPath retains its documented trusted-host replacement assumption; do not claim Windows NativeObject binding. Verify target kind/identity against the opened object immediately before ACL mutation and reject observed changes.

Preflight must reject missing targets without creating them, unsafe reparse topology, cross-boundary hard-link aliases, protected unsupported descendants, unavailable host ACL authority, and any policy whose requested restrictions cannot be enforced. Validate the topology before making any ACL change. A conservative rejection is preferable to an unproven grant. Runtime read-access fallback must never authorize project writes.

Use per-execution AppContainer SID ACL grants only on declared targets. Do not modify volume-root, SystemRoot, shared ancestor, home, or shared-temp ACLs. Preserve inherited read authority where explicitly declared. Write-only grants must not imply read permission.

Write rights cover existing-file overwrite/append/truncate, creation and reopen, and nested directories only where declared. Determine rename, atomic replacement, unlink, and recursive removal requirements with real Next build/test behavior. If necessary, add narrowly scoped deletion authority on declared subtree objects/descendants, not full control, ownership, WRITE_DAC or ancestor deletion rights. Exact-file writes do not authorize replacing the parent directory entry or creating siblings. Unsupported mutation semantics remain explicitly limited.

Ordinary limited medium-integrity-account project writes must work without test-only integrity-label changes. If mandatory integrity prevents that, keep the write gate closed and require a separate approved production integrity design; do not silently lower labels.

## Lifecycle and rollback

Apply grants transactionally. On partial preparation failure, explicitly restore previously applied grants and surface cleanup failures; do not rely solely on Drop paths that suppress restoration errors. Never issue success evidence on rollback failure. Revocation removes this execution SID from current DACLs rather than replaying stale snapshots, preserving unrelated concurrent ACE changes and protection state.

Keep the managed Job owned until its process tree is empty, then revoke grants and remove the AppContainer profile. Confirm revocation reaches preexisting and child-created files/directories, including overlapping grant cases. Verify same-SID access is denied after revocation. Normal completion, nonzero exit, timeout, output limit, cancellation, missing executable, actual child-creation failure and partial preparation failure need native evidence. Forced termination, power loss and external crash recovery remain outside this slice.

## Validation

Use TDD and Windows 11 Pro x64 VM126 as interactive limited tapidtest. Matching Rust tools, verified standalone Node, exact source/artifact digests, bounded native harnesses and retained logs are required. Hosted windows-latest complements but does not substitute for this acceptance.

Positive probes: existing overwrite/append/truncate, new file/create/reopen, nested create, Unicode/spaced paths, descendant writes, direct Node and integrated CLI. Mutation probes cover rename/replacement/delete and repeated build/test, with explicit supported or rejected outcomes.

Negative probes: read-only sibling, parent, outside sibling tree, home/shared temp/SystemRoot/volume root, exact-file sibling creation, junction/symlink/reparse descendants, dangling links, outside hard-link sentinel, changed target identity, missing target, protected ACL and denied WRITE_DAC. Pair denial payloads with uncontained positive controls where needed. Require absent prelaunch markers for rejection.

Compare full DACL bytes/control bits after cleanup on compatible disposable fixture baselines; do not mask mismatches or normalize shared host directories. Preserve the documented arbitrary legacy inheritance-metadata limitation. Test unrelated ACE preservation, read-root/write-child and overlapping grants, concurrent executions, mutex failure, and empty Job before restoration. Existing test-only inheritance initialization must stay limited to disposable roots.

Independently verify Restricted authority and ManagedTree tree/resource/completion dimensions; the news fixture explicitly uses Restricted and cannot establish ManagedTree guarantees by itself. Retain environment allowlist/secret exclusion, filesystem/network denial and resource/cancellation regression owners.

## Integrated news-site acceptance

Install Windows dependencies using Tapid in a disposable fixture copy, record install/replay separately, and explicitly precreate .next and .tmp. Run integrated tapid run build and test twice, checking real artifacts, test output, exact exits, outside sentinels and dependency immutability. Supply only approved scoped environment values.

Run unchanged tapid run start only to verify present prelaunch rejection: network=true remains unsupported. Do not substitute npm run, disable its network request, exempt loopback or label blocked readiness as success. Implementing and verifying network capabilities/start readiness is a later design slice. No external listener or public ingress is authorized here.

## Delivery and completion

Use isolated fork-first branch/worktrees, preserve user checkout, commit/push progress to a follow-up PR, never merge or approve automatically. Update only documentation/contracts whose supported behavior changes. Enable the write gate only after the supported write and cleanup matrix passes native acceptance. If a required property fails, retain the gate and report the blocker.

This slice is complete only with native declared-write confinement/rollback evidence, integrated real build/test acceptance, accurate limitations, and successful exact-head hosted CI. It does not close #157; missing-directory materialization and network/start remain deferred, with network-enabled policies fail-closed.
