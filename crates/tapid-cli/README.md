# Tapid

The Tapid command-line package manager, implemented with Clap parsing and a separate command-dispatch layer.

Supported commands:

- `tapid init [PATH]` safely creates a private `package.json` without overwriting an existing file.
- `tapid manifest validate [PATH]` validates a manifest from disk and defaults to `package.json` in the current directory.
- `tapid lock verify` validates `tapid.lock` in the current directory.
- `tapid install [--offline] [--frozen]` validates `package.json` and replays a local `tapid.lock` into `node_modules` without network access or lifecycle scripts. Use `--project-dir PATH` for a dynamic project location.

Install is deliberately fail-closed: offline and frozen modes require a lockfile, malformed lockfiles are rejected before `node_modules` is created, and this vertical slice never executes package scripts or performs network resolution.
Parsing errors use Clap's standard exit code `2`; command execution and input failures use exit code `1`. Filesystem orchestration stays in this crate while `tapid-manifest` remains a pure document parser.
