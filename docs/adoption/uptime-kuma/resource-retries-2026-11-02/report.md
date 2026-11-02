# Uptime Kuma resource retries and interrupted-update recovery

These are follow-ups to the [first unsuccessful attempt](../attempt-2026-11-01/report.md),
not a successful Tapid application journey. The original receipt remains unchanged.
The pinned source, license, commands and retained-input hashes remain in the
[plan](../plan.json). No application source or lock selections were changed.

## Environment and method

Execution used image
`sha256:9677fef92f65e2b2149770ebd11414a46d213ab4d95ab36ede6c2135e99b6e8b`,
Linux arm64, Node 22.23.3 and npm 10.9.9. The retained Tapid binary reports
0.0.13 at revision `e445eb2a3f9031745b0e34ad91a40973c5b430e6`, SHA-256
`58e02c16d0eb7ca52226a28d13b120d9155956e1183f2b559aa1757ed810a2b4`.
It is an unreleased historical build, not a binary rebuilt from the subsequent
test/documentation changes.

The original idle pilot retained about 1.4 GiB in RAM-backed work and temporary
mounts in a VM reporting 1,959 MiB total memory. Its 6 GiB tmpfs capacity did not
provide that much physical memory. The retry used a disk-backed Docker volume
for the work directory and temporary downloads, a read-only root filesystem,
read-only retained source, an unprivileged user, all capabilities dropped,
no-new-privileges, and a private container network namespace with registry access.
No host home, credentials or Docker socket were mounted. This is not a qualified
Tapid ManagedTree lifecycle host or native macOS/Windows evidence.

The runner received an explicit environment: PATH, LANG, CI and its existing
per-checkpoint HOME/USERPROFILE/npm configuration, plus disk-backed TMPDIR and
CYPRESS_CACHE_FOLDER, `npm_config_maxsockets=2`, and the heap setting below.
Lifecycle scripts and npm's normal audit remained enabled. No audit fix was run.
Commands and bounded outputs are retained in each linked receipt. The npm checks
reuse the existing pilot specification's commands, assertions and readiness probe.
Phase deadlines were at most ten minutes for the install/test retry; startup
retains its existing one-minute deadline. The separate build retries retain the
specification's twenty-minute build deadline.

## Results

| Capture | Result | Resource observation |
| --- | --- | --- |
| [Initial disk retry](overlap-receipts.json) | npm install received SIGKILL; copied Tapid binary lacked its executable bit | `oom_kill` 0 → 1; overlapped a Rust link, so not a controlled storage-only comparison |
| [Sequential baseline](baseline-receipts.json) | `npm ci --no-fund` passed in 54,984 ms; backend suite passed all 16 tests; build exhausted its 384 MiB V8 heap; startup not reached | `oom_kill` stayed 1 |
| [1,024 MiB build](build-1024-receipts.json) | `npm run build` exited 137 after 55,448 ms; startup not reached | `oom_kill` 1 → 2 |
| [768 MiB build](build-768-receipts.json) | Build exhausted its V8 heap, exit 134 after 45,766 ms; startup not reached | `oom_kill` stayed 2 |
| [Interrupted update](recovery-receipts.json) | Tapid recovered the pending transaction; offline install then failed for missing `tapid.lock`; snapshot confirmation and original npm input restoration passed | No successful dependency update or npm rollback install is established |

Before the sequential run, download caches and incomplete npm trees from the
original npm/add checkpoints were archived to disk. All 6,167 cache files and
76,780 partial-tree files passed SHA-256 verification before removing their
RAM-backed copies. Tar archives retain their directory and symlink entries.
The interrupted update checkpoint and journal were not manually removed.
The sequential install reused its own partially warmed npm cache; its timing is
not a cold-network benchmark. The copied binary was made executable in an owned
copy; the successful identity phase verifies the same binary digest.

The successful install added 1,479 packages and audited 1,480. npm reported 115
vulnerabilities: 13 low, 46 moderate, 48 high and eight critical. This is captured
npm output, not an independently reviewed advisory report or a safety claim.
The backend command remains the limited suite described in the first report;
passing it does not establish full service or browser correctness.

Recovery first retained journal bytes with SHA-256
`32aecfea77eb6fa36974277c91fc032fbfbe74a5e8ddd1b80fafd2bb6b1fb691`.
The command was `tapid install --offline --store-dir <checkpoint>/.store`.
It reported recovery followed by `LOCKFILE_MISSING`; that command is recorded
as failed. The independent confirmation required absence of pending journals and
byte equality of the recovered manifest with the Prepared journal snapshot.
Only then did the existing restoration phase verify all retained source inputs
and restore the original npm input hashes. Raw journal bytes stay outside public
evidence. This is restoration after an interrupted failed mutation, not rollback
after a successful maintenance cycle.

## Remaining work

The resource change enabled npm installation and backend tests, but the observed
heap/VM envelope still cannot complete the production build. Provision a larger
disposable memory envelope before another build/start attempt; do not keep
repeating these settings or disable lifecycle scripts to claim success.

The [offline Rust reductions](../attempt-2026-11-01/report.md#follow-up-order-and-evidence-limits)
preserve the import/workspace and malformed historical registry metadata blockers.
Resolve or explicitly bound those contracts before claiming Tapid replay,
add/update, graph equivalence or application success. Recovery has advanced to
restored inputs; npm rollback installation and application rechecks remain open.
Four further projects, lifecycle/platform qualification, participant evidence and
linking the final synthesis into issue #158 remain outstanding. No maintainer was
contacted, no private source was used, and no release support status changed.

These captures contain 23 phases and 17 runner launches. Internal subprocesses,
HTTP calls, resource preparation and inspection commands are excluded from launch
counts. Resource preparation/acquisition duration and human assistance metrics
were not instrumented. Participant completion, comprehension and stopping reasons
remain unmeasured.
