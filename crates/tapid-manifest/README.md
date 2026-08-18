# tapid-manifest

Parsing and validation for npm-compatible `package.json` manifests.

The current implementation validates the required `name` and `version` fields, the optional `private` flag, and string-valued `dependencies`. It also provides deterministic serialization for the supported fields.

This crate is intentionally independent of the CLI. Filesystem mutation and user-facing command behavior belong to `tapid`, while manifest parsing and validation belong here.
