# tapid-linker

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-linker)](https://crates.io/crates/tapid-linker)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-linker)](https://crates.io/crates/tapid-linker)
[![Docs.rs](https://docs.rs/tapid-linker/badge.svg)](https://docs.rs/tapid-linker)
[![License](https://img.shields.io/crates/l/tapid-linker)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

`tapid-linker` owns deterministic planning for materializing verified package instances and executable shims into a project layout. It accepts registry-qualified identities, peer and platform context, verified content-addressed tree references, and package metadata. Plans are sorted and constrained to an explicitly managed absolute root with a `.tapid-managed` marker path.

`plan_layout` places root dependencies at `<project>/node_modules/<name>` and nested edges under the parent package's `node_modules`. `InstanceKey` includes package identity plus peer and platform context. Conflicting targets, duplicate instances, unknown edges, escaping paths, unsupported platforms, malformed `bin` metadata, missing targets, symlinks, special files, and shim collisions are rejected.

`plan_shims` selects Unix symlinks or Windows command and PowerShell wrappers. The planner validates that each bin target is a regular file inside its verified tree. The CLI materializes the plan during staged install and atomically activates the managed layout.

This crate does not execute scripts, enforce a process sandbox, authenticate registries, or itself mutate the filesystem. Runtime platform checks remain required. Other platforms have no supported link strategy in this release.
