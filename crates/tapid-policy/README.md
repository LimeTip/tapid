# tapid-policy

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-policy)](https://crates.io/crates/tapid-policy)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-policy)](https://crates.io/crates/tapid-policy)
[![Docs.rs](https://docs.rs/tapid-policy/badge.svg)](https://docs.rs/tapid-policy)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Explainable policy planning primitives. Evidence is typed as `declared`, `inferred`, `observed`, `enforced`, or explicitly ambiguous; policy decisions are `allow`, `warn`, `prompt`, or `deny` with stable reason codes and deterministic JSON serialization.

This crate makes no execution or containment claim. Unattended operation fails closed when evidence would require an interactive decision.

Package security configuration is being implemented separately from capability
policy. `SecurityPolicy` validates version 1 malware, vulnerability-threshold and
content-analysis requirements. The current `unavailable_evidence` gate denies
required rules and warns for advisory rules; it does not consume or authenticate
security reports, evaluate findings or implement review exceptions.

The CLI recognizes this checked-in configuration in `tapid.toml`:

```toml
[security]
version = 1

[[security.rules]]
id = "malware"
kind = "malware"
mode = "required"
sources = ["approved-provider"]
max-age-seconds = 3600
status = "cached"
```

Rules require explicit mode (`required` or `advisory`), sources, positive maximum
age in seconds and status requirement (`cached` or `current`). Vulnerability
rules use `kind = "vulnerability"` and `threshold = "low"`, `"moderate"`, `"high"`
or `"critical"`. Content-analysis rules use `kind = "content_analysis"` plus
`profile` and `revision`. Unknown fields, duplicate IDs and invalid combinations
are rejected. The configuration remains bounded by the CLI's 64 KiB limit.

Until the shared authenticated audit/evidence consumer is integrated, every
explicit required rule denies installation before preparation, hooks or store
publication, including offline/frozen replay. Advisory-only rules warn that
coverage was not established; they introduce no service request. Omitting the
section introduces no security-evidence requirement. Cached package bytes and
lifecycle approvals do not satisfy a requirement. This initial gate is not
complete evidence-backed policy enforcement or a release support claim; tool
policy persistence, exact exceptions and two-gate evidence validity remain
unimplemented.
