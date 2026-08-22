# tapid-manifest

Pure parsing, validation, and deterministic serialization for the selected npm-compatible `package.json` fields.

The crate validates required `name` and `version`, optional `private`, `description`, and `license`, string-valued dependency maps, string-valued `scripts`, and package executable metadata. `bin` accepts either a string target or an object of command names to relative targets. Commands and targets are validated and exposed in deterministic order. Absolute, traversal, malformed, and unsupported bin values are rejected.

`PackageManifest::parse` accepts document text and `to_json` emits stable JSON with sorted map keys. Filesystem reads, archive extraction, link creation, and process execution are intentionally owned by other crates and the CLI.
