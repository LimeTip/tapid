# tapid-manifest

Parsing and validation for selected npm-compatible `package.json` fields.

The current implementation validates:

- Required `name` and `version`.
- Optional `private`, `description`, and `license`.
- String-valued `dependencies`, `devDependencies`, `optionalDependencies`, and `peerDependencies`.
- String-valued `scripts` entries.
- Manifest files loaded directly from disk with `PackageManifest::from_path`.

Dependency names are validated through `tapid-core`, and supported fields serialize deterministically with stable field ordering and sorted map keys.

This crate is intentionally independent of the CLI. Filesystem mutation and user-facing command behavior belong to `tapid`, while manifest parsing and validation belong here. Unsupported `package.json` fields are currently ignored rather than interpreted.
