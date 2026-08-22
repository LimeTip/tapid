# tapid-manifest

Pure parsing, validation, and deterministic serialization for selected npm-compatible `package.json` fields.

The crate validates required `name` and `version`, optional `private`, `description`, and `license`, string-valued dependency maps, and string-valued `scripts`. `PackageManifest::parse` accepts document text and `to_json` emits stable JSON with sorted map keys. Filesystem reads and mutation are intentionally owned by the CLI.
