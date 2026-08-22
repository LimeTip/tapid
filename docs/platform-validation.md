# Linux and Windows platform validation

The `platform-consumer-validation` GitHub Actions job runs only on `ubuntu-latest` and `windows-latest`. It builds the `tapid` binary, creates a fresh Node.js project under the runner's temporary directory, writes a lockfile whose manifest digest is calculated from that project, and invokes the built binary with the platform-native shell (`bash` on Linux and PowerShell on Windows). No macOS result is implied by this job.

## Validated today

- `tapid install --offline --frozen` accepts a dynamic project directory and creates managed `node_modules`.
- The fixture's `preinstall` script would create `LIFECYCLE_SHOULD_NOT_RUN`; the file must remain absent, proving lifecycle suppression for this install path.
- The same binary is probed for the `run` command on both platforms. The workflow requires the current stable failure (`unrecognized subcommand`) rather than treating an unsupported command as a passing script test.

## Exact prerequisite for the remaining consumer checks

The current `tapid-cli` binary does not expose a root-script execution subcommand. `crates/tapid-cli/src/run.rs` contains the process-execution contract and unit tests, but `crates/tapid-cli/src/main.rs` has no `run`/`exec` command wired into `Cli`/`dispatch`. Therefore the built binary cannot currently exercise, on either Linux or Windows:

1. root `scripts.test` execution,
2. executable `node_modules/.bin` shims from a consumer install,
3. argument forwarding through the CLI boundary, or
4. child exit-code propagation.

The required follow-up is to add a user-facing CLI command that constructs `RunRequest` from a project directory, selected root script, and trailing arguments, then returns the child status as the CLI exit code. It must use `ShellBackend::default_for_platform()` (Unix `/bin/sh`; Windows `cmd.exe`) and preserve the existing managed `.bin` `PATH` behavior. After that source change, replace the expected `unrecognized subcommand` probe with assertions that run the generated root script, invoke its `.bin` command with forwarded arguments, and verify a deliberate non-zero child status. That source work is intentionally outside this platform-validation change.

The fixture metadata includes the complete intended check set (`install`, `root-script`, `bin-shim`, `argument-forwarding`, `exit-code`, and `lifecycle-suppression`) so the follow-up can extend the existing dynamic project without developer-specific paths. macOS is deliberately not included in, or claimed by, these Linux/Windows checks.
