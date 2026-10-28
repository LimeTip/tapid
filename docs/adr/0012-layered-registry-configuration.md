# Layered registry configuration and explicit network trust

Accepted direction for [issue #346](https://github.com/LimeTip/tapid/issues/346),
2026-10-28. Implementation remains incomplete: transport options and the layered
resolver exist, but production user/explicit selection, network configuration
plumbing and standalone inspection are still pending.

Corporate registry access must work without Tapid Cloud and without granting
scripts global execution authority. Keep configuration discovery, layer merging
and provenance in the CLI; keep HTTP proxy enforcement and certificate parsing in
`tapid-registry-client`. Reuse existing routing, verified storage and transactions.

Resolve built-in < user < selected project < explicit configuration, validating
every selected document even when a higher layer shadows it. Replace a registry
route atomically, including its token environment reference. A replacement without
a token reference removes the lower reference. Retain all declared credential
variable names for execution-environment exclusion without reading their values.

The optional user file is `tapid/config.toml` under absolute `XDG_CONFIG_HOME` on
Linux, falling back to absolute `HOME/.config`; under absolute
`HOME/Library/Application Support` on macOS; and under absolute `APPDATA` on Windows.
Unset or empty location variables are unavailable; nonempty relative values fail.
An absent optional user file succeeds. Explicit `--config PATH` resolves from the
invocation directory and requires a readable file. Each document is limited to
64 KiB. Do not search additional ancestors or expand shell expressions.

User and explicit files may contain only registry/network settings. Project run
policy keeps its separate parser and authority. Standalone tools and standalone
status use built-in/user/explicit layers, never caller-project configuration.
Pass their resolved configuration in memory rather than persisting it into tool
projects. This preserves the [standalone-tool authority decision](0009-standalone-tool-environments.md).

Network fields inherit independently. `proxy = "direct"` clears an inherited
proxy; otherwise permit one unauthenticated HTTP CONNECT proxy with explicit host
and port. Disable ambient proxy/bypass discovery. `ca-files` replaces the lower
collection, and an empty collection clears it. Resolve relative CA filenames from
the declaring document. Snapshot bounded regular-file bytes once per online
operation: at most 16 files, 1 MiB each, 4 MiB combined and 128 certificates.
Offline and inspection operations must not open CA files or resolve token values.

Additional roots supplement public roots and preserve hostname validation.
Registry Authorization stays inside exact-origin HTTPS requests, absent from
CONNECT and cross-origin requests. A configured corporate root deliberately
trusts its operator to issue registry certificates, including TLS interception.
Keep existing request/retry/redirect/body limits and separate Git/release transport
contracts. Do not add TLS bypasses, proxy authentication, HTTPS proxies, PAC, SOCKS
or client certificates in this subset.

Expose credential-free effective settings and sanitized provenance through the
existing status report and versioned outcome. Provenance is diagnostic metadata,
not part of preview's semantic routing identity. Reject supplied URLs, malformed
configuration and transport failures using safe categories rather than raw inputs
or remote error bodies.

Full npm compatibility and ambient proxy discovery would be more convenient, but
would make authority, credential isolation and headless reproduction less clear.
Do not implicitly read npmrc, npm environment configuration, auth helpers or npm
execution settings. Manual migration uses canonical HTTPS registry origins and
`token-env`; CI or a secret manager supplies the named environment variable.
Path-prefixed registries remain unsupported. Browser identity is separate
[issue #314](https://github.com/LimeTip/tapid/issues/314).
