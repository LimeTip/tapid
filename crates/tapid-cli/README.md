# tapid

[Crates.io](https://crates.io/crates/tapid) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-cli)

The `tapid` command-line client for the Tapid JavaScript and TypeScript package manager, written in Rust. It provides deterministic installation and lockfile replay, verified package storage, Node-compatible linking, and explicit root-script execution.

## Commands

```text
tapid init [PATH]
tapid manifest validate [PATH]
tapid lock verify
tapid install [OPTIONS]
tapid run <SCRIPT> [-- <ARGS>...]
```

`tapid init` creates a private `package.json` without overwriting an existing file. Manifest and lock commands validate the selected files. Paths default to the current directory and `package.json` where applicable.

## Install

The supported install paths are the live npm path, validated lockfile replay, and the local registry fixture:

```text
tapid install --project-dir ./example
tapid install --offline --frozen --project-dir ./example
tapid install --registry-fixture ./fixture.json --project-dir ./example
```

The fixture option is for local tests and air-gapped development. It is not a registry authentication or production mirror feature. The live npm path resolves supported transitive ranges, requires registry-declared SHA-512 integrity by default, selects compatible optional packages for the current OS/CPU/libc target, verifies extracted trees, writes schema 5 locks, and stores trees in the platform cache outside the consumer project. `--allow-unverified-registry-artifacts` is an explicit online-only compatibility exception and emits a warning.

## Offline and frozen

```text
tapid install --offline --project-dir ./example
tapid install --frozen --project-dir ./example
tapid install --offline --frozen --store-dir ./verified-store
```

Both flags require `tapid.lock` and all referenced verified trees. Replay validates the root manifest digest, exact package identities, tree digests, regular `.tapid-tree` markers, and available store content before staging. It performs no network resolution or archive download. Activation replaces managed `node_modules` atomically; failed validation or staging does not intentionally activate partial output.

`--frozen` currently selects the same no-network replay path as `--offline`. It does not yet implement the complete npm frozen-lockfile policy.

## Run and `.bin`

```text
tapid run init
tapid run dev -- --host 127.0.0.1
tapid run --project-dir ./example test -- --runInBand
```

The command reads a root script from `package.json`, runs in the canonical project directory, prepends `<project>/node_modules/.bin` to `PATH`, preserves inherited environment variables, forwards arguments after `--`, and propagates the child exit code. Missing scripts fail with exit code `1`. Clap parsing errors use exit code `2`.

Install derives executable shims from verified package `bin` metadata. Unix uses symlinks. Windows writes `.cmd` and PowerShell wrappers. The planner rejects malformed metadata, absolute or traversal targets, symlink and special-file targets, collisions, and unsupported platforms. Root scripts may execute arbitrary code through the platform shell. The runner is not a sandbox and does not approve or contain scripts.

## Lifecycle policy and limitations

- Dependency lifecycle scripts are disabled during every install path.
- Root scripts run only after the explicit `tapid run` command.
- Full npm semver, aliases, tags, git/file/workspace specs, peer semantics, workspaces, and complete optional-dependency and lockfile compatibility are not implemented.
- `add`, `remove`, `update`, `prune`, script approval, private-registry authentication, and package publishing are outside this slice.
- JSR installation remains fail-closed unless metadata provides both an HTTPS npm tarball URL and a valid SHA-512 SRI value. Live JSR integrity behavior is unsupported and unverified.
- Linux and Windows execution is configured in CI, but must not be described as locally verified until those jobs have run. macOS local execution does not prove Windows or Linux behavior.
