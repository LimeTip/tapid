# Client release distribution

Status: implemented and verified through the real `v0.0.8` GitHub and crates.io release flow.

## Scope and sources of truth

This document is the operator runbook for releasing the Tapid command-line client. The implementation sources of truth are:

- `.github/workflows/release-publication.yml` for binary builds and draft GitHub releases;
- `.github/workflows/release-public-smoke.yml` for public installer verification;
- `.github/workflows/crates-publication.yml` for separate crates.io publication;
- `tools/release/release.ts` for tag and checksum validation;
- `tools/release/publish.ts` for the dependency-ordered crates.io plan;
- `docs/releases/<version>.md` for reviewed release notes.

ADR 0004 records why Tapid uses this GitHub-native design. This runbook records how to operate and recover it.
The [`v0.0.8` operations record](releases/0.0.8-operations.md) preserves the first complete release's concrete evidence and failure lessons.

## Trust model

Tapid currently relies on the reviewed repository, an annotated version tag, GitHub Actions, GitHub Releases, HTTPS, and repository release immutability. Release archives are accompanied by `SHA256SUMS`.

The checksum detects corruption, truncation, and accidental asset substitution. Because the checksum and archive come from the same GitHub release, it is not an independent authenticity proof. Tapid does not currently claim a separate signed release trust root.

The installers accept only stable `vX.Y.Z` versions and HTTPS release endpoints. They select a platform-specific archive, enforce conservative checksum, archive, and executable size limits, verify its SHA-256 checksum, require exactly one expected regular executable in the archive, extract into a temporary directory, and stage the destination before replacement.

`tapid upgrade` is intentionally unavailable. Reinstall through the public installer when upgrading. Authenticated self-update metadata can be reconsidered if Tapid later needs mirrors, independent update authorization, or provider migration.

## Versioning policy

The Git tag, GitHub release, `tapid` Cargo package, and `tapid --version` output share one product version.

Supporting crates are versioned independently. Publish a supporting crate only when its packaged contents or dependency requirements changed. Do not republish unrelated or unchanged workspace crates merely to align their numbers with the product release.

When an internal `0.0.x` crate changes, update its version and every affected dependent requirement deliberately. Cargo treats `0.0.x` requirements narrowly, so a changed dependency may require a dependent package version bump. The crates.io workflow publishes only the missing dependency closure reachable from `tapid`.

Reconsider a coordinated runtime-crate release group at `0.1.0` only if release evidence shows that nearly every runtime crate changes together. Do not retroactively normalize existing versions.

## Roles and approval boundaries

The flow has separate review and promotion gates:

1. A normal pull request reviews version, lockfile, release notes, and any changed supporting-crate versions.
2. Merging does not publish anything.
3. Creating the annotated tag is the immutable source-selection boundary.
4. The binary workflow creates a draft but never publishes it.
5. Publishing the reviewed draft is the public GitHub release boundary.
6. Public installer smoke tests must pass after publication.
7. crates.io publication is a separate manual dispatch and protected-environment approval.
8. A successful workflow is not sufficient by itself. Public endpoints, bytes, installed versions, registry versions, and final repository state require independent read-back.

Do not combine GitHub binary publication and crates.io publication into one approval.

## Standard release procedure

Use terminal-based `git` and `gh` operations. Keep credentials out of commands, logs, notes, and commits.

### 1. Prepare a reviewed release pull request

The release pull request must:

- update the `tapid` package to the intended product version;
- update only changed supporting-crate versions and affected dependency requirements;
- regenerate `Cargo.lock` through Cargo;
- add `docs/releases/<version>.md`;
- avoid hardcoding the version in reusable smoke or installer automation;
- pass the complete required check set and automated review on its final head;
- have every actionable review conversation answered and resolved.

Run the release contract tests before declaring the pull request ready:

```sh
node --experimental-strip-types --test tools/check_architecture_test.ts tools/release/release_test.ts tools/release/publish_test.ts
```

Before merge, record the exact pull-request head and base. After merge, read back the concrete merge commit and verify that it is the expected protected `main` tip. Wait for post-merge CI and security checks on that exact commit.

### 2. Create the immutable annotated tag

Set the intended values explicitly:

```sh
VERSION=X.Y.Z
TAG="v$VERSION"
COMMIT="REPLACE_WITH_VERIFIED_RELEASE_MERGE_COMMIT"
```

Verify the source before creating the tag:

```sh
set -euo pipefail
git fetch --no-tags origin "+refs/heads/main:refs/remotes/origin/main"
test "$(git rev-parse "origin/main^{commit}")" = "$COMMIT"
test "$(git show "$COMMIT:crates/tapid-cli/Cargo.toml" | sed -n 's/^version = "\([^"]*\)"/\1/p' | head -1)" = "$VERSION"

test -z "$(git tag --list "$TAG")" || {
  echo "local tag already exists: $TAG" >&2
  exit 1
}
remote_tag_refs="$(git ls-remote origin "refs/tags/$TAG" "refs/tags/$TAG^{}")"
test -z "$remote_tag_refs" || {
  echo "remote tag already exists: $TAG" >&2
  exit 1
}
```

Create and push the annotated tag exactly once:

```sh
git tag -a "$TAG" "$COMMIT" -m "Tapid $TAG"
git push origin "refs/tags/$TAG"
```

Immediately read back both the tag object and peeled commit:

```sh
git ls-remote origin "refs/tags/$TAG" "refs/tags/$TAG^{}"
```

Record both object identifiers. Never move, recreate, delete and recreate, or force-push a release tag.

### 3. Verify the binary workflow

The tag-triggered workflow must:

- refetch and validate the remote annotated tag object;
- require the peeled commit to belong to `main`;
- check out the exact peeled commit for metadata validation and every build;
- build the six native targets on Linux, macOS, and Windows runners;
- execute each native binary and require `tapid <version>`;
- produce six platform archives;
- aggregate artifacts by explicit name pattern;
- create one new draft release only when no release already matches the tag;
- retain the numeric release ID returned by creation;
- tolerate brief collection read-after-write delay with bounded retries;
- require exactly one matching release whose ID equals the create response;
- upload and read back the exact seven-asset set.

Warnings and notices are evidence, not harmless decoration. Inspect the workflow annotations even when every job is green. Upgrade deprecated action runtimes and validate announced runner-image migrations in an ordinary pull request before their deadlines.

### 4. Review the draft by numeric release ID

A draft is not public and some tag-based REST endpoints return 404 for drafts. List releases, including drafts, identify exactly one matching tag, and retain its numeric ID. Use that ID for subsequent API reads and asset downloads.

The expected asset set is exactly:

- `tapid-<version>-aarch64-apple-darwin.tar.gz`
- `tapid-<version>-x86_64-apple-darwin.tar.gz`
- `tapid-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `tapid-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `tapid-<version>-aarch64-pc-windows-msvc.tar.gz`
- `tapid-<version>-x86_64-pc-windows-msvc.tar.gz`
- `SHA256SUMS`

Before publication:

1. Require `draft=true`, `prerelease=false`, the exact tag, and the expected release ID.
2. Require exactly seven assets and no unexpected names.
3. Record every asset ID, name, and provider-reported size.
4. Download every asset from GitHub by numeric asset ID into a fresh directory.
5. Compare each downloaded size with the provider-reported size.
6. Verify all six archives against the downloaded `SHA256SUMS`.
7. Require each archive to contain exactly one regular file named `tapid` or `tapid.exe`, as appropriate.
8. Execute the locally compatible downloaded binary and require `tapid <version>`.
9. Correlate the other binaries with successful native build jobs that executed the version check.
10. Review `docs/releases/<version>.md` and require the draft body to match it exactly.

On macOS, checksum verification can use:

```sh
(cd <download-directory> && shasum -a 256 -c SHA256SUMS)
```

On Linux, use `sha256sum -c SHA256SUMS`.

An HTTP 200 response does not prove installation or asset correctness.

### 5. Edit draft metadata safely

The create-release API requires `tag_name`; the update-release API does not. During `v0.0.8`, however, an incomplete metadata update through the client and payload shape used at the time exposed the draft under an internal `untagged-*` slug. Treat that observed failure as a reason to make draft edits explicit and immediately verifiable, not as a claim that every PATCH requires a complete record.

Avoid editing metadata after final draft approval. If notes or other metadata must change before approval, identify the release by numeric ID, snapshot its current state, and send a minimal update containing only the fields that are intentionally changing. Avoid a client or payload shape that injects defaults for omitted fields.

Before and after the update, compare:

- `tag_name`;
- `target_commitish`;
- `name`;
- `body`;
- `draft=true`;
- `prerelease=false`;
- `make_latest=false`.

For an existing annotated tag, `target_commitish` does not determine source identity. Keep the draft's normal `main` value. The existing annotated tag and its peeled commit are the immutable source binding.

Changing `target_commitish` to an older commit that modifies `.github/workflows` relative to the default branch can require additional OAuth `workflow` scope. GitHub may deliberately return 404 when that scope is absent. Do not broaden token scope merely to make an unnecessary metadata edit work.

After every draft edit, read back the release by numeric ID and through the draft-aware `gh release view` surface. Require the original release ID, exact tag, expected seven assets, reviewed body, `draft=true`, and unchanged remote tag object and peeled commit. Require every field outside the intended change set to remain byte-for-byte or value-for-value unchanged. The edit invalidates any prior approval; repeat the final draft review before publication.

### 6. Publish the reviewed draft

Publication requires explicit human approval after the final draft read-back. Promote the already-reviewed numeric release record without resupplying its notes, title, tag, target, or prerelease state:

```sh
RELEASE_ID="REPLACE_WITH_VERIFIED_NUMERIC_RELEASE_ID"
case "$RELEASE_ID" in
  ''|*[!0-9]*) echo "invalid numeric release ID" >&2; exit 1 ;;
esac
gh api --method PATCH \
  "repos/LimeTip/tapid/releases/$RELEASE_ID" \
  -F draft=false \
  -f make_latest=true
```

This minimizes the irreversible promotion mutation and prevents a dirty, stale, or later-modified local notes file from replacing the approved body at publication time.

Immediately verify through an unauthenticated API request that:

- the public release ID is the reviewed draft ID;
- `tag_name` equals the intended tag;
- `draft=false` and `prerelease=false`;
- the exact seven assets remain present;
- the release is immutable when repository release immutability is enabled;
- the remote annotated tag object and peeled commit are unchanged.

### 7. Verify public installation

Publication triggers `.github/workflows/release-public-smoke.yml` at the tagged commit. Require exactly three successful jobs:

- `Unix installer (ubuntu-latest)`;
- `Unix installer (macos-latest)`;
- `Windows installer`.

Each platform must:

1. download the installer from `tapid.dev`;
2. install the explicit published version;
3. execute the installed binary and require `tapid <version>`;
4. execute `--help` where configured;
5. install again through latest-release discovery without an explicit version;
6. execute that binary and require the same version.

Read the job steps and logs. Do not infer real installation from workflow success alone.

Do not start crates.io publication until the public release and all three installer jobs are verified against the exact tag commit.

### 8. Verify and dispatch crates.io publication separately

Run the live dry-run before dispatch:

```sh
node --experimental-strip-types tools/release/publish.ts --dry-run
```

Review the exact missing package/version sequence. Every package in that plan must have exactly one crates.io Trusted Publisher configuration with:

- repository owner `LimeTip`;
- repository name `tapid`;
- workflow `crates-publication.yml`;
- environment `crates-io-release`;
- trusted-publishing-only enabled.

The GitHub environment must:

- allow the workflow run from `main`;
- require independent approval;
- prevent the dispatching actor from approving the same deployment.

Dispatch only after a separate explicit approval:

```sh
gh workflow run crates-publication.yml \
  --repo LimeTip/tapid \
  --ref main \
  -f tag="$TAG"
```

The workflow checks out the tag, validates that it is annotated and belongs to `main`, verifies the matching public GitHub release and exact-tag public smoke run, packages the workspace, acquires a short-lived OIDC token, and publishes only missing packages in dependency order.

No long-lived crates.io token is stored in GitHub. Do not add one as a fallback.

After environment approval, verify every package through the crates.io API before treating it as published. Then perform a clean registry installation of the exact `tapid` version and execute the installed binary.

### 9. Complete final public read-back

A release is complete only after verifying:

- protected `main` and its post-merge checks;
- annotated tag object and peeled commit;
- public immutable GitHub release and reviewed release ID;
- exact asset names, sizes, and SHA-256 values from public downloads;
- archive structure and locally compatible binary version;
- public `tapid.dev/install.sh` and `tapid.dev/install.ps1` endpoints;
- explicit-version installation on Ubuntu, macOS, and Windows;
- latest-release discovery installation on Ubuntu, macOS, and Windows;
- every planned crates.io version;
- clean `cargo install tapid --version <version> --locked` behavior;
- final repository, release, tag, workflow, and registry state.

## Recovery procedures

Recovery must preserve the original annotated tag and exact tagged commit.

### Tag-triggered workflow fails before draft creation

1. Diagnose the failed run and preserve its logs.
2. Fix the workflow through a normal reviewed pull request.
3. Merge and verify post-merge checks.
4. Manually dispatch the current input-free release workflow from `main`.
5. The workflow derives the expected tag from reviewed Cargo metadata, refetches the remote annotated tag, and builds the exact peeled commit.

Never recreate or push the tag again merely to retrigger the workflow. Do not rerun a historical workflow revision if it would build mutable `main` instead of the exact tag commit.

### Draft exists but workflow failed before or during asset upload

Do not dispatch the create-only workflow again and do not create a duplicate release.

1. Identify the existing draft by listing all releases and recording its numeric ID.
2. Confirm the draft tag and ID correspond to the failed create response.
3. Download only successful build artifacts from the exact failed run.
4. Require all six expected archives and reject extra files.
5. Generate and verify `SHA256SUMS` from those exact bytes.
6. Inspect archive layout and correlate native version checks with the build logs.
7. List assets already attached to the draft by numeric release ID. Require their names to be a unique subset of the expected seven names and reject unexpected or duplicate names.
8. Download every existing asset by numeric asset ID. Require its size and SHA-256 to match the corresponding locally recovered file exactly. Stop on any mismatch; never overwrite or delete an ambiguous asset.
9. Upload only expected names that are not already present, using the existing numeric release ID.
10. Download all seven resulting assets back by numeric asset ID.
11. Recheck names, sizes, checksums, archive layout, draft state, tag object, and peeled commit.

Manual recovery is an evidence-preserving exception, not the normal release path.

### Draft tag becomes `untagged-*`

Stop before publication. Do not delete the release or tag.

1. Locate the draft through the release collection and confirm its original numeric ID and seven assets.
2. Confirm the real annotated version tag still exists and peels to the original commit.
3. Restore the draft with a complete metadata update containing the intended `tag_name`, `target_commitish: main`, title, reviewed body, `draft=true`, `prerelease=false`, and `make_latest=false`.
4. Read back by numeric ID and with `gh release view <tag>`.
5. Require the same ID, exact tag, seven assets, reviewed notes, draft state, and unchanged tag object before continuing.

A 404 while changing the target to an older workflow-bearing commit can mean the token lacks OAuth `workflow` scope. Keep `target_commitish: main` for an existing tag instead of expanding credentials. The annotated tag remains the source binding.

### Public smoke fails after publication

A published immutable release cannot be repaired in place.

1. Stop crates.io publication.
2. Preserve logs and determine affected platforms.
3. Communicate the limitation if users can encounter it.
4. Fix through a new reviewed patch release with a new annotated tag.
5. Do not move the existing tag, replace assets, or pretend a rerun changed published bytes.

### crates.io publication partially succeeds

Published crate versions are immutable.

1. Record every confirmed package/version and the failing package.
2. Query crates.io independently rather than trusting only the failed workflow log.
3. Run `tools/release/publish.ts --dry-run` again against the registry.
4. Require the new plan to contain only the still-missing suffix of the dependency order.
5. Respect crates.io rate-limit instructions and avoid rapid blind retries.
6. Redispatch only after confirming the failure is safely resumable and obtaining the required environment approval.

## Evidence ledger

Record the following for every release in the release issue, pull request, or operator record:

- release-preparation PR URL, final head, and merge commit;
- post-merge check-run IDs and conclusions;
- annotated tag object ID and peeled commit;
- binary workflow run ID and any recovery run ID;
- draft/public release ID;
- asset IDs, names, sizes, and SHA-256 values;
- release-notes source and exact read-back result;
- publication time and immutable state;
- public smoke run ID, tagged commit, three job names, and conclusions;
- crates.io dry-run plan;
- Trusted Publisher verification result;
- crates.io workflow run ID and independent approval state;
- published package/version set;
- clean registry installation command and binary output;
- final public endpoint and repository-state verification.

Replace credentials, tokens, cookies, signing material, and secret values with `[REDACTED]`. Do not store them in the evidence ledger.

## Prohibited actions

Never:

- move, recreate, delete and recreate, or force-push a release tag;
- publish a draft before exact asset and note verification;
- use an HTTP 200 response as installation evidence;
- select a draft upload target only by tag when a numeric release ID is available;
- overwrite release assets or create a duplicate release during recovery;
- rebuild mutable `main` and label it as an existing tagged release;
- publish crates.io packages before public installer smoke succeeds;
- fall back to a long-lived crates.io token when Trusted Publishing fails;
- align unchanged crate versions merely for cosmetic consistency;
- call a release complete before independent public and registry read-back.

## Residual risks

- GitHub repository or workflow compromise can replace both an archive and its checksum before publication.
- The installers do not enforce rollback protection beyond selecting a requested immutable release version.
- Public smoke tests run after publication and can detect but cannot prevent a broken release from briefly being available.
- macOS and Windows platform code signing are not part of this flow.
