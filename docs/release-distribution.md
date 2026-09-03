# Client release distribution

Status: implemented in GitHub Actions, pending verification on the next real release.

## Trust model

Tapid currently relies on the reviewed repository, an annotated version tag, GitHub Actions, GitHub Releases, HTTPS, and repository release immutability. Release archives are accompanied by `SHA256SUMS`.

The checksum detects corruption, truncation, and accidental asset substitution. Because the checksum and archive come from the same GitHub release, it is not an independent authenticity proof. Tapid does not currently claim a separate signed release trust root.

The installers accept only stable `vX.Y.Z` versions and HTTPS release endpoints. They select a platform-specific archive, enforce conservative checksum, archive, and executable size limits, verify its SHA-256 checksum, require exactly one expected regular executable in the archive, extract into a temporary directory, and stage the destination before replacement.

`tapid upgrade` is intentionally unavailable. Reinstall through the public installer when upgrading. Authenticated self-update metadata can be reconsidered if Tapid later needs mirrors, independent update authorization, or provider migration.

## Release process

1. Open and review a normal version-bump pull request.
2. Merge only after the ordinary CI and package checks pass.
3. Create and push an annotated `vX.Y.Z` tag at the reviewed commit. Never move a release tag.
4. The tag-triggered workflow validates that the tag is annotated, matches the Cargo version, and points to a commit on `main`, then builds six native archives.
5. The aggregation job requires the exact six archive names, writes `SHA256SUMS`, refuses to replace an existing release, creates a draft GitHub release, and reads back the exact seven-asset set.
6. Inspect the draft assets and generated notes manually.
7. Publish the draft through GitHub's release interface.
8. Publication triggers public installer smoke tests on Ubuntu, macOS, and Windows.
9. After the release is verified, manually dispatch the crates.io workflow with the same annotated tag. The protected environment provides a separate approval gate.

A workflow run does not publish the draft automatically. The manual draft review is the promotion boundary.

## TypeScript helper

`tools/release/release.ts` contains the small amount of orchestration that is awkward in shell:

- `check-tag TAG` requires a stable annotated tag and checks that it matches the `tapid` Cargo version.
- `checksums DIRECTORY VERSION` requires the exact six archives and writes deterministic SHA-256 lines.

Run its tests with:

```sh
node --experimental-strip-types --test tools/release/release_test.ts
```

## crates.io

The crates workflow is dispatched manually with an annotated release tag after binary release verification. It verifies that the matching GitHub release is public and non-prerelease and that the public installer smoke workflow succeeded for the same tag. It then uses GitHub OIDC and crates.io Trusted Publishing. It packages the workspace, checks exact versions against crates.io, and publishes only the missing `tapid` dependency closure in dependency order with ordinary `cargo publish -p NAME --locked` commands. No long-lived crates.io token is stored in GitHub.

Every crate that may appear in the publication plan must register the `LimeTip/tapid` repository, `crates-publication.yml` workflow, and `crates-io-release` environment as a crates.io Trusted Publisher before dispatch. The workflow fails rather than falling back to a stored registry token.

Supporting workspace crates are published only when they actually change and receive explicit version bumps. The product release workflow does not republish unchanged workspace crates.

## Residual risks

- GitHub repository or workflow compromise can replace both an archive and its checksum before publication.
- The installers do not enforce rollback protection beyond selecting a requested immutable release version.
- Public smoke tests run after publication and can detect but cannot prevent a broken release from briefly being available.
- macOS and Windows platform code signing are not part of this flow.
