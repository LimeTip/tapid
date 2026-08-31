# tapid-resolver


[Crates.io](https://crates.io/crates/tapid-resolver) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-resolver)

Pure deterministic resolution for normalized npm- and JSR-compatible registry metadata.

Registry adapters own network access and normalize metadata. The resolver accepts only those values and performs no HTTP, archive, or filesystem work. Package identities retain registry origin, so equal names and versions from npm and JSR remain distinct.

The supported requirement language is intentionally bounded: exact stable or prerelease versions, caret ranges such as `^1.2.3` and `^2.0.0-rc.1`, major-only caret shorthand such as `^3`, tilde ranges, whitespace-separated intersections, and non-empty `||` alternatives. Stable ranges do not select prerelease candidates. Unsupported comparators, empty alternatives, tags, aliases, git, file, workspace, and other npm syntax return structured `UnsupportedRange` errors.

`resolve_graph` walks transitive dependency maps deterministically, handles cycles through registry-qualified exact identities, and can select different versions of one transitive package for different parent edges. It reports sorted requirements and candidates for conflicts. Offline resolution succeeds only with supplied cached metadata. Frozen resolution requires a lockfile replay input from the consumer layer and never falls back to the network.
