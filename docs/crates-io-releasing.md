# Tapid crates.io release runbook

This runbook covers only crates.io packages. It does not create a Git tag, GitHub release, native archive, signed release manifest, stable channel, public installer smoke, or website deployment. See [the end-to-end release runbook](releasing.md) for those separate phases.

## Authentication and approval boundary

Tapid uses crates.io Trusted Publishing through the official OIDC flow. The protected GitHub environment is `crates-io-release`, separate from `stable-release`.

For every existing crate that may be published, configure a crates.io trusted-publisher record for:

- repository: `LimeTip/tapid`;
- workflow: `crates-publication.yml`;
- environment: `crates-io-release`.

Trusted Publishing cannot bootstrap a new crate name that the configured crates.io owner does not already own. Before planning publication, confirm every crate in the closure exists and is owned by the expected account or the restricted LimeTip maintainer team. Bootstrap of a genuinely new crate requires a separately approved one-time process before it can use Trusted Publishing.

Only the protected publish job in `.github/workflows/crates-publication.yml` may have `id-token: write`. Keep repository contents read-only and grant no other write permission. After human approval, use the official `rust-lang/crates-io-auth-action@v1` to obtain a short-lived registry credential. Expose it only to the individual `cargo publish` process. Do not store or print it.

No long-lived crates.io token is used by default. A token is an emergency fallback only when the official OIDC path is unavailable for a verified reason. It requires explicit user approval, least privilege, storage as a protected `crates-io-release` environment secret, a defined expiry, and immediate revocation or rotation after use. Never paste a token into chat, a command line, shell history, source, logs, plans, or artifacts.

## Version and dependency rules

Published crate archives are immutable. A source change to a publishable crate needs a new package version. For early `0.0.N` releases, `^0.0.N` does not accept `0.0.(N+1)`.

Before planning:

1. Identify every changed or unpublished publishable workspace crate.
2. Bump each changed crate.
3. Update each internal dependency to include both its local path and the exact intended registry requirement.
4. Bump and include every direct dependent whose requirement does not accept the dependency's new version.
5. Repeat until the complete publication and install closure is version-consistent.
6. Regenerate `Cargo.lock` and every affected nested lockfile, including `tests/integration/Cargo.lock`, through Cargo.
7. Do not publish `publish = false` packages or hosted-service placeholders.

The deterministic plan computes the topological publication order. Do not replace it with a manually maintained list. Dependencies must become registry-visible before dependents are published.

## Package-closure preflight

Issue #28 and registry-resolved standalone packaging are explicit gates. From a clean checkout at the exact planned commit, run:

```bash
cargo package --workspace --locked
cargo package -p tapid --locked
cargo test --manifest-path tests/integration/Cargo.toml --locked
git diff --check
git status --short --branch
```

Inspect the packaged manifests and confirm internal path dependencies were rewritten with explicit registry version requirements. Hosted run `33533691942` is useful historical evidence but does not replace this exact-commit gate.

Before the planned dependency versions exist on crates.io, standalone packaging of a dependent may fail only because a required version in the reviewed closure is not yet registry-visible. Record that expected pre-publication result. Build and hash proposed archives through the planner's isolated pre-publication path, publish sequentially, and rerun normal `cargo package -p tapid --locked` after the dependency closure is visible. Do not use `--allow-dirty`, hide any unrelated verification failure, or claim the final registry-resolved gate passed before it actually does.

## Create the read-only plan

The planner needs no publishing credential and performs no registry mutation:

```bash
COMMIT=$(git rev-parse origin/main)
python3 scripts/crates_release.py plan \
  --commit "$COMMIT" \
  --output crates-plan.json
```

Review the complete `tapid-crates-publication-plan-v1` JSON and its canonical plan digest. Verify:

- the source commit is the exact reviewed `origin/main` commit;
- the root `Cargo.lock` digest is correct;
- all publishable workspace crates and internal edges are represented;
- local and observed crates.io version states are current;
- changed, unpublished, and unchanged classifications are correct;
- every required `0.x` dependent version or requirement update is satisfied;
- publication order is deterministic and dependency-first;
- every proposed crate passed isolated `cargo package -p <name> --locked`;
- each generated `.crate` archive has the recorded digest;
- final `tapid` package verification resolves registry dependencies;
- nested integration lockfile preflight passes;
- only intended entries are marked `publish`.

Record the digest in the approved release change record. Never edit the plan. Source, lockfile, package archive, crates.io, ownership, or dependency-state drift requires a newly generated and reviewed plan.

A plan with no `publish` entries is a successful no-op. Record it and do not approve a mutation job.

## Dispatch protected publication

Dispatch `.github/workflows/crates-publication.yml` with the exact source commit and reviewed digest. The workflow interface is intentionally small:

```bash
gh workflow run crates-publication.yml \
  --ref main \
  -f commit="$COMMIT" \
  -f plan_digest=<crates-plan-digest>
```

Use the workflow's no-op dry-run path for PR validation. Before approving the publish job:

1. Compare the uploaded preflight plan and digest with the reviewed plan.
2. Confirm the workflow checked out the exact commit.
3. Confirm registry state and package archive digests were recomputed.
4. Confirm only the publish job requests `id-token: write` and `environment: crates-io-release`.
5. Confirm repository contents remain read-only and no long-lived token is configured.
6. Confirm the first package is a dependency root and the whole order matches the plan.

Do not approve if any precondition differs.

## Publication and read-back evidence

The protected workflow publishes sequentially. For each planned package it must:

1. Query crates.io before mutation.
2. If the exact version already exists, verify package identity and checksum, mark it verified, and never republish it.
3. Publish exactly one package using the short-lived OIDC credential.
4. Poll with bounded backoff until that exact version and checksum are visible from crates.io.
5. Atomically update and upload progress evidence.
6. Continue only after visibility is confirmed.

After all packages are visible, require clean Cargo-home package or install verification for `tapid` against registry dependencies. Record exact package names, versions, checksums, workflow run, plan digest, and final verification outcome.

A successful `cargo publish` process alone is not completion. Registry read-back and checksum equality are required.

## Partial failure and safe resume

Crates.io publication is not transactional. On any timeout, HTTP 429, rejection, indexing delay, or workflow interruption:

1. Stop at the first unverified package. Do not start a dependent.
2. Preserve the atomic progress report and workflow logs.
3. Query crates.io read-only for every package reported as published or in flight.
4. Mark a version complete only when the exact package identity and checksum match the reviewed plan.
5. Treat HTTP 429 as not published unless read-back proves otherwise. Respect the server-provided retry time.
6. Resolve authentication, ownership, metadata, or rate-limit causes without changing published versions.
7. Recompute the plan immediately before resuming. If the source, lockfile, package archive, or registry state has changed beyond verified prior success, stop for a new reviewed digest.
8. Rerun the protected workflow with the same exact commit and accepted digest when its recomputed plan can classify verified prior successes safely.
9. Resume from the first unverified package. Never replay the whole batch.

Already-published versions cannot be deleted or overwritten to restore atomicity. If a source correction is required after partial publication, preserve the valid published versions, prepare new versions for affected crates and dependents, regenerate lockfiles, and create a new publication plan. Record the abandoned partial plan and its immutable results.

## Credential incident response

If an OIDC credential, emergency token, workflow token, or authorization header may have leaked:

1. Cancel publication and block `crates-io-release` approvals.
2. Do not copy the suspected secret into incident evidence.
3. Revoke the emergency token or remove the affected trusted-publisher authorization.
4. Review crates.io publication and owner history and GitHub workflow and environment audit logs.
5. Restore one trusted-publisher record per owned crate with the intended repository, workflow, and environment.
6. Remove any long-lived fallback secret after rotation or recovery.
7. Regenerate the plan if registry or repository state changed.
8. Resume only after containment, independent approval, and exact registry read-back.

Keep the signing-key incident path separate. crates.io credentials do not authorize `stable-release`, and the stable signing key does not authorize crates.io.
