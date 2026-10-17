# ADR 0009: Standalone tool environments and global command activation

Status: Accepted for implementation in issue #282.

## Context

Standalone CLI tools should not require a caller-created project or manual
symlinks. A shared global dependency graph couples unrelated tools' updates.
Project install transactions do not own a global command directory. Existing
root-script containment also requires an explicit policy and cannot be silently
replaced by ordinary global execution.

## Decision

Give each installed npm tool a separate private project and ordinary lockfile,
reusing verified installation and the shared cache. Export only its declared
direct-package bins. Persist requirements, versions, generations, command
mappings, and launcher ownership bytes in a versioned catalog.

Use stable command launchers that dispatch through Tapid and the selected
catalog. Serialize tool mutations and cleanup with the existing activation lock.
A separate catalog lock covers recovery, publication, and invocation lease
acquisition. Release it during staging and before runtime discovery or execution;
invocations wait up to 30 seconds for it, then report contention. The mutation
lock remains held during staging, so another writer cannot retire that generation.
Verify a new generation before publication. Record both catalogs in a bounded journal;
a digest-bound, separately synced commit decision selects the new state.
Recovery is idempotent and treats recorded launcher bytes as the only files it
may replace. Catalog replacement reuses the existing atomic lockfile writer,
with the tool journal covering its interruption window. Parent directory syncing
reuses the existing filesystem activation helper.

Keep old generations until commit and defer cleanup while a direct invocation
holds its environment lease. A settled catalog retains the journal digest so
recovery can distinguish a crash between journal removal and decision cleanup.
Unknown files and unowned/replaced launchers are preserved.

First initialization atomically creates and syncs an empty
`.tapid-tools-initializing-v1` directory before publishing the final ownership
file. An interrupted initialization may resume only with that non-link ownership
directory and regular, strictly named write temporaries; unknown entries still
block claiming the root. Existing temporary files are preserved. The final
ownership file is synced before the initialization directory is removed
nonrecursively. Unix command names reserve `node`, including case variants, so
adding the tool bin directory to PATH cannot replace implicit runtime discovery.
Listing resumes this owned initialization, including missing `bin` and
`environments` directories, but does not claim an empty unowned root.
Generation staging similarly syncs an empty
`.tapid-tool-staging-v1-<generation>` ownership directory before creating the
generation. Recovery can then complete its ownership file and reuse normal
retirement, while preserving unrelated markerless generations and rejecting
invalid ownership records or links.

Global commands run with normal user authority and an externally managed runtime.
They are not root-script sandbox requests. Lifecycle hooks remain disabled;
runtime downloads/setup require an explicit package command. No caller-project
configuration or lifecycle approvals are copied.

## Alternatives and consequences

A shared global project is simpler but allows one tool's update to change
another's dependency graph. Manual symlinks leave ownership and cleanup to the
user. Direct generation-specific shims make partial publication observable as
mixed versions; catalog dispatch provides a single selected mapping under the
mutation lock.

The catalog and recovery journal are a distinct CLI orchestration contract;
resolver, manifest, verified storage, linker collision rules, and ordinary
project activation remain in their existing owners. Launchers depend on the
Tapid executable path used to create them. Moving that executable can require
reinstalling/updating the tool to refresh owned launcher bytes.

Environment ownership markers are local management evidence, not an authenticity
boundary against a host process with write access. No-follow checks and
conservative deletion reject static traversal/reparse attacks; hostile concurrent
host path replacement is outside the guarantee. Leases do not supervise detached
descendants. Native backend tests, process-crash recovery cases, collision tests,
and sentinel preservation tests enforce the supported contract.
