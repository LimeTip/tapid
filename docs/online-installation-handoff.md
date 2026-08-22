# Online installation handoff

The CLI's fixture installation path remains available through
`tapid install --registry-fixture ...`. The non-fixture path is deliberately
fail-closed until the registry-client contract can carry the metadata required
for a verified transitive install.

## Exact blocking API gaps

The public `tapid-registry-client` APIs currently provide:

- `NpmRegistry::fetch` → `Vec<RegistryArtifact>`
- `JsrRegistry::fetch` → `Vec<RegistryArtifact>`
- `RegistryArtifact` fields: package identity, artifact URL, and optional
  integrity

Neither fetch method exposes the dependency map belonging to each selected
package version. Consequently the CLI cannot construct the normalized
`PackageVersionMetadata` required by `tapid-resolver` or generate trustworthy
lockfile dependency edges for transitive packages. `JsrRegistry` currently
returns `integrity: None`, so it also cannot satisfy the required SHA-512
verification contract for JSR artifacts.

The existing `HttpTransport`/`HttpsTransport` boundary can retrieve bounded
HTTPS response bodies, but parsing registry JSON in the CLI would bypass the
registry-client's validation boundary and would not fix the missing JSR
integrity contract. The CLI therefore reports a stable error instead of
claiming to install a graph that it cannot verify.

## Required narrow follow-up

Extend `tapid-registry-client` (outside this change's ownership) with a
per-version metadata result that includes:

1. normalized dependency maps (including the registry/origin semantics needed
   for npm and JSR dependencies),
2. artifact URL,
3. required SHA-512 integrity for both registries, and
4. a safe artifact-body fetch operation or an explicitly documented way for
   the CLI to use the bounded transport while retaining URL/origin policy.

After that contract exists, `crates/tapid-cli/src/online.rs` can wire the
existing HTTPS transport into metadata retrieval, deterministic transitive
resolution, artifact verification, archive ingestion, lockfile v3 edges, and
node_modules materialization without ad-hoc registry parsing.