# Security Policy

Tapid is currently in the design and planning stage. There is no production Tapid registry or released Tapid binary in this repository yet.

## Reporting a vulnerability

Please do not disclose security vulnerabilities in public issues, discussions, pull requests, or social media.

Use the private security advisory mechanism for the [`tapid-dev/tapid`](https://github.com/tapid-dev/tapid) repository when available. If it is unavailable, use the official private contact method listed on [tapid.dev](https://tapid.dev).

A useful report includes:

- A short description and affected component.
- Reproduction steps or a minimal proof of concept.
- Security impact and attack assumptions.
- Affected commit, version, platform, or configuration.
- Suggested mitigation, if known.

Do not include passwords, access tokens, private customer data, or unrelated personal information. Replace secrets with `[REDACTED]`.

## Research expectations

Security testing must be authorized and proportionate. Do not access, alter, or disrupt systems, registries, packages, or data that you do not own or have permission to test.

Please allow maintainers reasonable time to validate and address a report before public disclosure. We will coordinate disclosure and credit with the reporter where appropriate.

## Security scope

Reports may concern:

- Dependency resolution or lockfile integrity.
- Registry identity, scope routing, and dependency confusion.
- Credential handling and cross-origin redirects.
- Archive extraction, path traversal, symlink escape, or resource exhaustion.
- Store integrity, cache poisoning, or unsafe cleanup.
- Lifecycle-script approval and executable isolation.
- Unattended policy bypasses.
- Signed metadata, provenance, attestations, revocation, or rollback protection.
- Private registry authorization or tenant isolation.
- CI, release, installer, website, or registry infrastructure.

## Security principles

Tapid will treat package metadata, archives, scripts, native binaries, and executable code as untrusted. It will verify artifacts by digest, disable dependency lifecycle scripts by default, fail closed for unsafe unattended decisions, keep private registry routing explicit, and separate factual evidence from policy decisions.
