# tapid-registry-client


[Crates.io](https://crates.io/crates/tapid-registry-client) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-registry-client)

Read-only, validated registry metadata and artifact-download boundary for Tapid.

## npm

`NpmRegistry` accepts an HTTPS npm registry origin, normally `https://registry.npmjs.org`, and package names such as `foo` or `@scope/name`. It reads the `versions` map, validates package and version identity, exposes string dependency requirements, and requires an HTTPS `dist.tarball`. Optional `dist.integrity` must be a valid `sha512-` SRI value. Metadata without integrity remains allowed for npm, subject to the consumer's policy.

## JSR

`JsrRegistry` accepts scoped names such as `@std/path` and the current `/{scope}/{name}/meta.json` shape. Version keys must be strict semver. Dependencies come from `manifest.dependencies` and `manifest.peerDependencies`. An artifact is returned only when metadata explicitly supplies an HTTPS `npm.tarball` and valid SHA-512 `npm.integrity`; integrity is never derived from the package name, version, URL, or response transport. Missing or unusable integrity returns `UnsupportedIntegrity`.

Live JSR installation and live integrity behavior are not verified. Local fixtures exercise the parser and fail-closed contract only.

## Transport contract

`download_artifact` accepts HTTPS URLs and uses an injected transport. `HttpsTransport` allows only configured exact origins, rejects cross-origin redirects, bounds response bodies, and sends no credentials. The standard origins include npmjs.org, jsr.io, and npm.jsr.io. Malformed JSON, URLs, identities, dependencies, and integrity values are rejected.

The crate is read-only. It does not publish, mutate registry state, authenticate private registries, retry, mirror, or run live calls in unit tests.
