# tapid-manifest

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-manifest)](https://crates.io/crates/tapid-manifest)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-manifest)](https://crates.io/crates/tapid-manifest)
[![Docs.rs](https://docs.rs/tapid-manifest/badge.svg)](https://docs.rs/tapid-manifest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Pure parsing, validation, and deterministic serialization for the selected npm-compatible `package.json` fields.

The crate validates required `name` and `version`, optional `private`, `description`, and `license`, string-valued dependency maps, string-valued `scripts`, and package executable metadata. `bin` accepts either a string target or an object of command names to relative targets. Commands and targets are validated and exposed in deterministic order. Absolute, traversal, malformed, and unsupported bin values are rejected.

`PackageManifest::parse` accepts document text and `to_json` emits stable JSON with sorted map keys. Archive extraction, link creation, and process execution are owned by other crates and the CLI.

`with_dependency_kind` adds a dependency to one section and removes declarations of that name from other sections. `update_dependency_kind` changes an existing declaration only in the selected section, preserving overlapping declarations and unrelated fields. It returns an error when that section does not declare the name.

`Workspace::discover` reads root and member manifests, checks canonical path containment, and selects members by exact package name. It accepts string, array, and `workspaces.packages` declarations with literal directory paths and `*` as a whole component for one directory level. Unsupported glob syntax fails with a diagnostic. Overlapping patterns are deduplicated, duplicate names are rejected, and manifests must be regular files. See the [workspace contract](https://github.com/LimeTip/tapid/blob/main/docs/compatibility.md#npm-workspaces) for the exact supported subset.

`dependency_lifecycle_commands` preserves exact script bytes in lifecycle order. It includes the implicit `node-gyp rebuild` install hook when `binding.gyp` exists and neither `preinstall` nor `install` is declared. The CLI supplies that filesystem fact; parsing grants no execution permission.
