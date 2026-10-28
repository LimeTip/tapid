# tapid-archive

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-archive)](https://crates.io/crates/tapid-archive)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-archive)](https://crates.io/crates/tapid-archive)
[![Docs.rs](https://docs.rs/tapid-archive/badge.svg)](https://docs.rs/tapid-archive)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

`tapid-archive` validates archive entry metadata before extraction. Names using either slash style are checked. Traversal, absolute, drive, and UNC paths are rejected. Entry count, path length, compressed size, extracted size, and path limits are explicit. Duplicate and case-colliding names, links, device nodes, and other special files are rejected. Tar and tar.gz archives are extracted into a fresh staging directory.

Validation applies entry and aggregate limits as each logical header arrives,
before traversing that entry's payload. Size checks use the effective size,
including PAX overrides. Decompressed tar framing and extension metadata have a
64 KiB budget between logical entries, in addition to the preceding validated
payload. This also bounds GNU/PAX metadata consumed internally by the parser.
After validation or extraction fails, extraction attempts to remove the newly
created staging directory. Filesystem cleanup can fail; the original processing
error is returned even if staging removal fails.

Unix extraction normalizes regular files to `0644` or `0755`, preserves only the executable distinction, and strips ownership and privilege bits. A canonical internal mode manifest makes executable-aware tree digests identical across Unix and Windows while Unix verification also checks the actual mode. The crate does not scan executable content or provide malware detection; store activation remains a separate layer.

`validate_tree` applies portable archive bounds to generated filesystem outputs and rejects symlinks, special files, noncanonical paths, and reserved store metadata. Canonical tree hashing streams regular-file contents while preserving the established digest framing.

`canonical_tree_digest_without_root_entry` hashes an explicitly filtered tree view. The caller must omit the same single root entry from its private copy. Dependency-tool preparation uses this for generated npm `.bin` shims; ordinary archive and store verification continue to use the complete canonical tree digest. Links elsewhere remain rejected.

Git commit tar archives may contain one global PAX header with a full hexadecimal
commit comment. That bounded, inert header is validated and omitted from the
tree. Other global PAX fields and duplicate global headers remain rejected.

A preflight inspects at most 64 KiB of decompressed leading metadata and stops
before the first package payload. It rejects GNU long name and long link
declarations above the configured path limit plus their terminator, global PAX
declarations above 1 KiB, and local PAX declarations above 64 KiB before reading
those bodies.
The complete logical validation pass bounds all remaining parser metadata and
honors PAX size overrides when checking entry and aggregate sizes.

`capture_directory` is an initial Unix-only directory snapshot capability; the
CLI does not yet consume it. It accepts an explicit project root, a normalized
relative source path, relative forbidden managed roots, and archive limits.
Descriptor-relative no-follow opens reject links and special or multiply linked
files. It includes dotfiles and empty directories, excludes real `.git` and
`node_modules` directories, and requires a regular root `package.json` of at most
1 MiB. Entries are sorted by path bytes and emitted as GNU tar with zero ownership
and timestamps, normalized executable modes, deterministic GNU long-name records,
and two terminating zero blocks. Existing entry/path/payload limits and the final
archive byte ceiling apply. Returned bytes still require normal archive/store
verification; capture does not publish artifacts.

Observable identity, size, mode, and timestamp changes abort capture. This is not
an atomic filesystem snapshot: callers must stop writers for a coherent view,
and same-length concurrent writes can evade metadata comparisons. Non-Unix
platforms currently return `Unsupported`; Windows object-safe traversal, additional
directory/ancestor race coverage, and CLI-level declaration/overlap/replay policies remain required
before directory dependencies can be advertised.
