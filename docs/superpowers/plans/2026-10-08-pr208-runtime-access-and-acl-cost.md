# PR208: immutable runtime access and large-project DACL cost

Addresses review `4203184565` (runtime `WRITE_DAC`) and measures review `4203184557` (large-project ACL traversal). Based on `41034109ed122f64102a0fbb9a2811a47c22d0fa`; isolated branch `fix/pr208-effective-runtime-access`. No push, review-thread resolution, merge, or hosted CI run was performed.

## Runtime change

For backend-runtime, non-write grants **outside the existing SystemRoot exception**, first inspect host DACL authority. A specific `ERROR_ACCESS_DENIED` opening the target for `READ_CONTROL | WRITE_DAC` can now take a read-only verification path instead of unconditionally failing. Other open errors remain errors. Project grants and runtime writes cannot use this alternative. Any actual DACL grant/update failure still propagates; it is never converted into permission to execute.

The verification creates a **never-resumed**, handle-noninheriting, suspended `cmd.exe` AppContainer process using the existing launcher and the same execution's AppContainer SID/no-capability security attributes. The launcher verifies its actual AppContainer token. A duplicate of that actual token is used for thread-local impersonation and real kernel `CreateFileW` opens with the requested access mask (read/execute for a runtime `Read` grant). This evaluates deny ACEs, traditional and AppContainer principals, traversal, and integrity policy rather than guessing from allow-ACE presence or using the host token. Directory-subtree verification checks every existing descendant under that token; metadata/enumeration/open errors and reparse points fail closed. An already-impersonating thread is rejected rather than losing its original identity; failure to revert terminates the process rather than continuing in an unintended context.

No file is executed by the verifier. Its suspended process owner terminates/reaps it on scope exit. Verification pins the same mandatory host environment fields as real launches. This adds a small suspended-process setup cost only when the host lacks DACL authority; it is not an optimization of the normal project-grant path. This verification does not claim to eliminate filesystem races or confer rights: real launch and subsequent kernel access remain under the same AppContainer authority.

Root, SystemRoot, shared ancestors, and sibling DACLs are not broadened. The existing SystemRoot treatment and containment/Job Object lifecycle are unchanged.

## TDD and native acceptance

Target: Proxmox **VM126, Microsoft Windows 11 Pro 10.0.26300/build 26300**, not Windows Server. Tests ran as scheduled-task principal `tapidtest`, `Interactive`, `Limited`; `whoami /all` confirms medium integrity and Administrators deny-only. Cross-built with the matching Rust **1.99.0** GNU Windows toolchain; transferred compressed artifacts and compared guest/source SHA256.

`runtime_preexisting_access_does_not_require_write_dac` was written and run before production edits. Native RED reached its intended assertion: preexisting read/execute was rejected solely because DACL modification was unavailable (`red3.txt`, exit 101). The fixture removes inherited rights, explicitly denies `WRITE_DAC`, suppresses implicit owner DACL authority with an OWNER RIGHTS ACE, and grants Everyone/ALL APPLICATION PACKAGES read/execute on a **disposable copied executable**, never a shared runtime installation.

Final native acceptance:

- Library: **122 passed, 0 failed, 2 ignored**, exit 0. One ignored diagnostic was preexisting; the other is the explicit large-project measurement, run separately.
- Runtime positive: host cannot open for `WRITE_DAC`; verified AppContainer existing read/execute is accepted; the copied executable actually launches under the AppContainer/Job and exits 23; exact runtime DACL/control bytes are unchanged.
- Runtime negative: a read/execute allow ACE plus an explicit ALL APPLICATION PACKAGES execute-deny is rejected by the kernel with error 5, at the actual access-check seam. A denied outcome cannot pass merely because token setup failed.
- Both runtime cases verify unrelated host-only file access stays denied, project-policy grant failure stays fatal, and ancestor/SystemRoot/volume-root DACL/control bytes stay unchanged.
- Integration: **19 passed, 0 failed, 1 preexisting ignored**, exit 0. Both Node prerequisite variables were configured: the final run has no conditional Node-prerequisite skip. Real Node argv/environment/read-only boundaries, subprocess launch, descendant timeout, output limit, network denial, cancellation and restoration checks passed.
- Windows-target runner/all-targets Clippy with `-D warnings`, locked offline cross-builds, formatting and diff checks passed. Existing manifest license/license-file warnings are unchanged.

Library artifact SHA256: `67583074443bb5a77711861c854760dd4d4247bfae15e767e4f2d715c21851cc`.

Integration artifact SHA256: `6680fd110f8163647feda0febc292da2b3076be1416c65c0c386ddf648670d5b`.

Official Node v22.6.0 SHA256: `59ceb9e78a1db169b4e05da49a4c7268c31ac994db7a4100cab76bb2897c82d5`.

## Representative project measurement

The opt-in native test `representative_node_modules_acl_measurement` creates a disposable synthetic project containing **500 package directories, 500 lib directories and 25,000 module files**, plus project/node_modules roots and audit boundaries: **26,004 audited objects**. This is representative node_modules-scale object count/shape, not a measured npm installation or a production latency SLA. Files are small because this operation changes security metadata, not file contents. It does not model antivirus variation, concurrent writers, protected/custom descendant ACLs, or cross-process contention.

Only the newly created fixture root is initialized to the current Windows inheritance model before taking full `(control bits, raw ACL bytes)` baselines. Every audited target/descendant is compared exactly after each grant/restore cycle; no masking or post-run normalization is used. Ancestor and sibling bytes are also checked while the grant is active. Snapshots are compared in memory, not retained as a per-object byte dump; the committed raw receipt preserves the executed test and results.

The test uses the same production `grant_inner`/`restore_unlocked` operations and the real process/global named-mutex guard to expose critical-section durations without modifying production instrumentation. Grant/restore totals include mutex acquisition/release; hold timings cover the work after acquisition and before guard release, excluding release syscall overhead. Fixture creation, baseline collection and exact after-ACL audit are outside those timing windows. The mutex is released between preparation and cleanup, not held for child execution.

Final-artifact measurements, milliseconds:

| Cycle | Grant total | Grant critical section | Restore total | Restore critical section | Grant lock wait | Restore lock wait |
|---|---:|---:|---:|---:|---:|---:|
| 0 | 3689.977 | 3689.876 | 3524.319 | 3524.280 | 0.068 | 0.032 |
| 1 | 3411.706 | 3411.687 | 3365.496 | 3365.456 | 0.011 | 0.030 |
| 2 | 3425.249 | 3425.228 | 3398.656 | 3398.617 | 0.012 | 0.032 |

Native test: **1 passed, 0 failed**, exit 0; entire fixture/test/audit/removal run 53.27s. Each cycle restores exact ACL bytes/control bits across all 26,004 objects. The measured cost is seconds for **each** subtree ACL propagation, with the cross-process mutex occupied for essentially the whole propagation. This is a real startup/cleanup cost, not the former implicit shared-ancestor traversal. No speculative caching, skipped restoration, weakened audits, or ACL broadening was introduced to hide it. These uncontended measurements do not predict another execution's wait under load.

Run deliberately on native Windows, outside ordinary test runs:

```text
runner.exe representative_node_modules_acl_measurement --ignored --nocapture --test-threads=1
```

## Evidence and remaining gates

Raw native logs and exact-source/artifact digest receipt are committed under `docs/superpowers/evidence/pr208-acl/`. The identity file is an explicitly decoded copy of the raw UTF-16 `whoami /all` log; its original raw digest is retained. Final artifacts are bound to the Windows-source digest map, not merely the clean base SHA. After execution, native inspection found no `runner`, `containment`, or `node` processes and no remaining worker-owned scheduled tasks. Disposable measurement/test trees were removed by their passing tests.

Harness issues encountered and recovered: PowerShell/cmd quotation, the distinction between icacls `WD` (write data) and `WDAC` (DACL modification), mandatory LOCALAPPDATA in AppContainer creation, and file-sharing errors when polling live redirected logs. None was treated as a product RED or hidden behavioral pass.

Hosted CI is a separate, unexecuted gate for this local-only slice. The parent's upstream-integration/cancellation/test-fixture work is separate. Existing Windows write-policy acceptance limitations and the ignored integration write test remain unchanged.
