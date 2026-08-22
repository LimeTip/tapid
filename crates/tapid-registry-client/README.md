# tapid-registry-client

Read-only, validated registry metadata and artifact-download boundary for Tapid.

## Supported registry forms and limitations

- **npmjs.org:** `NpmRegistry` accepts an HTTPS npm registry origin (normally `https://registry.npmjs.org`) and package names such as `foo` or `@scope/name`. It reads the npm `versions` map, validates each version's package/version identity, exposes `dependencies` as validated `PackageName` keys with non-empty requirement strings, and requires a valid `dist.tarball` URL. npm `dist.integrity`, when present, must be a `sha512-` SRI value; npm metadata may omit integrity.
- **jsr.io:** `JsrRegistry` accepts scoped packages such as `@std/path` and the current `/{@scope}/{name}/meta.json` response shape (`scope`, `name`, `latest`, and `versions` records such as `createdAt`). Version keys must be strict semver. Dependencies are read from documented `manifest.dependencies` and `manifest.peerDependencies` objects. An artifact is returned only when the version explicitly supplies an HTTPS `npm.tarball` and a valid SHA-512 SRI `npm.integrity`; no URL or digest is derived from the package name, version, or response transport. Versions without a trustworthy integrity value fail closed with `UnsupportedIntegrity`.

Both clients return normalized per-version `RegistryArtifact` records containing the registry-qualified package identity, registry kind, dependency map, artifact URL, and integrity. Dependency requirements intentionally remain opaque strings; resolver policy and semver conversion belong outside this boundary.

`download_artifact` accepts only HTTPS URLs and delegates to the injected transport. With `HttpsTransport`, the URL must be in the configured exact-origin allow-list, redirects may not cross origins, and the response body is bounded by the configured byte limit. The standard allow-list includes `registry.npmjs.org`, `jsr.io`, and `npm.jsr.io`; no credentials are sent. Metadata is untrusted input and malformed JSON, fields, URLs, identities, dependencies, and integrity values are rejected.

The crate is deliberately read-only. It does not publish, mutate registry state, or perform live calls in its test suite.
