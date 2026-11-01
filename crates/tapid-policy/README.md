# tapid-policy

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-policy)](https://crates.io/crates/tapid-policy)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-policy)](https://crates.io/crates/tapid-policy)
[![Docs.rs](https://docs.rs/tapid-policy/badge.svg)](https://docs.rs/tapid-policy)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Explainable policy planning primitives. Evidence is typed as `declared`, `inferred`, `observed`, `enforced`, or explicitly ambiguous; policy decisions are `allow`, `warn`, `prompt`, or `deny` with stable reason codes and deterministic JSON serialization.

This crate makes no execution or containment claim. Unattended operation fails closed when evidence would require an interactive decision.

Audit policy uses `evaluate_audit` on verified v2 reports. The default
`AuditThreshold::Any` fails for every active advisory or reported-malware finding.
Explicit advisory floors are low, moderate, high and critical (inclusive).
Unrecognized or missing severity always meets the threshold, as does active
reported malware. Withdrawn reports do not trigger a finding decision.

`AuditOutcome::Incomplete` takes precedence over findings when collection is
incomplete. The caller must establish current verified evidence and complete
coverage; an empty iterator alone cannot establish a clean audit. Policy does not
perform signature verification, retrieval, authorization or persistence, and does
not discard reports below threshold from the caller's output.

`AuditOutcome::exit_code()` maps below-threshold, findings and incomplete results
to 0, 1 and 3 respectively. Invalid invocation/configuration uses CLI exit 2
before policy evaluation.
