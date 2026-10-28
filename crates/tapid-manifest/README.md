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

`ManifestError::diagnostic` accepts the same input and returns the failed field,
map key, expected constraint, and observed JSON type. It does not copy arbitrary
field values. `Workspace::discover_typed` preserves this evidence and the
manifest path in `WorkspaceError::Manifest`; `discover` retains its string-error
interface for existing callers. `WorkspaceError::ManifestRead` retains the path
for manifest metadata, file-type, read, UTF-8, and size-limit failures while
preserving the existing error text.

`with_dependency_kind` adds a dependency to one section and removes declarations of that name from other sections. `update_dependency_kind` changes an existing declaration only in the selected section, preserving overlapping declarations and unrelated fields. It returns an error when that section does not declare the name.

`Workspace::discover` reads root and member manifests, checks canonical path containment, and selects members by exact package name. It accepts string, array, and `workspaces.packages` declarations with literal directory paths and `*` as a whole component for one directory level. Unsupported glob syntax fails with a diagnostic. Overlapping patterns are deduplicated, duplicate names are rejected, and manifests must be regular files. Discovery rejects root and member manifests larger than `MAX_MANIFEST_BYTES`, 32 MiB, before parsing and bounds the read itself if a file grows. See the [workspace contract](https://github.com/LimeTip/tapid/blob/main/docs/compatibility.md#npm-workspaces) for the exact supported subset.

`dependency_lifecycle_commands` preserves exact script bytes in lifecycle order. It includes the implicit `node-gyp rebuild` install hook when `binding.gyp` exists and neither `preinstall` nor `install` is declared. The CLI supplies that filesystem fact; parsing grants no execution permission.

`Workspace::discover_bounded` uses the same discovery rules with limits on candidate
manifests and expansion steps. Patterns, components, directory entries, literal
path expansions, and candidate probes consume steps. Candidate manifests count
before deduplication. Exceeding either budget returns
`WorkspaceError::DiscoveryLimitExceeded` before member parsing. Existing discovery
entry points retain their unrestricted enumeration behavior.

On Unix, manifest opens reject replacement symlinks and do not wait for FIFO
writers. Discovery checks the opened descriptor is a regular file before reading.

`Workspace::discover_bounded_with_member_source` passes each member's exact parsed
UTF-8 source to a borrowed callback. Snapshot consumers can fingerprint those bytes
before a later filesystem read. Discovery discards the source after each member;
the shared workspace model retains only parsed manifests. Callback observations
are partial if discovery fails.

`Workspace::discover_ancestor(directory, selector)` walks canonical ancestry for
the nearest workspace declaration, retaining the nearest standalone package as a
fallback. It returns the validated owner and exact selected member name (or None
for root). Explicit selectors override containing-member inference within that
owner. Invalid encountered manifests fail; undeclared intervening packages and
multiple containing members require explicit selection. It reuses the regular-file,
size, pattern, and canonical-containment checks above. Existing supplied-root and
bounded interfaces retain their exact-root behavior. See the workspace contract
for nested roots, nonmember directories, and symlink rules.
