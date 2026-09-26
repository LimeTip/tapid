---
name: Release evidence
about: Record the independently verified evidence for a Tapid release
title: "Release evidence: v"
labels: ""
assignees: ""
---

# Release evidence: vX.Y.Z

Use this checklist with [the release runbook](../../docs/release-distribution.md). Record immutable IDs and exact command output; do not paste sensitive authentication material.

## Scope and source

- Product version: `vX.Y.Z`
- Preparation PR URL: <!-- URL -->
- Preparation PR final head: <!-- 40-character SHA -->
- Merge SHA: <!-- 40-character SHA -->
- Post-merge checks on merge SHA: <!-- check names, run URLs, conclusions -->
- Product-vs-supporting-crate version distinction confirmed: <!-- product version is independent from each supporting-crate version -->
- Changed supporting crates (one row per crate; package, version, and reason are independent fields):

| Package | Published version | Reason it changed |
| --- | --- | --- |
| <!-- package --> | <!-- version --> | <!-- reason --> |

## GitHub source and binary release

- Annotated tag: <!-- tag name -->
- Annotated tag object: <!-- tag-object SHA -->
- Peeled commit: <!-- commit SHA -->
- Binary workflow run ID: <!-- numeric run ID and URL -->
- Recovery workflow run ID (or `not applicable`): <!-- numeric run ID and URL -->
- Draft/public release ID: <!-- numeric ID -->
- Exact assets (name, numeric asset ID, provider size, and SHA-256 verification for each):

| Asset name | Asset ID | Size (bytes) | SHA-256 verification |
| --- | ---: | ---: | --- |
| <!-- exact asset name --> | <!-- numeric ID --> | <!-- numeric size --> | <!-- verified digest/result --> |

- Release notes read-back: <!-- exact reviewed file/body comparison and URL -->
- Independent GitHub publication approval: <!-- approver, timestamp, and approved release ID -->
- Public release read-back: <!-- public ID, tag, draft/prerelease state, and asset set -->

## Public smoke evidence

- Public-smoke workflow run ID: <!-- numeric run ID and URL -->
- Tagged commit used by public smoke: <!-- peeled commit SHA -->
- Job 1 conclusion: <!-- exact job name and conclusion -->
- Job 2 conclusion: <!-- exact job name and conclusion -->
- Job 3 conclusion: <!-- exact job name and conclusion -->

## crates.io publication

- crates.io dry-run plan: <!-- command/output and dependency-ordered package/version set -->
- Trusted Publisher verification: <!-- workflow, repository, environment, and successful verification -->
- Independent crates.io approval: <!-- approver, timestamp, and protected-environment approval state -->
- crates.io workflow run: <!-- numeric run ID, URL, and tagged commit -->
- Published package/version set: <!-- exact package@version entries and registry read-back -->
- Clean locked cargo-install command/output: <!-- isolated Cargo home/target, command, version output, and help result -->

## Final state

- Final endpoint/repository-state read-back: <!-- public release, tag, registry, workflow, and repository state -->
- Limitations: <!-- known trust, signing, timing, or coverage limitations -->
- Follow-ups: <!-- issue URLs and owners, or `none` -->

## Verification sign-off

- [ ] All fields above are complete, independently read back, and tied to the stated product version.
- [ ] GitHub publication and crates.io publication were approved independently.
