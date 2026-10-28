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

`resolve_owned_graph_with_preferences` accepts `(BindingOwner, Dependency)`
declarations for a project root and named workspace members in one combined
request. Direct requirements intersect within each owner, registry, local import
name, and actual package name. Different owners can select incompatible versions;
compatible selections share exact graph identities. `OwnedResolution::bindings`
retains each owner's exact local binding, while `graph` contains the combined
transitive graph. Owner-qualified direct preferences and refresh requests preserve
unrelated owners' exact selections. Metadata and declaration limits apply to the
whole request, including owner names. Only `BindingOwner::Root` bindings provide
registry peers; member bindings never form a global provider pool. The original
root-only entry points retain their existing contract through the same graph
implementation. This resolver interface does not itself provide CLI workspace
placement or lockfile replay support.
