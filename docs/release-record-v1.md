# Release record v1

This contract is implemented for the next release, expected to be Tapid 0.0.11. Its presence in the repository does not establish that the release or website routes have been published. See the [release runbook](release-distribution.md) for the first-release cutover.

A release record describes the version and exact downloadable archives. It is served over HTTPS and supplies SHA-256 integrity checks. It is not a signed release manifest and does not establish an independent release trust root.

## Format

The release asset is named `tapid-release-v1.tsv`. It contains an ASCII header and tab-separated artifact rows, terminated by LF. There is exactly one final LF, no blank lines, and no CR characters. The complete response is at most 262144 bytes.

```text
tapid-release-v1<TAB>VERSION<LF>
TARGET<TAB>ARCHIVE_NAME<TAB>SIZE_DECIMAL<TAB>SHA256_LOWERHEX<TAB>HTTPS_URL<LF>
```

`<TAB>` and `<LF>` above denote single bytes, not literal text.

- `VERSION` has exactly three decimal components, each from 0 through 18446744073709551615. A component has no leading zero unless it is `0`. There is no `v` prefix, prerelease suffix, or build metadata. Any major version is supported.
- `TARGET` is nonempty and contains only ASCII letters, digits, underscores, and hyphens. Targets must be unique. Consumers validate every row and require a row for their selected target. Unknown targets remain available for future platform additions.
- `ARCHIVE_NAME` is exactly `tapid-VERSION-TARGET.tar.gz`.
- `SIZE_DECIMAL` is the compressed archive's byte count, from 1 through 536870912 inclusive, with no leading zero.
- `SHA256_LOWERHEX` is exactly 64 lowercase hexadecimal characters and binds the downloaded archive bytes.
- `HTTPS_URL` is an absolute ASCII URL beginning with `https://`. The authority matches `[A-Za-z0-9][A-Za-z0-9.-]*(:[0-9]+)?`, covering ordinary DNS names and IPv4 addresses with an optional decimal port. IPv6 literals are outside v1. Whitespace, control characters, credentials, `@`, backslashes, query strings, and fragments are forbidden. The publisher supplies a version-specific immutable download URL. Readers do not infer provider-specific URL paths.

The current publisher requires exactly these six regular, nonempty archives and emits rows in this order:

```text
aarch64-apple-darwin
aarch64-pc-windows-msvc
aarch64-unknown-linux-gnu
x86_64-apple-darwin
x86_64-pc-windows-msvc
x86_64-unknown-linux-gnu
```

`tools/release/release.ts metadata DIRECTORY VERSION BASE_URL` generates the record from archive bytes. `BASE_URL` is the immutable directory for that exact version. The release workflow generates `SHA256SUMS` and the record beside the same six archives, uploads all eight assets to one draft, and reads back the asset names. Draft review must compare the record's version, URLs, sizes, and hashes with downloaded assets before publication.

## Public addresses

The public discovery addresses are:

- `https://tapid.dev/releases/v1/latest.tsv`
- `https://tapid.dev/releases/v1/vVERSION.tsv`

The website routes redirect to the release provider's metadata asset. Initially the latest route points to `https://github.com/LimeTip/tapid/releases/latest/download/tapid-release-v1.tsv`; a version route points to `https://github.com/LimeTip/tapid/releases/download/vVERSION/tapid-release-v1.tsv`. Ordinary releases need no website edit. Moving providers requires changing these routes and publishing compatible records and archives at the new provider.

Both installers and the new default upgrade path consume this contract. The CLI accepts `--release-url` or `TAPID_RELEASE_RECORD_URL` to override the record address. The installers accept `TAPID_RELEASE_RECORD_URL`. An explicit installer version through 0.0.10 uses the historical GitHub archive and `SHA256SUMS` path unless a record override is supplied. These historical releases do not contain this asset.

`/stable.json` remains the historical signed-channel-index address. Do not place this TSV record or unsigned JSON there. Tapid 0.0.10 would reject received metadata of the wrong format before attempting its GitHub fallback. Explicit CLI `--endpoint` continues to select the legacy signed protocol; it is separate from `--release-url`.

## Failure and trust behavior

Received malformed metadata, unsupported targets, invalid archive sizes, and digest mismatches fail before activation. The default upgrade path does not fall back to GitHub discovery after a record error. Metadata transport unavailability can use a previously verified local recovery cache, but reports that the latest release could not be checked. It cannot claim a successful latest-release check from cached state.

After successful download verification, the client validates archive contents and stages executable replacement. Identical executable bytes leave the installation unchanged. The local release floor prevents normal rollback below recorded state, but does not resist hostile modification of that local state.

Compromise of the domain, its route configuration, the release provider, or the publishing workflow can substitute both a release record and an archive. SHA-256 detects mismatched bytes; it does not independently authorize a publisher. This contract has no signing keys or periodic metadata re-signing obligation.
