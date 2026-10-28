# tapid-resolver

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-resolver)](https://crates.io/crates/tapid-resolver)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-resolver)](https://crates.io/crates/tapid-resolver)
[![Docs.rs](https://docs.rs/tapid-resolver/badge.svg)](https://docs.rs/tapid-resolver)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Pure deterministic resolution for normalized npm- and JSR-compatible registry metadata.

Registry adapters own network access and normalize metadata. The resolver accepts only those values and performs no HTTP, archive, or filesystem work. Package identities retain source identity, so equal names and versions from registries, copied file tarballs, and pinned Git archives remain distinct.

Normalization and resolution reject metadata above 100,000 version records,
1,000,000 combined dependency, peer, optional-peer, and tag declarations, or
64 MiB of metadata text and retained parsed requirement storage, including tags,
alias targets, range clauses, comparators, and prerelease identifiers even when
callers edit the public diagnostic text. Resolution applies these bounds across
all supplied registries before building its candidate index, including
caller-edited records that bypass normalization. Root registries, names, and
retained requirements share that byte budget before root constraints are cloned.
Root declarations are also capped at 100,000. Oversized input returns
`MetadataLimitExceeded`.

The supported npm range contract is differentially tested against pinned `node-semver` 7.8.5: exact and partial versions, `x`/`*` wildcards, comparator sets and intersections, caret/tilde including zero-major boundaries, hyphen ranges, and non-empty `||` alternatives. Empty range strings have npm wildcard semantics at the range parser; registry metadata separately rejects blank dependency declarations as malformed. Prerelease eligibility follows npm's same-base comparator rule, and build metadata is ignored for satisfaction. Candidate selection remains a separate deterministic policy. Git, file, and workspace declarations are source specifications rather than ranges and return structured `UnsupportedRange` errors at this parser. The CLI prepares immutable copied source metadata before resolution. Dist-tags are separate from version ranges: `Requirement::matches` cannot match a tag without metadata, while candidate selection uses each exact version's `dist_tags`. Tags select their exact target, including prereleases, and missing tags never fall back to the highest version.

`resolve_graph` walks transitive dependency maps deterministically, handles cycles through registry-qualified exact identities, and can select different versions of one transitive package for different parent edges. `resolve_graph_with_routing` accepts a caller-owned route selector for each transitive dependency and peer provider; root identities remain caller-selected, and routing failures are returned as `RegistryRouting` errors. It reports sorted requirements and candidates for conflicts. Offline resolution succeeds only with supplied cached metadata. Frozen resolution requires a lockfile replay input from the consumer layer and never falls back to the network.

Candidate failures retain `RequirementOrigin`, including the local dependency
name and the exact declaring package for transitive edges. Root origins leave
the declaring package absent so callers can attach their manifest locations.
Peer failures retain typed requiring/provider identities, including source and
version, instead of flattening those identities to strings.

Npm alias requirements retain the actual target name alongside the parsed version range. Resolution selects the actual registry identity while retaining local root bindings and parent edge names. Routing callbacks receive the actual target name. Peer providers are selected by local root name and validated against the actual target registry; explicit alias peers must also match the target name. Distinct local aliases can select different versions of one actual package.

`resolve_graph_with_preferences` accepts exact locked version preferences. Compatible preferences precede the highest-version rule while retaining registry identity and range checks. The CLI supplies locked graph metadata and requests fresh metadata only when the prior candidates cannot satisfy changed inputs. `refresh_roots` selects the highest compatible versions for named local root bindings while retaining locked preferences for transitive edges and other aliases of the same actual package. Frozen installs replay the exact graph outside fresh resolution.

`resolve_instances_with_preferences` is the contextual selection interface under
construction for [#306](https://github.com/LimeTip/tapid/issues/306). It shares
ordinary candidate selection with the legacy resolver and returns deterministic
placement references, source-qualified artifacts, named ordinary edges, and
separate exact peer-provider references. Distinct placements remain distinct,
including when their artifact and provider versions match.

All ordinary siblings are allocated before peer binding. Lookup uses the local
peer name in the consumer's enclosing scope and then outward; the consumer's
private dependencies cannot satisfy its own peers. The nearest binding shadows
outer bindings. Required absence fails; optional absence succeeds without adding
providers. Visible incompatible, wrong-origin, or alias-target mismatched
providers fail even for optional peers. No provider is installed or hoisted.
Contextual errors retain the consumer placement, expected source, and nearest
provider; their display text is bounded to 8 KiB with explicit truncation.

Sibling mutual peers use finite preallocated references. Dependency recurrence
uses an ancestor reference only when enclosing lookups agree on exact references
for every reachable peer name. This permits peerless cycles and cycles with
unchanged providers, including peer-bearing descendants. The comparison is
conservative: a difference for a name that would be shadowed inside the repeated
subtree can still cause rejection. Context-changing cycles fail before returning
a graph. Expansion is bounded to 100,000 instances, 1,000,000 combined root,
ordinary, and peer edges, depth 256, and 64 MiB of retained graph text accounting.
Cycle analysis also has a 1,000,000-step work budget.

The contextual interface does not yet retain contextual locked preferences,
workspace scopes, or platform-excluded declaration evidence; metadata remains
caller-filtered. Schema 11 persistence, exact linking, native maintenance, and
Node runtime acceptance remain pending. The CLI continues to use the legacy
direct-root contract. Do not flatten contextual results into version-only peer
summaries for installation or infer native maintenance support from these
resolver tests. See [ADR 0012](../../docs/adr/0012-contextual-peer-instance-graphs.md).
