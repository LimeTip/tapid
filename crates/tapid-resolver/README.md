# tapid-resolver


[Crates.io](https://crates.io/crates/tapid-resolver) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-resolver)

Pure deterministic resolution for normalized npm- and JSR-compatible registry metadata.

Registry adapters own network access and normalize metadata. The resolver accepts only those values and performs no HTTP, archive, or filesystem work. Package identities retain registry origin, so equal names and versions from npm and JSR remain distinct.

The supported requirement language is intentionally small: exact `1.2.3`, caret `^1.2.3` and major-only caret shorthand such as `^3`, tilde `~1.2.3`, and whitespace-separated intersections. Unsupported comparators, prereleases, unions, tags, aliases, git, file, workspace, and other advanced npm syntax return structured `UnsupportedRange` errors.

`resolve_graph` walks transitive dependency maps deterministically, handles cycles through registry-qualified identities, and reports sorted requirements and candidates for conflicts. Offline resolution succeeds only with supplied cached metadata. Frozen resolution requires a lockfile replay input from the consumer layer and never falls back to the network.
