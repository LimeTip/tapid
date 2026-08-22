# Tapid

The Tapid command-line package manager, implemented with Clap parsing and a separate command-dispatch layer.

Supported commands:

- `tapid init [PATH]` safely creates a private `package.json` without overwriting an existing file.
- `tapid manifest validate [PATH]` validates a manifest from disk and defaults to `package.json` in the current directory.
- `tapid lock verify` validates `tapid.lock` in the current directory.

Parsing errors use Clap's standard exit code `2`; command execution and input failures use exit code `1`. Filesystem orchestration stays in this crate while `tapid-manifest` remains a pure document parser.
