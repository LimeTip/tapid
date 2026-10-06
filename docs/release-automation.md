# Automated releases

The ordinary release flow is GitHub-native. Start preparation, review and merge its PR, then approve one complete candidate. The workflow handles tagging, signing, GitHub publication, public installation checks, and crates.io publication. T3 Code and Codex can run the same commands, but neither is required.

[ADR 0007](adr/0007-one-candidate-one-release-approval.md) changes the approval policy. Deploying the workflow files alone does not prove that the required repository, App, environment, or registry configuration is active. Complete and verify the setup below before using this lane.

## One-time setup

1. Merge the reviewed automation changes and pass the required checks on protected `main`.
2. Install a repository-scoped GitHub App with Contents and Pull requests write access for release-PR preparation. Set the repository variable `RELEASE_APP_ID` and repository secret `RELEASE_APP_PRIVATE_KEY`. Keep the private key out of commands and logs. The App allows normal PR checks to start without a second workflow-approval click; do not substitute a personal access token.
3. Configure `stable-release` with exactly one custom deployment policy of type `branch`, named `main`. Select "Selected branches and tags", not "Protected branches", and add no tag policies or other branch policies. Disable "Allow administrators to bypass". Require at least one independent user or team reviewer and enable "Prevent self-review". Retain its existing `TAPID_RELEASE_ED25519_PRIVATE_KEY` secret and `TAPID_RELEASE_SIGNING_KEY_ID` variable. Verify the key with the protected read-only key-check workflow after provisioning or rotation.
4. Configure `crates-io-release` with the same single custom `main` branch policy and administrator bypass disabled. Retain the existing crates.io Trusted Publisher bindings for repository `LimeTip/tapid`, workflow `crates-publication.yml`, and environment `crates-io-release`. After verifying that the new candidate approval gate is active, remove this environment's additional reviewer requirement. Do not remove the `stable-release` reviewer or change registry bindings to a wrapper workflow.
5. Configure an active branch ruleset covering `main`, with no excluded refs. Require `Release intent freshness` from GitHub Actions, integration ID `15368`, and enable strict up-to-date status checks. The ruleset must also require pull requests with at least one approval, dismiss stale approvals when new commits are pushed, and require approval of the most recent reviewable push. Classic branch protection alone does not satisfy the policy checker. A release PR cannot merge until the check validates the current base; merging a stale release intent is not a supported recovery path. Verify repository release immutability, release-tag protection, the other required PR checks, and the owned-domain installer and release-record redirects. The redirects follow public release assets and require no per-release website deployment.

Repository administrators review the environment-policy change explicitly. During activation, the policy checker accepts an existing `crates-io-release` reviewer requirement and reports that an additional approval is still needed. This lets the first run reach the `stable-release` candidate gate before the administrator removes the crates.io reviewer. The ordinary flow needs only one release approval after that removal. Both environments' branch restrictions and administrator bypass settings remain mandatory throughout activation. A missing App, secret, environment restriction, or registry binding must fail with an actionable setup message, rather than silently weakening verification.

## Prepare and review

In GitHub Actions, run "Prepare release" from `main`. Leave `version` empty for the next patch, or supply a newer stable product version. The same entrypoint works from a terminal:

```sh
gh workflow run release-prepare.yml --repo LimeTip/tapid --ref main
```

For an explicit version, add `-f version=X.Y.Z`. The version analysis may block a default patch and report a required larger bump. Review that result rather than overriding it blindly.

The generated PR contains the product version, only changed supporting-crate versions and affected dependency requirements, regenerated lockfiles, release notes, and `docs/releases/intent.json`, with schema `tapid-release-intent-v1`, consumed by the coordinator. Its required `prepared_from` SHA records the `main` commit analyzed during preparation. The release merge commit must have that SHA as its first parent. If another PR advances `main`, close the old release PR and prepare again so the version analysis includes those changes. Review the version table and notes, then merge after the required checks pass. Automated preparation is a proposal; it does not decide whether a change deserves a breaking release or whether generated notes describe it correctly.

Rerunning preparation preserves the open `release/prepare` PR and reports its link, including any maintainer edits. It does not regenerate that PR or publish anything. Edit it directly before merge. To deliberately replace it, close the existing PR before preparing again.

## Approve and follow the release

The merged release intent starts the "Release Tapid" workflow on `main`. Its actual file remains `crates-publication.yml` for crates.io identity compatibility. Ordinary changes without a release intent do not publish a release.

The coordinator first checks the GitHub environment and branch policies and waits for CI, CodeQL, and command-help checks on the exact source commit. Missing setup blocks the run before tagging. Before the `stable-release` approval, read the candidate summary. It identifies the selected source commit and version, reviewed notes, the ten unsigned files and their digests, including all six archives and both generated installers, package compatibility results, and the exact dependency-ordered crate plan. Approval authorizes publication of this whole candidate, including crates.io. The reviewer must be independent of the actor who started the run.

After approval, the workflow signs and creates the draft, downloads and verifies its exact eleven assets, executes all six target binaries on native runners, promotes the verified draft, checks public installation and upgrades on Linux, macOS, and Windows, then publishes missing crates and checks a clean Cargo installation. It carries the tag, source SHA, release ID, and asset identity between jobs. Operators do not copy them into separate dispatch forms.

The signed draft adds the detached signature to those ten unsigned files, producing eleven assets. The final report links the release and verification evidence and lists confirmed package versions. Treat a queued approval, skipped job, failed public smoke, or partial registry publication as incomplete. A green build alone is not a completed release.

## Recover a failed run

Start with the failed run's summary and preserve its evidence. Retry only after understanding the failing prerequisite. Use the coordinator's manual dispatch from `main` with its optional `release_tag` when a particular existing release must be recovered. The workflow resolves its commit and release ID itself. Each new recovery run enters the independent `stable-release` approval again, including a crates-only remainder; there is no unapproved registry-publication shortcut. A retry must select the same reviewed candidate and pass its identity checks; a workflow fix on newer `main` must still build and publish the original immutable source when recovering that release.

- Before approval, rerun preparation or fix the release PR if the candidate itself needs changes. A changed merged candidate requires a new reviewed release intent.
- After approval but before publication, existing draft assets must match the candidate bytes exactly. A complete existing draft can be downloaded and verified by a new recovery run. An incomplete upload requires rerunning the failed upload job with its original retained candidate artifact; the workflow refuses to rebuild replacement archives. It checks existing bytes and uploads only missing expected assets. Never overwrite an asset, create a duplicate draft, or move a tag to make a retry pass.
- After GitHub publication, first rerun failed public checks if the failure was transient. If the published bytes are broken, block crates.io and prepare a new patch release. Published immutable assets cannot be repaired in place.
- After partial crates publication, independently query the registry and recompute the plan from the original tagged source. Continue only with the remaining approved package versions, in dependency order. Never blindly repeat a possibly successful publish or use a long-lived token as a fallback.

The signing-key check, standalone draft verification, and public smoke workflows remain available for diagnostics. Their inputs do not define the ordinary automatic flow; binary candidate building is called by the coordinator. The detailed [distribution runbook](release-distribution.md) retains exact asset, registry, and endpoint checks for investigation.
