## Peer dependency behavior

Manifest and registry parsing preserve `peerDependencies` separately from regular dependencies. `tapid add --peer <name>@<range>` records only a manifest declaration; it is not itself installed or checked as an ordinary dependency. Registry-package peers are checked against compatible direct project roots from the same registry. Workspace member peers are checked against either a matching local workspace member or a compatible direct project root; missing or incompatible providers fail before project mutation and are revalidated during frozen/offline replay. Registry peer provider versions are stored in the lockfile and linker peer context; workspace peer providers are validated against persisted workspace or root identities.

This bounded model does not implement nested/ancestor peer lookup, multiple placements of the same package under different peer contexts, or full npm/pnpm automatic peer placement. The consuming project's own `peerDependencies` remain declarations and are not installed as roots; their presence does not prove that the consuming runtime supplies them.

# Compatibility matrix

This matrix distinguishes current implemented behavior from accepted target contracts that are explicitly marked pending. Supported means covered by relevant local contract or runtime tests; an ADR or configured CI job alone is not support evidence. It does not mean full npm compatibility or production readiness.

| Contract | Current behavior | Stable limitation |
|---|---|---|
| Manifest | Parses selected npm-shaped fields, dependency maps, scripts, string or object `bin` metadata, and simple root `overrides` maps; lifecycle mutations preserve unknown fields and dependency kinds. Workspace discovery supports root `workspaces` string or array declarations and `workspaces.packages` string or array declarations with the implemented glob subset; duplicate member names, symlinks to non-regular targets, and paths escaping the canonical root are rejected; symlinks to regular files are ignored because they cannot be workspace directories. Root install recognizes local direct dependencies with ordinary semver or `workspace:*`, `workspace:^`, and `workspace:~`; it resolves member registry dependencies transitively and validates member peers against local members or compatible direct root providers. Unsupported `workspace:` forms and missing or incompatible local targets fail closed before mutation | Glob behavior is narrower than npm's full minimatch semantics. `--workspace <name>` selects a member manifest for install and lifecycle mutations, while the workspace root remains the lockfile, store-coordination, and activation project. The selected member is the mutation target; root `package.json` continues to define direct resolution roots and root overrides for install and replay. Selected-member lifecycle mutations resolve sibling packages locally and never fall back to a registry. Nested/package-specific override selectors and non-string override values are unsupported |
| npm registry | Reads npm `versions`, validates package/version identity and HTTPS tarballs, and requires a valid SHA-512 `dist.integrity` by default; `tapid install --allow-unverified-registry-artifacts` is an explicit interactive compatibility escape hatch | The escape hatch is not allowed with `--offline` or `--frozen`, emits a warning, and does not provide registry-declared artifact authentication; npm `.npmrc` behavior, tags, git and file dependencies, and complete packument behavior are unsupported; configure private registry routes and exact-origin credentials through Tapid's explicit registry configuration |
| JSR registry | Accepts scoped metadata and semver versions; preserves JSR regular and peer metadata separately through registry artifacts; accepts an artifact only with explicit HTTPS `npm.tarball` and valid SHA-512 `npm.integrity` | Live JSR installation and integrity behavior are not verified; no derived or transport-only integrity is accepted |
| Resolution | Deterministic exact-identity graph selection; simple npm root overrides replace matching transitive regular or optional dependency ranges; range satisfaction is differentially tested against pinned `node-semver` 7.8.5 for exact/partial versions, `x`/`*` wildcards, comparators and intersections, caret/tilde including zero-major bounds, hyphen ranges, OR ranges, prerelease eligibility, and build metadata; distinct parents may select different versions of one transitive package; compatible npm optional dependencies are selected using bounded `os`, `cpu`, and `libc` metadata; registry peer requirements are checked against compatible direct project roots and peer provider versions are retained in peer contexts. Workspace packages have distinct identities, resolve locally without registry fallback, and member registry dependencies are recursively resolved and recorded as member edges. Direct declarations that map one local install name to conflicting registry or workspace identities are rejected before mutation; registry origin remains part of package identity | Direct requirements from multiple workspace members share resolver constraints, so incompatible ranges for the same registry/package fail instead of producing member-specific versions. Glob and `workspace:` compatibility are bounded. Nested/package-specific override selectors and overrides of direct dependencies unless the range is identical are unsupported. Other `node-semver` edge cases beyond the pinned compatibility contract and full npm CLI/package-specifier behavior are not guaranteed; nested/ancestor peer-provider lookup, multiple peer contexts for the same exact package instance, automatic peer placement, and all optional-dependency failure semantics remain incomplete; stable-range selection does not select prerelease candidates. `outdated` recognizes ordinary unaliased workspace dependency specs; aliased or prefixed workspace declarations are not fully reported yet |
| Online install | Resolves npm metadata, downloads and verifies archives, writes `tapid.lock`, stages verified trees for rollback, and atomically activates `node_modules`; store publication commits only after activation succeeds; durable project/store journals recover pre-commit crashes by rolling back and post-commit crashes by finishing cleanup; schema 7 records separate workspace package identity, registry integrity provenance, and peer contexts. Root workspace installs resolve member registry dependencies recursively, persist member edges, validate peers against local or direct-root providers, and stage local links for activation | Lifecycle scripts never run; `add`, `remove`, `update`, read-only-during-normal-operation `outdated`, and atomic managed-tree `prune` are available. Pre-commit resolution, integrity, archive, peer, workspace, or materialization failures restore prior project/store state when rollback succeeds; typed outcomes distinguish failed recovery and committed changes with cleanup pending. Windows junction behavior and broad cross-platform workspace-link verification remain unverified. The compatibility escape hatch records locally computed integrity and offline/frozen replay rejects it. Static path/symlink escapes are rejected and detected failures roll back, but pathname-based lifecycle recovery can race a concurrent local path substitution; containment is not race-free under concurrent filesystem mutation |
| Offline and frozen install | Requires a lockfile, matching root manifest digest, verified registry store trees, and valid `.tapid-tree` markers; schema 7 persists exact registry roots, workspace source identities, and integrity provenance; replay rebuilds local links and reconstructs member registry edges and peer providers from current contained manifests, validates them against the lock, and performs no network lookup | Schema 6 remains readable for registry-only locks; schema 5 requires online regeneration because it lacks the schema 6 provenance contract. Frozen uses the same replay path as offline and is not complete npm frozen-lockfile policy |
| `.bin` | Package `bin` metadata is planned and materialized from verified regular files; Unix symlinks and Windows `.cmd` plus PowerShell wrappers | Other platforms are unsupported; collisions, unsafe targets, and missing targets fail closed |
| Root scripts | Default-on fail-closed CLI, explicit `assurance = "restricted"`, and legacy-safe omitted assurance as ManagedTree are implemented. Native macOS Restricted uses sampled native controls; Linux Restricted uses Landlock, `no_new_privs`, seccomp, and explicit environment/descriptor setup. Local `.bin` commands precede runtime tools, while a byte-verified private Node snapshot on a distinct inode preserves runtime selection; runtimes under project write authority are rejected. Human and JSON receipts include authority and completion evidence. Post-separator arguments remain opaque | Native ManagedTree, configured resource limits, narrower network scopes, and `--no-sandbox` remain unsupported. Targeted Ubuntu 24.04.5 x86_64 local-VM consumer and runner tests pass on code commit `3dd67f75c8d6f4187f8f1145f4a615939472e1c4`; the broader common Restricted probe matrix remains incomplete. Scripts can use all granted authority; host writes or races after final validation remain outside Restricted containment |
| Archive | Bounded hostile-path, duplicate, case-collision, symlink, and special-file validation; materialization accepts a direct package root or exactly one named top-level npm wrapper containing `package.json` | Missing or ambiguous package roots fail closed; malware scanning and executable analysis are outside the crate |
| Store and lockfile | SHA-256 content-addressed staging, executable-aware tree identity, exact tree replay, advisory replay leases, canonical lockfile schema 7 with exact registry roots, separate root-relative workspace identities, registry-integrity provenance, and peer-context identities; controlled schema 4 and 6 read compatibility, rollback-capable staged tree publication under cross-process locking with durable lifecycle coordination, and recoverable atomic managed activation | Schema 5 requires online regeneration; schemas earlier than 4 are rejected; schema 6 remains registry-only, workspace members are not represented as verified registry artifacts, and no remote cache, garbage collection, or full npm lockfile graph exists |
| Platforms | Experimental macOS Restricted is implemented using deprecated/private native Seatbelt APIs and path bindings. Linux Restricted is implemented using Landlock and seccomp. Support is gated by native positive/negative controls and exact integrated-revision acceptance | Native ManagedTree and Windows native containment remain unsupported; targeted Ubuntu 24.04.5 x86_64 local-VM consumer and runner tests pass on code commit `3dd67f75c8d6f4187f8f1145f4a615939472e1c4`; the broader common Restricted probe matrix remains incomplete. Local tests on a dirty worktree are not release or cross-platform certification. See platform-validation.md |

## Persisted registry identity compatibility

New registry inputs are canonicalized as HTTPS origins: lowercase/IDNA hosts,
canonical IP literals, no default `:443`, and no trailing root slash. Userinfo,
queries, fragments, and non-root paths are rejected. Non-default ports remain
part of identity.

Persisted registries must already have that canonical spelling in package records,
map keys, roots, and dependency references, including schema 4 locks. Older builds
could emit uppercase hosts or explicit default ports (for example
`https://REGISTRY.example.test:443`). Those locks now fail with
`NonCanonicalRegistryIdentity`, rather than being silently rekeyed. Distinct legacy
entries can normalize to one identity with different artifacts or graph edges;
normalizing only the keys also disconnects exact roots and dependencies. No
byte-preserving legacy replay or automatic lockfile migration is provided.

### Deliberate recovery

1. Stop concurrent project installs. Preserve a separate, byte-for-byte backup of
   `tapid.lock` in a new, non-existing backup location; verify the copy before
   continuing. Keep it until the replacement graph is approved. Do not rely on
   Tapid's temporary transactional backup, which is discarded after success.
2. Review the manifest and registry routing. Run `tapid install --project-dir PATH`
   **without** `--offline` or `--frozen` (and with the intended `--store-dir` when
   applicable). Ordinary online install already re-resolves without reading the
   old lock; no deletion or manual string replacement is necessary. This requires
   available metadata/artifacts and can select different versions, digests, roots,
   and dependency edges. It is re-resolution, not identity-preserving migration.
3. Compare the replacement lock against the preserved backup, review all identity,
   artifact and graph changes, and run `tapid lock verify` from the project plus
   frozen replay against the verified store before adopting it.

If the original registry cannot be reached through supported routing, or the old
exact graph must be retained, stop: this release cannot safely migrate that lock.
Do not alias origins, drop colliding entries, or bypass integrity checks. Invalid
credential-bearing origins remain rejected, not treated as recoverable aliases.
Offline/frozen rejection performs no network work and leaves the lock, manifest,
store, layout and activation state untouched for an unchanged incompatible lock.
The CLI checks before acquiring activation state and checks again under the lock;
this is not a transaction against arbitrary concurrent external file writers.

Package keys encode empty contexts as `peer=-|platform=-`. Peer contexts use canonical `name=...;version=...` fields. Platform contexts preserve independent OS, CPU, and libc fields using named fields, for example `platform=os=linux;cpu=;libc=` for OS-only, `platform=os=;cpu=x86_64;libc=` for CPU-only, or `platform=os=linux;cpu=x86_64;libc=gnu` when all are present. Reserved characters are percent-encoded. Duplicate, unordered, malformed, and noncanonical context representations are rejected.

Schemas 6 and 7 require sorted, unique, canonical roots for every nonempty package graph and explicit registry-integrity provenance for every registry package. During replay, each root must satisfy a direct manifest identity and all requirements contributed by the supported manifest maps, and every direct identity must have exactly one root. Schema 5 locks require online regeneration. Rootless schema 4 compatibility reconstructs the highest matching locked version per direct identity and rejects incomplete or context-ambiguous reconstruction.

Local fixtures are runtime-derived and do not imply live registry or cross-platform validation. Existing CI consumer jobs do not establish ADR 0005 containment; platform status requires the exact-commit evidence in [platform-validation.md](platform-validation.md).

## npm aliases

Declarations such as `"h3-v2": "npm:h3@2.0.1-rc.20"` install the verified `h3`
artifact under the local import name `h3-v2`. Scoped local and actual names are
supported, as are exact versions, supported semver ranges, and omitted ranges,
which select the highest stable version. Explicit dist-tags such as `latest`,
nested aliases, non-registry targets, aliases under a `jsr:` manifest key, and
aliases in JSR package metadata are rejected. This implements the alias part of [#153](https://github.com/LimeTip/tapid/issues/153).

Registry routing and authentication use the actual package name and scope.
Different local aliases can select different versions of one actual package.
Regular and optional transitive edges retain their declared local names.
Executable names come from the actual package's `bin` metadata, and existing
shim collision checks still apply. Dependency lifecycle scripts do not run.

Schema 7 adds `rootBindings`, mapping local root names to exact actual package
keys, and `dependencyAliases`, explicitly recording renamed transitive edges.
Package keys, artifact integrity, and verified store identities keep the actual
registry package name. Frozen and offline replay restore the recorded local
names and verify root targets against the manifest and configured registry.
Schema 6 locks without aliases remain readable and replayable. Older clients
reject schema 7. Schema 4 compatibility and schema 5 regeneration rules are unchanged.

`tapid add 'local@npm:actual@^1'` and `tapid i 'local@npm:actual@^1'` preserve the
alias declaration. `update` preserves its range, `update --latest` changes only
the target range to `*`, and `outdated` looks up the actual registry package.
Simple overrides of an aliased transitive edge's local name replace its range
while preserving the target package. Alias-valued overrides remain unsupported.

The synthetic alias fixture covers scoped routing, distinct versions, the h3
prerelease declaration, bins, malformed declarations, and frozen/offline replay.
This does not establish full npm compatibility or support for every arvtree dependency.
