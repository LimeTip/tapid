# tapid-resolver

Pure deterministic resolution for normalized npm- and JSR-compatible registry metadata.

Registry adapters own network access and normalize each registry into `RegistryMetadata` and
`PackageVersionMetadata`. The resolver only accepts those values; it never performs HTTP,
archive, or filesystem work. Package identities retain the registry origin, so identical
names and versions from npmjs.org and jsr.io remain distinct.

The supported requirement language is intentionally small and npm-compatible: exact
`1.2.3`, caret `^1.2.3`, tilde `~1.2.3`, and whitespace-separated intersections. Other
comparators and advanced npm range syntax fail with a structured `UnsupportedRange` error.

`resolve_graph` walks transitive dependency maps deterministically, handles cycles through
already-selected registry-qualified identities, and reports sorted requirements and
available versions for conflicts. Offline resolution succeeds only with supplied cached
metadata; frozen resolution is rejected until a lockfile replay input is provided by the
consumer layer. No network fallback is performed.
