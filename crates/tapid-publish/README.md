# tapid-publish

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-publish)](https://crates.io/crates/tapid-publish)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-publish)](https://crates.io/crates/tapid-publish)
[![Docs.rs](https://docs.rs/tapid-publish/badge.svg)](https://docs.rs/tapid-publish)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Deterministic package packing and staged publication foundations for Tapid.

`PackageSource` describes a package root, immutable version label, and explicit
`ExclusionRules`. `NormalizedFileManifest::from_source` validates and normalizes
relative paths, excludes `.git`, `target`, and configured paths, hashes file
contents, and sorts entries lexicographically. `pack` emits a versioned,
byte-stable pack format and binds its exact bytes to a SHA-256 `ArtifactDigest`.

`PackageSource::with_limits` configures `PackLimits` for packing, inspection, and
previews. Defaults permit 100,000 files, 512 MiB per file, and 512 MiB for the
complete encoded artifact, including version, paths, and framing. Source paths
are limited to 4,096 bytes and directory recursion to 128 levels. Reads check
file metadata first and still stop if a file grows past the remaining budget.
Packing reads directly into the capped artifact buffer; inspection hashes with
a fixed-size buffer and applies the same byte accounting.

## Consistency and portable-path policy

`pack` reads each included file once. The encoded content and its manifest size
and digest come from the same captured bytes. Use the manifest returned by `pack` when
you need this binding; a separate `NormalizedFileManifest::from_source` call is
an independent read. Neither operation provides an atomic directory snapshot.
Keep the source tree stable and trusted during traversal and reads: concurrent
file changes, replacement by symlinks, and directory changes are not prevented.

After case-sensitive exclusions and separator normalization, complete file
paths with equal Rust `to_uppercase().to_lowercase()` keys are rejected. This
conservative casing policy includes ASCII case pairs and ordinary/final Greek
sigma (`σ`/`ς`). It deliberately can reject names that a particular filesystem
keeps distinct, such as `ß` and `ss`. Casing follows the Rust toolchain's Unicode
tables; it is not full Unicode case folding. Original path spelling is retained
in accepted artifacts.

This is **not** a universal portable-filesystem guarantee: Unicode normalization
(e.g. composed/decomposed accents), filesystem-specific casing tables, reserved
names, trailing dots/spaces, short-name aliases, and file/directory component
collisions with different complete paths are not covered. Collisions already
collapsed by the source filesystem cannot be recovered. Consumers still need
target-specific extraction validation. Non-UTF-8 source names are converted
lossily by the current traversal; this policy does not make them portable.

Native publishing remains future product scope; this crate supplies local pack
and preview/promote foundations, not a deployed registry or complete protocol.

Publication is deliberately transport-neutral. `Publisher::preview` performs
filesystem reads without writing source files; only `promote` invokes the injected
`PublicationTransport`. Repeated promotion is rejected only within the same
`Publisher` instance; durable immutability must be enforced by the transport and
registry. Public artifact/preview fields are mutable and are not revalidated by
`promote`. No registry, credentials, or
network transport is included in this crate.

## npm archive capability

`pack_npm(root, options)` is a separate operation from the custom `pack` format.
It returns immutable inspection data bound to completed gzip bytes. Entries are
sorted under `package/`, with GNU tar headers, zero uid/gid/mtime, empty owner
names, normalized modes, and gzip level 6 with zero mtime, no filename/comment,
and OS byte 255. Tar streams directly into gzip. Identical captured bytes and
normalized modes produce identical archives with the same encoder version.
Encoder upgrades require renewed determinism validation.

Selection-policy version 1 uses this precedence:

1. Unconditional exclusions and the caller-resolved destination dominate inclusion.
2. Without `files`, ordinary files are candidates; an empty `files` selects none.
3. Root `.npmignore` filters only without `files`. Nested policies always filter
   their subtree. Ancestor rules precede deeper rules; the last matching rule wins.
   Negation restores candidates without expanding the allowlist. Ignored directories
   are still traversed so negation can restore descendants.
4. Root `package.json`, README/LICENSE/LICENCE/COPYING with optional extensions,
   and existing declared main/bin targets are mandatory, subject to safety exclusions.
   Required targets must be regular files. Other exports/types/module targets are
   preserved metadata and are not validated.

Patterns support relative POSIX components, optional `./`, component-local `*`/`?`,
and whole-component `**`. Directory matches include descendants. Ignore patterns
support `!` negation, `/` anchoring, trailing `/` directory matching, empty lines,
and leading `#` comments. Slashless ignore patterns match basenames below their
policy directory. Whitespace is literal. Backslashes, bracket classes, braces,
extglobs, traversal, and files negation fail explicitly. This is a supported subset,
not npm packlist equivalence. `.gitignore` is not a fallback.

Case-insensitive basename exclusions at every depth are `.git`, `.hg`, `.svn`,
`node_modules`, `target`, `.tapid`, `.npm`, `.npmrc`, `.yarnrc`, `.yarnrc.yml`,
`.netrc`, `.env`, `.env.*`, `.DS_Store`, `npm-debug.log*`, `.npmignore`,
`package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`, `pnpm-lock.yaml`,
`bun.lock`, `bun.lockb`, `tapid.lock`, `*.tgz`, `.tapid-pack-*`, names ending
`~`, `.swp`, `.swo`, and names enclosed by `#`. `dist` is allowed. Inventory
review remains necessary: this policy does not detect arbitrary secrets.

Selected symlinks/special files and encountered non-excluded directory symlinks
are rejected; directory links are never traversed. Hard-linked files are captured
independently. Bin targets use 0755 everywhere. Other files use 0755 when Unix
executable bits are present and 0644 otherwise (0644 on Windows). Ownership and
other mode bits are discarded.

Paths reject invalid UTF-8, absolute/drive/UNC forms, dot/parent components,
backslashes, controls, Windows forbidden characters/device names, and trailing
dots/spaces. Directory and file component casing collisions use the custom
format's conservative uppercase/lowercase key. Accepted spelling is preserved.
Unicode normalization and universal filesystem portability are not guaranteed.

`NpmPackLimits` defaults: 100,000 selected files, 1,000,000 visited entries,
depth 128, 4,096 path bytes, 512 MiB per file and aggregate captured content,
640 MiB tar stream, 512 MiB compressed output, 1 MiB per ignore file, 8 MiB total
ignore input, 10,000 patterns, and 10 million matching steps. Exceeding a budget
fails without a successful partial artifact. The captured manifest is also bounded
by `tapid_manifest::MAX_MANIFEST_BYTES`. Peak memory includes captured content,
compressed output, inventory/pattern metadata, and bounded encoder buffers.

The manifest metadata and archived manifest derive from the same captured bytes.
File digests/sizes describe bytes actually encoded, and the artifact digest covers
completed compressed bytes. There is no atomic source snapshot: keep source and
destination trees trusted and stable. Preview and creation are independent reads.
`prepack`, `prepare`, and `postpack` never run; prepare build outputs separately.
No publication, registry transport, credentials, or Cloud service is involved.
