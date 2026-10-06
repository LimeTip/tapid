# Windows 11 native Node acceptance

## Outcome and limits

The previously reported `Exited(-1073741502)` (`0xC0000142`) did **not** reproduce with pinned official Node v22.6.0, either the existing smoke artifact or a clean build from `abe3d86`. No production runner fix is justified by this investigation. These changes add native acceptance coverage and the requested cancellation diagnostics, not a demonstrated fix for that historical status. Windows Server was not used.

The branch was subsequently fast-forwarded to the parent's `e936be52c644dcd5142a22b945ca8b0693bdada7`; that commit changes CLI output/CI, not the runner. The CLI was rebuilt and its smoke acceptance repeated successfully at that revision.

## Target and artifacts

- Proxmox VM126, Microsoft Windows 11 Pro, `10.0.26300`, build `26300`.
- Scheduled-task principal: `tapidtest`, `Interactive`, `Limited`; target identity `TAPID-WIN11\\tapidtest`, console session `2`.
- Transfer: compressed artifacts through `qm guest exec 126`, SSH `BatchMode=yes`, `StrictHostKeyChecking=yes`; destination SHA256 verified.
- Node v22.6.0 executable SHA256: `59ceb9e78a1db169b4e05da49a4c7268c31ac994db7a4100cab76bb2897c82d5`.
- Final containment-test executable SHA256: `5b6a9f61fc8c8e933f536bbff0d2260c5ce923c7fce75051b257a4eaa98f3466`.
- Final CLI executable (includes parent e936be5) SHA256: `54f95d5e325823c62b1b79e9a2f45bceda5a5bf1a404999dfa40cd79fbab35e6`.

## Executed native checks

`containment.exe --nocapture --test-threads=1`: **19 passed, 0 failed, 1 ignored**, 191.76s, test process exit `0`, scheduled task result `0`. The ignored existing-file-write-allow test remains intentionally disabled; declared write policies and network-enabled policies still fail closed before spawn.

Real Node acceptance checks executed (runtime configured, not skipped):

- Version, project script/module resolution, and Node-created child process.
- Exact argv including space, quote, trailing backslash, and empty argument.
- Allowlisted environment value and rejection of inherited sentinel (parent set `TAPID_SECRET_NOT_ALLOWED`).
- Exact stdout `NODE_CONTRACT_OK`, stderr `NODE_STDERR_OK`, exit `23`.
- Allowed project read; denied outside read, outside write, existing-project-file overwrite, and new project file creation. Host file contents unchanged and prohibited paths absent. Windows Node reported `EPERM` for the outside-read denial; assertions accept only `EPERM`/`EACCES`.
- Node TCP connection to a positively reachable host listener denied; no accepted connection. Separate native curl/host positive-control denial test also passed.
- Node descendant emitted `NODE_DESCENDANT_ACTIVE\n`; root/descendant Job Object timeout returned `TimedOut`, `KernelOwnedComplete`, and exact project DACL restoration. Both Node processes used the unchanged 128MiB job-wide limit.
- Existing native output-limit, subprocess-denial, missing-executable cleanup, and project DACL checks passed.

Ctrl+C on the **same final test artifact** passed three times (once in the full suite, twice isolated). Isolated helper success timings: 8.1073462s, 7.8802585s, 8.0287398s. Each returned `Cancelled`, `KernelOwnedComplete`, and exact project DACL restoration; the helper owns `CREATE_NEW_CONSOLE`, and both output streams are now drained concurrently.

CLI invocation:

`tapid.exe run windows-smoke --project-dir C:\Users\tapidtest\AppData\Local\Temp\tapid-node-cli-smoke --node-runtime C:\Users\tapidtest\AppData\Local\Temp\node-v22.6.0-win-x64\node-v22.6.0-win-x64\node.exe --receipt-json`

Verified exit `0`; exact stdout `NODE_CHILD_MARKER TAPID_NODE_CLI_MARKER`; receipt `Exited(0)`, backend `tapid-runner/windows-appcontainer-job`, cleanup `KernelOwnedComplete`; PowerShell before/after project SDDL exact equality. Read-only/no-network/subprocess profile had timeout 30s, output limit 4096, process limit 8, job memory limit 134217728.

Post-run process inspection found no `node`, `tapid`, or `containment` process.

## Other checks and investigation pitfalls

- Windows GNU cross-builds (`cargo ... --locked --offline`) succeeded with the pinned Rust 1.99.0 toolchain; `cargo fmt --all -- --check` and `git diff --check` passed.
- Host macOS runner suite: 147 passed, 1 failed. The unrelated existing `project_hardlink_cannot_mutate_reserved_node_or_trusted_runtime` test failed because its copied Homebrew Node could not load `@rpath/libnode.147.dylib`. No macOS production change was made.
- A throwaway descendant fixture that called `child.unref()` exited normally instead of timing out. Increasing memory to 512MiB did not change that result; keeping the parent alive until the child exits produced the intended timeout at the original 128MiB limit. This fixture correction is not evidence of the historical Node startup failure or of a memory-limit defect.
- Initial new fixture errors (explicitly requesting a non-allowlisted environment variable, and assuming only EACCES) were corrected to test the actual contract. No security restrictions, memory limits, volume-root DACLs, or SystemRoot DACLs were broadened.

Guest logs remain under `C:\Users\tapidtest\AppData\Local\Temp\tapid-node-fix`; local probe scripts live in the Hermes scratch directory. The historical 0xC0000142 cause remains unverified and needs an exact failing artifact/invocation if it recurs.
