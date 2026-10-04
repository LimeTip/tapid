---
name: Release operation evidence
about: Record evidence for one completed release operation
title: "release evidence: v"
labels: ""
assignees: ""
---

<!--
Evidence only: do not publish artifacts, approve environments, or trigger workflows from this issue.
Do not include secrets, credentials, access tokens, cookies, private keys, signing material, or their values.
Replace sensitive values with [REDACTED]. Reference docs/release-distribution.md instead of copying its procedure.
Worked example: docs/releases/0.0.8-operations.md.
-->

## Release identity

- Product/version: `vX.Y.Z`
- Release-preparation PR: URL, final head SHA, merge commit SHA
- Changed supporting crates and reason for each change: package, version, reason (or `none`)
- Package/version matrix: package | version | changed? | publication required?

## Immutable source references

- Annotated tag: tag name and tag-object SHA
- Peeled commit: commit SHA
- Binary workflow run ID:
- Recovery run ID(s): (or `none`)
- Post-merge check-run IDs and conclusions:

## GitHub release and asset verification

- Public release ID:
- Release state and immutable read-back:
- Asset read-back: exact seven asset names, provider-reported sizes, asset IDs
- SHA-256 read-back: each downloaded archive matched the downloaded `SHA256SUMS`
- Archive structure and locally compatible binary version:

## Public smoke evidence

- Public smoke run ID and tagged commit:
- `Unix installer (ubuntu-latest)`: conclusion and installed version
- `Unix installer (macos-latest)`: conclusion and installed version
- `Windows installer`: conclusion and installed version
- Public installer endpoints and final read-back:

## crates.io evidence and approval boundary

- Dry-run plan: exact package/version sequence (or `none`)
- Separate crates.io approval: approver, protected environment, and approval read-back
- crates.io workflow run ID and conclusion:
- Trusted Publisher/workflow read-back:
- Registry read-back: every published package/version confirmed independently and not yanked
- Clean command: `cargo install tapid --version X.Y.Z --locked`
- Clean install output: installed version and `tapid --help` result

## Limitations and follow-ups

- Limitations:
- Follow-up issues:
- Final repository, release, tag, workflow, and registry state read-back:

## Operator attestation

- [ ] GitHub publication approval was separate from crates.io approval.
- [ ] No secrets, credentials, tokens, cookies, private keys, signing material, or secret values are present.
- [ ] This issue records evidence only; no publication or workflow trigger was performed from it.
