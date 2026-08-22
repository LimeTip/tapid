# tapid-registry-client

Read-only registry metadata foundations for Tapid.

## Supported sources

- `NpmRegistry` reads npm package metadata, validates package/version identity, and returns normalized tarball URLs plus `sha512-` integrity values.
- `JsrRegistry` reads scoped JSR `meta.json` documents and maps supported versions to the JSR npm-compatible downloadable artifact URL (`npm.jsr.io`).

Both clients depend on the injected `HttpTransport` trait, so tests and callers can supply a fixture transport without network access. `HttpsTransport` is the production implementation: it uses `reqwest` with Rustls, HTTPS-only explicit origin allow-listing, bounded response bodies, request timeouts, and redirects limited to the original allowed origin. It sends no credentials. Metadata is treated as untrusted input and rejects malformed JSON, wrong shapes, missing artifact fields, mismatched package/version identities, invalid URLs, and invalid integrity strings.

The crate is deliberately read-only. It does not publish, mutate registry state, or perform live calls in its test suite.
