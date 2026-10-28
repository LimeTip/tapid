# tapid-registry-client

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-registry-client)](https://crates.io/crates/tapid-registry-client)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-registry-client)](https://crates.io/crates/tapid-registry-client)
[![Docs.rs](https://docs.rs/tapid-registry-client/badge.svg)](https://docs.rs/tapid-registry-client)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Read-only, validated registry metadata and artifact-download boundary for Tapid.

## npm

`NpmRegistry` accepts an HTTPS npm registry origin, normally `https://registry.npmjs.org`, and package names such as `foo` or `@scope/name`. It requests npm's abbreviated install metadata, validates package and version identity, exposes string dependency requirements, and requires an HTTPS `dist.tarball`. Normal fetches require a valid SHA-512 `dist.integrity`, normalize equivalent padded and unpadded Base64 to canonical padded SRI, and exclude historical records that omit integrity. The explicit compatibility option can retain missing-integrity records for an interactive install, but those artifacts are not registry-authenticated and the CLI warns before proceeding.

`RegistryArtifact::dist_tags` carries the npm tags pointing to that exact version. Targets that are not strings or cannot be parsed as supported concrete versions are skipped, matching version-key handling. Missing or unusable tag targets are not replaced with another candidate. A `dist-tags` value that is not an object fails metadata validation.

`NpmRegistry::inspect` requests full npm metadata for one exact version and
returns `NpmPackageEvidence`. It validates identity, SHA-512 SRI and HTTPS
references, while retaining absent evidence instead of excluding the version.
`NpmPackageEvidence::from_metadata` applies the same contract to local snapshots.
It reports signature counts and attestation references without fetching or
verifying them. Malformed supplied evidence fails closed. Installation retains
its stricter artifact and integrity requirements.

Inspection also retains the selected version's `time[version]` publication time
and the package metadata's `time.modified`. Present timestamps must be valid
RFC 3339 strings and are normalized to RFC 3339 for display. Missing fields remain
absent; malformed timestamps fail validation. These registry-reported times are
unverified and do not establish artifact, provenance, or vulnerability freshness.

`PackagePlatform` remains re-exported here; its pure matching rules belong to `tapid-core`.

## JSR

`JsrRegistry` accepts scoped names such as `@std/path` and the current `/{scope}/{name}/meta.json` shape. Version keys must be strict semver. It preserves `manifest.dependencies` and `manifest.peerDependencies` as separate metadata maps. An artifact is returned only when metadata explicitly supplies an HTTPS `npm.tarball` and valid SHA-512 `npm.integrity`; integrity is never derived from the package name, version, URL, or response transport. Missing or unusable integrity returns `UnsupportedIntegrity`.

Live JSR installation and live integrity behavior are not verified. Local fixtures exercise the parser and fail-closed contract only.

## Transport contract

The crate-level `download_artifact` operation accepts canonical HTTPS URLs, uses an injected transport, and requires HTTP 200. The npm and JSR convenience methods delegate to it. `HttpsTransport` allows only configured exact origins, permits at most 10 same-origin redirect hops, rejects cross-origin and credentialed redirects, and bounds response bodies. `authenticated_metadata` and `authenticated_artifact` accept caller-supplied bearer credentials bound to exact configured origins; credentials are never forwarded to another origin. Standard metadata reads are limited to 32 MiB and archive reads are separately limited to 512 MiB. Idempotent GETs use at most three attempts with deterministic 100 ms and 200 ms delays for timeouts, connection or request failures, response-read failures, HTTP 429, and selected transient HTTP 5xx responses. Permanent status, origin, redirect-policy, and response-limit failures are not broadened into unbounded retries. The standard origins include npmjs.org, jsr.io, and npm.jsr.io. Malformed JSON, URLs, identities, dependencies, and integrity values are rejected.

The crate is read-only. It does not publish, mutate registry state, choose credential sources, or mirror. Unit tests use local fixtures and do not call live registries.

## HTTPS Git artifacts

`GitDependency` parses credential-free `git+https` repositories with branch, tag,
or commit references. A bounded smart HTTPS v0 exchange downloads a shallow pack
before Git can write it. Advertisements are limited to 32 MiB, pack responses and
archive output to 512 MiB, and packs to one million objects to bound index writes.
The advertised SHA-1 or SHA-256 format selects the receiving repository format.
Git validates the pack in a fresh private scratch directory, peels the reference
to a full commit, rejects submodules, and archives that commit through a bounded
pipe without an archive file. Each request and command has a 120-second deadline.
HTTP redirects, proxies, and credentials are disabled. Dumb HTTP repositories
and ambiguous branch/tag names are unsupported.
Git inherits no environment, credentials, user/system configuration, hooks,
redirect permission, or non-HTTPS protocol permission. No checkout or package
preparation code executes. Revision expressions and semver selectors are rejected.
The caller pins the returned archive digest and applies archive/store verification.
`RegistryPackageId` reuses core's source-qualified `PackageInstanceId`.

Selected npm install metadata (`RegistryArtifact::node_engine`) and exact-version
inspection (`NpmPackageEvidence::node_engine`) preserve Node engine declarations.
`NodeEngineRequirement` distinguishes inspected absence, a raw range string,
malformed fields, and unavailable evidence. Extraction does not evaluate a range,
execute Node, or establish archive-authenticated compatibility. JSR metadata
currently reports unavailable evidence.
