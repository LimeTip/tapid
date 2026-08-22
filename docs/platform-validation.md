# Linux and Windows platform validation

The repository configures a `platform-consumer-validation` GitHub Actions job for `ubuntu-latest` and `windows-latest`. Each job builds the `tapid` binary, creates a Node fixture under the runner's temporary directory, and invokes the binary with the native runner shell. The workflow is configuration, not execution evidence. No CI run was available to this documentation change, so Linux and Windows remain configured but unverified here. A local macOS result cannot substitute for either platform.

## Configured validation

The workflow is intended to run these exact checks on both platforms:

```text
cargo build --bin tapid --locked
tapid install --project-dir "$TAPID_FIXTURE_PROJECT" --offline --frozen
tapid run --project-dir "$TAPID_FIXTURE_PROJECT" test -- forwarded 0
tapid run --project-dir "$TAPID_FIXTURE_PROJECT" test -- wrong 0
```

The fixture checks that:

- install creates managed `node_modules`;
- the dependency lifecycle marker remains absent;
- the root script runs in the project directory;
- arguments after `--` are forwarded;
- the child exit code is propagated;
- package executable behavior can be extended through the fixture's declared `bin` contract.

Linux uses Bash and Unix shell behavior. Windows uses PowerShell to invoke the binary and `cmd.exe` for the child script backend, with `.cmd` and PowerShell shim formats selected by the linker.

## Local evidence

Local macOS unit and integration tests can exercise resolver, archive, store, lockfile, linker planning, install replay, root-script execution, and Unix shim behavior. They do not verify Windows wrappers, Windows path handling, Windows junction activation, or Linux runtime behavior. Do not report those as passed based on macOS output.

## JSR status

No live JSR integrity result is claimed. Local fixtures cover the parser and fail-closed behavior only. Live JSR installation is unsupported until the service supplies an explicit HTTPS npm tarball URL and valid SHA-512 SRI that can be verified by a read-only smoke test.

## Required evidence for updating this document

After a successful workflow run, record the workflow URL or run identifier and platform-specific output before changing the status above. Until then, describe the checks as configured, not verified. The configured job must not be weakened to make unsupported behavior pass.
