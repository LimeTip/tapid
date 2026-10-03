# Production adoption gate

**Status: production support is not currently available.** Tapid is a package manager: it manages package selection, artifacts, `node_modules`, and lockfiles. It is not a runtime. Node executes Node.js applications; workerd executes Cloudflare Workers; Wrangler owns Workers development and deployment workflows. Tapid does not replace any of them. A green fixture, unit suite, one-machine install, or successful `tapid run` is not by itself production-support evidence.

## Support states

- **Experimental:** exploratory behavior; interfaces and guarantees can change; not for production dependency management.
- **Development:** a versioned build with documented behavior and regression checks, but one or more production gates below are absent. Use only in controlled non-production evaluation.
- **Production-supported:** an exact stable release and commit has a published, complete evidence record satisfying every applicable gate below. Support applies only to the listed feature/platform combinations, not to all npm projects or all uses of Tapid.

The current project status is **development; production support pending**. No version/platform combination is promoted by this document. A release claim must name the exact version, full source commit, binary digest, supported feature set and platform tuple, evidence links, known limitations, and approval in the release record. Missing, stale, partial, or failed evidence means pending, not supported.

## Release and support policy

Only a stable, immutable, tagged release can be considered for production support. Development builds, source refs, prereleases, and unverified alternate-repository builds remain development releases. Release packaging, publication, installer and upgrade verification, operator evidence, and lifecycle state are governed by the existing work: [#3](https://github.com/LimeTip/tapid/issues/3), [#115](https://github.com/LimeTip/tapid/issues/115), [#123](https://github.com/LimeTip/tapid/issues/123), and [#124](https://github.com/LimeTip/tapid/issues/124). This gate does not reimplement those mechanisms.

Before a supported release is adopted, the operator must have a named owner, a tested recovery path, and an upgrade window that allows a fix or rollback before the next production deployment. Review every stable release's compatibility notes before upgrade; do not assume automatic major-version compatibility. Apply a published security fix or mitigation within the response window set by the operator's risk policy, recording the chosen deadline and any exception. Security reports and coordinated disclosure use [SECURITY.md](../SECURITY.md). A release with an unresolved critical/high-impact defect affecting the declared support scope cannot pass the gate. Follow the release's immutable artifact and upgrade procedure; checksums from the same release source detect corruption but are not independent authenticity proof.

Rollback means redeploying the previously approved application artifact with its retained npm lockfile and npm-based install/deployment procedure. Preserve `package.json`, the original `package-lock.json`, the Tapid lockfile, and deployment configuration throughout canary. Never assume that replacing a Tapid binary alone restores a prior dependency graph. If rollback needs npm network access, cached npm artifacts, or a prior image, verify that prerequisite before beginning the canary.

## Exact support matrix

“Pending” means no production claim is permitted. An implementation, issue, CI job, or single local result is not sufficient. Support is feature-specific and must be re-attested for the exact release candidate.

| Dimension | Production-supported scope today | Evidence required to change pending status |
|---|---|---|
| Tapid release | None; production support pending | Stable immutable release; exact tag-to-commit binding, installed binary version and digest; release/operator evidence required by #3, #115, #123, #124; complete gates in this document |
| Package-manager core | None declared production-supported | Clean consumer installs and lock replay; parity report against the declared baseline; failure/rollback tests; independent repeatability on every claimed platform; no unresolved compatibility blocker |
| Node.js | Node.js 22 is the reference fixture line only; production support pending | Exact Node patch/version and ABI/tooling recorded, clean install/build/test/start on each claimed OS/architecture, repeated on clean workers and independent environments |
| Cloudflare Workers runtime | Pending; Tapid is not a runtime and does not replace workerd | Workers-specific install/build/test evidence naming Node tooling, workerd compatibility, and the exact Wrangler version/workflow. Wrangler remains the owner of Workers development and deployment. See [#166](https://github.com/LimeTip/tapid/issues/166) |
| Operating systems and architectures | No production-supported tuple declared | Exact OS release/image and CPU architecture, native binary digest, installation/upgrade/uninstall plus consumer acceptance on a clean hosted runner and an independent environment; repeat for every tuple. Release archive availability or cross-compilation alone is insufficient |
| npm lockfile migration | Pending | #151 implementation, supported lockfile/schema inventory, byte-stable import, parity and unsupported-entry diagnostics, no-mutation failure tests, and rollback drill preserving original lock. No automatic or lossless migration is implied before this evidence |
| npm registry | npm public registry subset only; production support pending | Exact metadata/specifier/optional/peer coverage report, live clean install and replay, integrity and failure behavior, repeated on each target tuple. Private authenticated registries remain pending under #164 |
| JSR registry | Experimental; live install unverified | Live registry acceptance and exact integrity/replay evidence on every claimed tuple; until then excluded from production scope |
| Workspaces and local links | Pending | #154 implementation, root/member and glob coverage, install/mutation/replay, path containment, and failure-atomicity tests on each claimed tuple |
| Dependency lifecycle scripts | Suppressed by default; opt-in production support pending | #155 exact digest/script approval, fail-closed isolation and limits, secret non-inheritance, crash/rollback and offline replay evidence on every claimed platform. No script execution support may be inferred from `tapid run` |
| `tapid run` | Optional Node-backed script convenience, not required for package management; production assurance pending | Runner evidence below for exact OS/architecture, backend, policy and runtime. It does not make Tapid a runtime, nor prove package install compatibility or deployment support |
| Deno/Bun and other runtimes | Pending / not claimed | Separate consumer matrix and runtime-specific evidence; Node-compatible layout alone is not support |
| Wrangler / deployment | Not owned by Tapid | Deployment remains operator/tooling responsibility; test the actual supported Wrangler pipeline separately. Tapid does not deploy Workers |

For the implementation scope and precise limitations, see [compatibility.md](compatibility.md) and [platform-validation.md](platform-validation.md). Planned work is not evidence: the relevant tracked implementation/acceptance items include [#150](https://github.com/LimeTip/tapid/issues/150), [#151](https://github.com/LimeTip/tapid/issues/151), [#152](https://github.com/LimeTip/tapid/issues/152), [#153](https://github.com/LimeTip/tapid/issues/153), [#154](https://github.com/LimeTip/tapid/issues/154), [#155](https://github.com/LimeTip/tapid/issues/155), [#156](https://github.com/LimeTip/tapid/issues/156), [#157](https://github.com/LimeTip/tapid/issues/157), and [#164](https://github.com/LimeTip/tapid/issues/164). These links track implementation; this document defines the adoption decision and evidence standard.

## What `tapid run` does and does not establish

`tapid run <SCRIPT> -- <ARGS...>` is an optional convenience: Tapid reads a project script and launches the selected Node executable with project-local tools/arguments. Node executes the JavaScript. It is not needed for Tapid package install, lockfile generation, or replay; users can use their ordinary runtime/tooling and deployment workflow.

The checked-in synthetic news-site fixture currently exercises Tapid script execution against an npm-installed reference tree on Ubuntu 24.04 / Node 22 in CI. That is script-execution evidence for that fixture, not Tapid-managed dependency-install parity, full npm compatibility, production support, or Workers deployment evidence. The README's local instructions are not independent production certification. `tapid run` isolation is separately gated by the native behavioral evidence in [platform-validation.md](platform-validation.md); Linux targeted evidence does not establish macOS or Windows support. Profiles and backends that are experimental, unsupported, or pending must remain described that way. Never convert a missing prerequisite or rejected restricted request into a claim of safe unsandboxed execution.

## Production evidence gate

Every item below is required for the exact release/commit and each claimed support tuple. Keep evidence in the release/operator record rather than duplicating release implementation.

1. **Identity and release integrity:** tag, source commit, workflow/check IDs, artifact name and SHA-256, version output, and installer/upgrade read-back agree. Record integrity limitations and provenance honestly.
2. **Feature contract:** list each claimed feature and excluded feature; link to compatibility rows and acceptance cases. Unsupported inputs fail clearly before state mutation.
3. **Representative compatibility:** pass the committed synthetic fixture and its npm baseline, including clean install, frozen and offline replay where claimed, build/test/start, readiness marker, deterministic package identity/version/source/integrity comparison, and documented diffs. The fixture is necessary evidence, never sufficient on its own.
4. **Platform breadth:** for every claimed OS/architecture/runtime tuple, run the full acceptance flow on a clean hosted runner and a second independent clean environment; record exact images, tool versions, binary digest, command logs and results. Repeatability across workers is required; one developer machine or one fixture execution does not qualify.
5. **Operational recovery:** complete the canary and rollback drill below; preserve logs and compare outputs. Verify the prior npm deployment can be restored without overwriting source or either lockfile.
6. **Security and support:** review threat/compatibility changes, outstanding advisories and release limitations; record owner, response window, escalation/contact route, and explicit go/no-go decision. Any unverified feature stays pending and outside the production claim.

Evidence must be reproducible from the named commit and retrievable by an operator. A CI badge, aggregate green status, unit tests, compilation, release archive matrix, source-level test, or synthetic fixture alone does not pass these gates.

## Canary migration and rollback

1. **Select and freeze:** choose a low-risk staging service, supported feature subset, exact Tapid candidate, and owner. Preserve source, `package.json`, `package-lock.json`, current deployment artifact/configuration, and the existing npm installation route. Record digests or immutable references; confirm npm rollback prerequisites before changing anything.
2. **Generate and inspect:** using the documented supported migration path, produce `tapid.lock` without deleting or rewriting the npm lock. If a lockfile import is unsupported or pending, do not invent a migration: use only a separately verified clean resolution workflow, record that it may select different versions, and compare every selected package identity, version, origin, integrity, optional/platform choice, and peer edge. Stop on unexplained drift or unsupported entries.
3. **Stage install:** install Tapid only in an isolated staging workspace/container from the exact candidate binary. Record command output and artifact digest. Verify frozen replay and, where claimed, offline replay. Do not run production deployment or change the production lockfile/install path.
4. **Compare behavior:** run the same build, tests, and startup/readiness checks against npm baseline and Tapid staging install. Compare dependency graph and application outputs; investigate every difference. For Workers, execute the actual workerd test and Wrangler build/deployment validation separately. A successful Node fixture or `tapid run` does not validate workerd or Wrangler.
5. **Canary:** deploy to staging or a tightly bounded non-production canary using the existing deployment owner/tool. Observe health, errors, latency, and rollback triggers for the agreed window. Record exact deployed artifacts and decision. Do not expand production scope if a gate is missing.
6. **Rollback drill:** restore the prior approved application artifact and npm install/deployment path using the preserved `package-lock.json`. Confirm the service starts and passes its readiness check; verify source and lockfiles remain byte-identical to their preserved copies. Record elapsed time, commands, outcome, and any manual dependency. If rollback fails, stop adoption and remediate before another canary.
7. **Decision:** approve only the exact release/commit, feature subset, and tested platform tuple supported by complete evidence. Keep all other combinations pending. A successful canary is necessary operational evidence, not a substitute for compatibility, platform, or security gates.

## Evidence record fields

For each adoption decision retain: Tapid version, full commit, tag, artifact SHA-256, feature matrix snapshot, Node/runtime and tool versions, OS/image/architecture, registry and lockfile formats, fixture and independent-environment run identifiers, parity report, test logs, limitations, canary service/environment identifier (non-sensitive), deployment/readiness observations, rollback result, named operator/approver, decision date, and next review/security-fix deadline. Exclude credentials, private source and customer data from public reports; store confidential evidence in the approved restricted system.
