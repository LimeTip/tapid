# Tapid

The Tapid command-line package manager.

This crate provides the `tapid` command-line interface.

The current CLI supports:

- `tapid init [PATH]`, which safely creates a private `package.json` without overwriting an existing file.
- `tapid manifest validate [PATH]`, which validates a manifest from disk and defaults to `package.json` in the current directory.

Package installation, dependency resolution, and registry access are still under development.

Tapid is developed and maintained by LimeTip Company and is licensed under the MIT License.
