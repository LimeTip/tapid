# Automated releases

The ordinary release flow is GitHub-native. Start preparation, review and merge its PR, then approve one complete candidate. The workflow handles tagging, signing, GitHub publication, public installation checks, and crates.io publication. T3 Code and Codex can run the same commands, but neither is required.

[ADR 0007](adr/0007-one-candidate-one-release-approval.md) changes the approval policy. Deploying the workflow files alone does not prove that the required repository, App, environment, or registry configuration is active. Complete and verify the setup below before using this lane.

## Pre-publication installer investigation (#126)

**Decision (2026-10-26): no implementation for [#126](https://github.com/LimeTip/tapid/issues/126).** The bounded experiment reached the platform-machinery stop condition. This is a scope decision for non-blocking hardening, not successful native installation evidence or acceptance of the original feature. Revisit if a smaller exact-byte transport becomes available.

| Option | Coverage and cost | Decision |
| --- | --- | --- |
| Native builds install their own archive | Exercises archive installation, but final generated installers and signed metadata are assembled downstream. Moving those inputs upstream duplicates candidate coordination; synthetic metadata misses the final candidate. | Reject as the primary check. |
| Existing draft verification installs exact assets | Could exercise final installers, signature verification, installation policy and installed executables. Reuses numeric release/asset downloads and six native runners, but requires HTTPS routing and trust for unchanged URLs. | Preferred location if revisited; reject the present transport approach as too much platform machinery. |

### Findings and scope stop

Inspected source: `b040cef091d6267257b9a1fc5583fa5126924294`.
[Bootstrap generation](../tools/release/bootstrap.ts) uses candidate archive hashes,
version and release URL: normal generated bootstrap archives are already among the
eleven draft assets. A future implementation must validate this for each actual
handoff, including historical installer differences.

The installers download metadata/signatures from `tapid.dev`, then bootstrap and
target archives from embedded or signed GitHub URLs. `TAPID_RELEASE_RECORD_URL`
alone cannot redirect the journey. Rewriting installers or signed records would
weaken exact-draft evidence and was not used.

A disposable Node HTTPS probe bound only to `127.0.0.1` on an ephemeral port. It
served one exact host/path GET mapping and rejected an unmapped query with 404;
its TLS client rejected untrusted and wrong-host certificates. The actual curl
command shape from `scripts/install.sh` fetched exact synthetic bytes using
child-only `CURL_HOME` configuration (`connect-to`, `cacert`, `noproxy`). OpenSSL
provided a one-day self-signed certificate for `draft.invalid`. TLS verification
stayed enabled. The probe used no credentials, forwarding, system trust changes,
hosts edits, installer modifications or candidate execution. Requests had bounded
timeouts; connections, server and temporary files were cleaned up. It did not
exercise every routing failure or prove isolation against arbitrary candidate code.

The Windows downloader constructs a fresh `HttpClientHandler`/`HttpClient`; it
has no curl configuration hook. Microsoft's [handler proxy contract](https://learn.microsoft.com/en-us/dotnet/api/system.net.http.httpclienthandler.proxy)
describes handler/system proxy configuration. The draft workflow uses Windows
PowerShell, so modern .NET proxy-environment behavior is not evidence of support.
Direct routing would require hosts-file mapping of public hostnames, a port-443
listener, temporary OS certificate trust, proxy bypass, and restoration after
exceptions/timeouts. Installation also requires restoring Windows user PATH.
The proxy alternative requires HTTPS interception. Neither was implemented or
tested. Temporary runner changes were allowed; they are not claimed to be
impossible or forbidden. Adding privileged routing and OS-specific trust/recovery
is judged too much machinery for this small check. This is an early scope stop,
not a failed Windows experiment or sandbox limitation.

### Measurements and retained change

One local synthetic transport sample: macOS arm64, Node 22.23.3, curl 8.7.1
(SecureTransport). Child curl configuration plus HTTPS fetch: **22.4 ms**. Final
probe test including certificate generation and TLS negative checks: **105.1 ms**;
test-runner total: **177.7 ms**. The initial test failed with the missing module,
then passed after implementation. No build or hosted queue was involved.

**Exact-candidate native installation samples: zero on all six targets.** No
candidate identity, numeric release/asset IDs, hosted run URL, per-phase installer
timings, slowest native platform or added critical-path measurement exists. The
under-ten-added-minutes target remains unproven. These transport timings establish
no Ubuntu/Windows, candidate-installation or percentile claim. Existing extraction
and native binary/signature checks do not fill the canonical-installer gap.

The scratch probe was removed at the scope stop; no experimental module or
workflow integration remains. Retained changes are this decision and the
[distribution-runbook cross-reference](release-distribution.md#4-review-the-draft-by-numeric-release-id).
Added production dependencies, jobs, permissions, credentials, approvals and
operator steps: **zero**. Scratch dependencies were Node built-ins, curl and
OpenSSL. The implementation handoff records actual changed lines.

No new success receipt or release gate exists. Existing identity, checksum,
signature, native-execution failures and fail-closed promotion ordering remain
unchanged. The proposed provenance handoff and installer failure-path tests were
not added after the scope stop. No release was created or mutated.

Validation: release-tooling suite **287 passed, 6 skipped, 0 failed** (293 tests,
22.51 s); all skips require unavailable PowerShell. `sh -n scripts/install.sh`
passed. PowerShell parser/native checks remain with existing Windows CI. The
removed transport probe is separate synthetic evidence, not a candidate receipt.

Post-publication public installer and upgrade smoke remains unchanged. Even a
successful fixture would not prove public DNS, redirects, endpoint routing,
availability, discovery or upgrade behavior.

## One-time setup

1. Merge the reviewed automation changes and pass the required checks on protected `main`.
2. Install a repository-scoped GitHub App with Contents and Pull requests write access for release-PR preparation. Set the repository variable `RELEASE_APP_ID` and repository secret `RELEASE_APP_PRIVATE_KEY`. Keep the private key out of commands and logs. The App allows normal PR checks to start without a second workflow-approval click; do not substitute a personal access token.
3. Configure `stable-release` with exactly one custom deployment policy of type `branch`, named `main`. Select "Selected branches and tags", not "Protected branches", and add no tag policies or other branch policies. Disable "Allow administrators to bypass". Require at least one independent user or team reviewer and enable "Prevent self-review". Retain its existing `TAPID_RELEASE_ED25519_PRIVATE_KEY` secret and `TAPID_RELEASE_SIGNING_KEY_ID` variable. Verify the key with the protected read-only key-check workflow after provisioning or rotation.
4. Configure `crates-io-release` with the same single custom `main` branch policy and administrator bypass disabled. Retain the existing crates.io Trusted Publisher bindings for repository `LimeTip/tapid`, workflow `crates-publication.yml`, and environment `crates-io-release`. After verifying that the new candidate approval gate is active, remove this environment's additional reviewer requirement. Do not remove the `stable-release` reviewer or change registry bindings to a wrapper workflow.
5. Configure an active branch ruleset covering `main`, with no excluded refs. Require `Release intent freshness` from GitHub Actions, integration ID `15368`, and enable strict up-to-date status checks. The ruleset must also require pull requests with at least one approval, dismiss stale approvals when new commits are pushed, and require approval of the most recent reviewable push. Classic branch protection alone does not satisfy the policy checker. A release PR cannot merge until the check validates the current base; merging a stale release intent is not a supported recovery path. Verify repository release immutability, release-tag protection, the other required PR checks, and the owned-domain installer and release-record redirects. The redirects follow public release assets and require no per-release website deployment.

Repository administrators review the environment-policy change explicitly. During activation, the policy checker accepts an existing `crates-io-release` reviewer requirement and reports that an additional approval is still needed. This lets the first run reach the `stable-release` candidate gate before the administrator removes the crates.io reviewer. The ordinary flow needs only one release approval after that removal. Both environments' branch restrictions and administrator bypass settings remain mandatory throughout activation. A missing App, secret, environment restriction, or registry binding must fail with an actionable setup message, rather than silently weakening verification.

## Prepare and review

In GitHub Actions, run "Prepare release" from `main`. Leave `version` empty to reuse an unpublished product version already on `main`, or choose the next patch when `main` matches the latest public release. You can also supply an explicit stable version newer than the public baseline and at least as new as the version on `main`. The same entrypoint works from a terminal:

```sh
gh workflow run release-prepare.yml --repo LimeTip/tapid --ref main
```

For an explicit version, add `-f version=X.Y.Z`. The version analysis may block a default patch and report a required larger bump. Review that result rather than overriding it blindly.

Preparation preserves existing nonempty release notes for the selected version, generates notes when absent, and always regenerates the release intent and package plan.

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
- Publication prerequisites include publishable workspace development dependencies. Cargo can require their registry versions while packaging a consumer, even with `--no-verify`. Private test fixtures remain outside the publication plan.
- If a planner correction changes the order recorded in the original release intent, the approved-suffix check blocks recovery. Review and explicitly approve a recovery procedure for the corrected order before publishing more crates. Do not edit the immutable tagged intent or weaken the suffix check to make a retry pass.

The signing-key check, standalone draft verification, and public smoke workflows remain available for diagnostics. Their inputs do not define the ordinary automatic flow; binary candidate building is called by the coordinator. The detailed [distribution runbook](release-distribution.md) retains exact asset, registry, and endpoint checks for investigation.
