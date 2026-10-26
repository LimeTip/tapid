# tapid-attestations

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-attestations)](https://crates.io/crates/tapid-attestations)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-attestations)](https://crates.io/crates/tapid-attestations)
[![Docs.rs](https://docs.rs/tapid-attestations/badge.svg)](https://docs.rs/tapid-attestations)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Typed artifact-bound claims covering issuer, methodology, scope, issue/expiry, findings, confidence, limitations, and payment disclosure. Claims require canonical RFC 3339 timestamps with an explicit `Z` or numeric offset; issue and expiry are compared as instants before conversion to an unsigned canonical trust envelope. The unsigned envelope remains a transport/canonicalization primitive and is not evidence of signature verification or trust.

`verify_security_evidence` checks bounded security-result JSON, contextual Ed25519 signatures against a dedicated caller-owned issuer keyring, exact request/digest binding, coverage, trust expiry, analysis age and replay state. Its accepted result still requires status evaluation and does not certify safety. Public fixtures run without Cloud or accounts. Trust distribution, persistent replay floors and hosted authorization are caller responsibilities. See [the public v1 contract](../../docs/security-evidence.md).
