# Tapid release runbook

This runbook is the operator procedure for a Tapid release. It deliberately separates source preparation, crates.io publication, annotated tag creation, GitHub binary publication, public smoke verification, and website deployment. These are distinct gates and distinct failure domains.

GitHub release archives and crates.io packages are separate distribution channels. Publishing one does not publish, verify, repair, or roll back the other. Record evidence for each channel independently.

## Roles and protected environments

Use two maintainers for a stable release:

- The release operator prepares the source, generates and reviews plans, creates the annotated tag, and dispatches workflows.
- The release approver independently checks the exact commit, version, tag, plan digests, workflow inputs, and preflight evidence before approving a protected job.

Protected environments have separate authority:

- `crates-io-release` protects crates.io Trusted Publishing. Only its publish job receives `id-token: write` and only after approval.
- `stable-release` protects release-manifest signing and stable GitHub publication. The signing key is available only inside its protected jobs.

Do not combine the environments, credentials, or approvals. Keep branch protection and environment reviewer rules enabled. The release operator must not approve their own protected job when GitHub settings allow that restriction.

## Phase 1: Prepare the source

1. Start from a clean checkout of the canonical repository. Fetch `main` and tags, then record the exact full commit:

   ```bash
   git fetch origin main --tags
   git status --short --branch
   git rev-parse origin/main
   ```

2. Choose a `0.x.y` version and matching `v0.x.y` tag. Update every changed publishable crate's package version. For a `0.x` dependency, update direct dependents whenever their requirement does not accept the new version. In particular, `^0.0.N` does not accept `0.0.(N+1)`. Bump a dependent package if that dependent must be republished.
3. Update the root lockfile through Cargo. Also regenerate every affected nested lockfile through Cargo, including `tests/integration/Cargo.lock`. Never hand-edit a lockfile.
4. Add `docs/releases/<version>.md` and make package metadata and release documentation agree on the version.
5. Run the source and pre-publication package gates from a clean tree:

   ```bash
   python3 scripts/check_architecture.py
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo test --workspace --all-features --locked
   cargo test --manifest-path tests/integration/Cargo.toml --locked
   cargo package --workspace --locked
   cargo package -p tapid --locked
   git diff --check
   git status --short --branch
   ```

6. Treat issue #28 and registry-resolved packaging as explicit evidence gates. The planner requires Cargo 1.89 or newer, where workspace packaging can overlay selected workspace packages, then creates and hashes every candidate archive with `cargo package --workspace --locked --no-verify` before any mutation and records `archives-hashed-without-registry-verification` in the digest-bound plan. Normal `cargo package --workspace --locked` and `cargo package -p tapid --locked` may fail before publication only because crates.io does not yet contain an internal dependency version included in the reviewed closure. Record that expected pre-publication blocker rather than claiming registry verification passed. Any API, manifest, lockfile, requirement, or unrelated package failure remains blocking. After the dependency closure is published, rerun both normal package commands and require them to pass before release completion. Hosted run `33533691942` is prior evidence, not a substitute for fresh evidence at the release commit.
7. Merge all release preparation to `main`. Re-run the gates against the exact resulting commit. Do not release from an unmerged branch or dirty worktree.

## Phase 2: Generate and review dry-run plans

The default planning commands are read-only. They do not create a tag, dispatch a workflow, publish a crate, or read a publishing credential.

Generate the binary release plan:

```bash
VERSION="${VERSION:?export VERSION as 0.x.y}"
TAG="v$VERSION"
COMMIT=$(git rev-parse origin/main)
python3 scripts/release.py plan \
  --version "$VERSION" \
  --commit "$COMMIT" \
  --output release-plan.json
```

Generate the crates.io publication plan when crates are in scope:

```bash
python3 scripts/crates_release.py plan \
  --commit "$COMMIT" \
  --output crates-plan.json
```

Review both canonical JSON files, not only terminal summaries. Record each plan digest in the release issue or other approved change record. Verify:

- repository, full source commit, version, and tag;
- `origin/main` equals the planned commit;
- release note, Cargo versions, requirements, and lockfile checks pass;
- tag state is `create` or `reuse-exact-annotated`;
- GitHub release state is absent or the expected draft, never already public;
- all seven derived binary workflow inputs are correct;
- the crates plan contains only intended packages in deterministic dependency order;
- package archive digests and the root `Cargo.lock` digest match reviewed evidence;
- no plan contains credentials.

Any source, Cargo metadata, lockfile, registry state, tag state, release state, timestamp, or workflow-input change invalidates the relevant plan. Regenerate and review a new digest. Never edit a generated plan by hand.

## Phase 3: Publish crates.io packages when required

Complete [the crates.io runbook](crates-io-releasing.md) before creating the release tag if this release changes public crates. The crates workflow is `.github/workflows/crates-publication.yml`; its protected mutation job uses `crates-io-release` and the reviewed crates plan digest.

Do not continue until every planned crate version is visible from crates.io with the expected package identity and checksum, and clean registry-based package or install verification for `tapid` passes. If no crate is marked `publish`, record the no-op plan and continue without a crates publication approval.

## Phase 4: Create or verify the annotated tag

Tag creation is an explicit operation bound to the reviewed release plan:

```bash
python3 scripts/release.py tag \
  --plan release-plan.json \
  --expect-digest <release-plan-digest>
```

The command must either create an annotated tag at the planned commit or safely accept an existing annotated tag that peels to exactly that commit. It must refuse lightweight, moved, malformed, or already-public release tags.

Verify locally and from the remote before dispatch:

```bash
git cat-file -t "$TAG"
git rev-parse "$TAG^{commit}"
git ls-remote --tags origin "$TAG" "$TAG^{}"
```

The object type must be `tag`, and the peeled commit must equal the reviewed full commit. Push the new tag only through the approved operator procedure if `scripts/release.py tag` does not do so itself. Never move or replace a release tag.

## Phase 5: Dispatch GitHub binary publication

Dispatch `.github/workflows/release-publication.yml` through the guarded command:

```bash
python3 scripts/release.py dispatch \
  --plan release-plan.json \
  --expect-digest <release-plan-digest>
```

The command recomputes repository and GitHub preconditions, checks the digest, and passes all seven derived workflow inputs explicitly. It must refuse a public release and source drift. A rerun against the same exact draft is allowed.

Before approving `stable-release`, compare the workflow inputs with the reviewed plan. Confirm that no secret was available to preflight or build jobs. The workflow must build these six native archives:

- `tapid-<version>-aarch64-apple-darwin.tar.gz`
- `tapid-<version>-aarch64-pc-windows-msvc.tar.gz`
- `tapid-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `tapid-<version>-x86_64-apple-darwin.tar.gz`
- `tapid-<version>-x86_64-pc-windows-msvc.tar.gz`
- `tapid-<version>-x86_64-unknown-linux-gnu.tar.gz`

The complete GitHub release has exactly eight assets: those six archives, `release-manifest.json`, and `stable.json`.

### Draft inspection and safe rerun

Before accepting public promotion, inspect the draft and workflow evidence:

```bash
gh release view "$TAG" --json isDraft,isPrerelease,targetCommitish,assets,url
```

Require a draft, not a prerelease. Confirm the exact eight asset names, no extras, nonzero sizes, manifest signature verification, tag and commit binding, freshness, and archive member checks. The workflow performs the final exact asset-set and freshness checks immediately before promotion.

If a job fails while the release is still a draft, keep it draft. Diagnose the cause, remove any unexpected asset, and rerun the guarded dispatch with the same plan only when all source and release state still match. Expected assets may be replaced only on that exact draft by the workflow's checked `--clobber` path. Never use this path on a public release.

## Phase 6: Public smoke verification

Public promotion is not completion. `.github/workflows/release-public-smoke.yml` must run after promotion in `stable` mode and remains manually rerunnable in `stable` or explicit `tag` mode.

The smoke workflow must use unauthenticated public reads and read-only repository permission. Require its `tapid-public-release-verification-v1` evidence to prove:

- GitHub reports a public, non-prerelease release at the expected tag and commit;
- the exact eight public assets are present;
- `stable.json` resolves the expected manifest endpoints;
- the signed manifest is valid, current, and bound to the expected version, tag, and commit;
- all six archives match declared size and SHA-256 and contain only the expected executable;
- clean Unix and Windows installers install the public binary;
- `tapid --version` matches the dynamically resolved release;
- a real public npm package installs, executes, and repeats successfully with frozen replay;
- redacted evidence includes final URLs, byte counts, and SHA-256 values without credentials or temporary paths.

A smoke failure after publication does not authorize deleting assets, moving the tag, or rolling back metadata. Open a blocking incident, stop website promotion claims, and either repair mutable website delivery or prepare a new version for immutable release defects.

## Phase 7: Verify website deployment

Website delivery is a separate deployment gate. The `website-installer-sync.yml` workflow synchronizes canonical `scripts/install.sh` and `scripts/install.ps1` to `LimeTip/tapid-web` after changes reach `main`. The public smoke workflow verifies deployment after its bounded propagation window.

Require byte-for-byte equality between:

- `scripts/install.sh` and `https://tapid.dev/install.sh`;
- `scripts/install.ps1` and `https://tapid.dev/install.ps1`.

Also require rendered homepage, getting-started, and release-page content to identify the intended release. A working GitHub release with stale website content is not a completed website deployment. Record website evidence separately from cryptographic release evidence.

## Recovery by publication state

### Before draft creation

No public release asset exists. Fix source or metadata on a new commit, regenerate both plans and digests, re-run gates, and create the tag only after review. If a tag was created but no draft exists, do not move it. Either safely reuse it when it is annotated and points to the exact unchanged commit, or abandon that version and prepare a new version.

### After draft upload, before public promotion

Keep the release draft. Preserve workflow logs and plan files. Verify the exact tag and commit, remove unexpected draft assets, regenerate expired freshness values through a new release plan when necessary, and rerun guarded dispatch. The rerun may replace expected draft assets after validation. Do not manually publish an incomplete draft.

### After public promotion

Treat the tag, six archives, and signed manifest as immutable. Do not move the tag, overwrite assets, edit signed metadata, or attempt to make crates.io match retroactively. Preserve evidence and open an incident. Website pages and installer copies may be corrected through their normal reviewed deployment if release bytes are sound. Any binary, manifest, or crate defect requires a new version and new plans. Public smoke can be rerun, but it must not mutate or roll back the release.

### After partial crates.io publication

Stop at the first unverified crate. Follow the crates.io runbook. Never replay the complete batch and never assume an HTTP success or failure proves registry visibility. Resume only after verifying already-published versions and checksums and after the reviewed plan still matches the exact source and registry state.

## Credential exposure and rotation

Never paste the release signing key, an OIDC credential, an emergency crates.io token, GitHub token, cookie, netrc content, or environment dump into logs, chat, issues, plans, artifacts, or commits. Do not pass a registry token on a command line. Redact authorization headers and secret-bearing command output from evidence.

If exposure is suspected:

1. Stop the affected workflow and all release approvals.
2. Preserve non-secret audit evidence without copying the secret.
3. Revoke or rotate the affected credential immediately through its provider.
4. For a signing-key event, quarantine stable publication and follow [release key management](security/release-key-management.md). Update the trusted public key through the approved trust-root process before signing again.
5. For crates.io, revoke an emergency token or remove the compromised trusted-publisher authorization, review crate owner and publication history, then restore the official OIDC configuration.
6. For GitHub, rotate the affected secret or token and review workflow, environment, and release audit logs.
7. Regenerate plans if any repository, registry, tag, or release state changed during response.

Do not resume until the incident owner and release approver confirm containment.
