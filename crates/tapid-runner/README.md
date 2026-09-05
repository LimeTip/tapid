# tapid-runner

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-runner)](https://crates.io/crates/tapid-runner)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-runner)](https://crates.io/crates/tapid-runner)
[![Docs.rs](https://docs.rs/tapid-runner/badge.svg)](https://docs.rs/tapid-runner)
[![License](https://img.shields.io/crates/l/tapid-runner)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Policy-aware package execution foundations for Tapid. Existing approval APIs bind an exact artifact digest to the SHA-256 hash of a normalized script.

`RunConfig::parse_toml` accepts checked-in `[run.defaults]` and `[run.scripts.<name>]` profiles. Profiles declare project-relative read/write grants, network access, environment-variable names, subprocess access, and optional resource limits. Configuration is strict, denies unknown fields, and cannot disable required sandboxing.

The platform-neutral execution API validates requests and reports stable errors, termination states, and per-dimension containment evidence. Support and post-execution receipts identify the backend name, version, and deprecation state; distinguish requested, supported, and actually enforced filesystem-read, filesystem-write, network, environment-sanitization, descendant-lifecycle, and resource-limit dimensions; and expose configured limits and canonical filesystem grants. Receipt fields are private, so only crate-owned backends can produce enforcement claims.

`ExecutionRequest` carries only explicit environment values whose names are both valid and allowlisted by the selected policy. Backends must start from an empty child environment rather than inherit ambient variables. Platform backends are not implemented. `execute` therefore fails closed before spawning a child and cannot produce an enforcement receipt. `SandboxMode::Disabled` exists only as an explicit programmatic override for a future CLI boundary; it cannot be selected from checked-in configuration.
