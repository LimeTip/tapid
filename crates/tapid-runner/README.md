# tapid-runner

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-runner)](https://crates.io/crates/tapid-runner)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-runner)](https://crates.io/crates/tapid-runner)
[![Docs.rs](https://docs.rs/tapid-runner/badge.svg)](https://docs.rs/tapid-runner)
[![License](https://img.shields.io/crates/l/tapid-runner)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Policy-aware package execution foundations for Tapid. Existing approval APIs bind an exact artifact digest to the SHA-256 hash of a normalized script.

`RunConfig::parse_toml` accepts checked-in `[run.defaults]` and `[run.scripts.<name>]` profiles. Profiles declare project-relative read/write grants, network access, environment-variable names, subprocess access, and optional resource limits. `/` is the sole portable path separator; backslashes are rejected on every host. Configuration is strict, denies unknown fields, and cannot disable required sandboxing.

The platform-neutral execution API validates requests and reports stable errors, termination states, and per-dimension containment evidence. Support and post-execution receipts identify the backend name, version, and deprecation state and distinguish four evidence states: restrictions requested by policy, capabilities declared by the backend, capabilities observed by runtime probes, and restrictions actually enforced for the execution. Each state reports filesystem read, filesystem write, network, environment sanitization, subprocess restriction, descendant lifecycle, timeout, output, process-count, and memory dimensions independently. Receipts also expose configured limits and the exact resolved filesystem grants from canonical preflight. Receipt fields and construction remain private, so external callers cannot manufacture enforcement claims.

`ExecutionRequest` carries only explicit environment values whose names are both valid and allowlisted by the selected policy. Backends must start from an empty child environment rather than inherit ambient variables. The sandboxed `execute` path rejects `SandboxMode::Disabled` and validates containment evidence plus canonical, confined filesystem grants before its private backend spawn seam. Existing grants are canonicalized; missing declared targets are resolved through a canonical existing ancestor and a validated relative suffix. Explicit backend runtime additions must be typed, absolute, existing, and canonical. Platform backends are not implemented, so `execute` fails closed before spawning a child and cannot produce an enforcement receipt. `SandboxMode::Disabled` exists only for a future distinct unsandboxed CLI outcome that must not contain an enforcement receipt; this crate does not implement that escape.
