# Windows containment fixture inheritance verification

Base: `7ca56883ac570df40f2afced67c7ad0f75e34288`.

## Hosted failure audit

`gh run view 37570000621 --log-failed` reports containment **14 passed, 5 failed, 1 ignored**. All five failures are final project DACL comparisons:

- combined output limit;
- missing executable;
- ordinary ManagedTree execution;
- Ctrl+C helper cleanup (the parent failure replays the same helper failure);
- timeout cleanup.

Decoded all six printed left/right byte-array pairs, including the repeated Ctrl+C pair. Each differs only by addition of `(I)` to the SYSTEM, Administrators, and runneradmin `(OI)(CI)(F)` ACEs. No identity, access mask, order, path, or other output changes were present. All assertions preceding cleanup passed; the cancellation helper returned `Cancelled` with `KernelOwnedComplete`. This audit does not imply that later workspace test targets ran after Cargo stopped at containment.

## Change boundary

The shared test-only helper reapplies the existing DACL on a newly created disposable root before the baseline. Setup asserts unchanged ACL length and all bytes except a one-way addition of `INHERITED_ACE`, and allows no descriptor control change except setting `SE_DACL_AUTO_INHERITED`. The library regression retains its stronger unchanged root/descendant ACE-byte setup checks and exact ancestor/sibling/grant cleanup comparisons. Integration snapshots retain complete `icacls` output equality and additionally compare raw DACL bytes and descriptor control bits exactly. No production grant logic, policy gates, shared ancestor DACLs, or security limits changed.

Production legacy-tree conversion is a documented metadata-restoration limitation, not a claim of byte-identical restoration for arbitrary ACL trees.

## Native verification

Proxmox VM126: Windows 11 Pro `10.0.26300`, Interactive/Limited `tapid-win11\tapidtest`, medium integrity, administrator groups deny-only. Node `v22.6.0` configured explicitly from the existing official standalone distribution.

Final containment executable SHA256, matched on host and guest:
`57bf16a37c950fd5f5ed2c8a2fe729b57d370213c63ea6d92d399781f0379d7a`.

`containment.exe --nocapture --test-threads=1`: **19 passed, 0 failed, 1 ignored**, 12.42s; process exit and scheduled-task result both `0`. The existing-file write positive test remains intentionally ignored. Real Node argv/environment/read-only boundaries, direct runtime, script-file resolution, child launch and descendant timeout tests ran. Ctrl+C returned `Cancelled`, `KernelOwnedComplete`, and exact restoration. No containment/library/Node process remained after completion.

Library executable SHA256, matched on host and guest:
`5aba83e83a322afb766aa46fbe075159b3778fb532538fa9a70d99ebff1fa9fa`.

`library.exe declared_grant_leaves_ancestor_and_sibling_dacls_unchanged --nocapture --test-threads=1`: **1 passed, 0 failed**, 116 filtered, 0.04s; process exit and scheduled-task result both `0`.

## Build gates

Rust 1.99.0:

- `cargo test -p tapid-runner --test windows_containment --lib --target x86_64-pc-windows-gnu --locked --offline --no-run` passed with the MinGW linker.
- `cargo clippy -p tapid-runner --all-targets --all-features --target x86_64-pc-windows-gnu --locked --offline -- -D warnings` passed.
- `cargo fmt --all -- --check` and `git diff --check` passed.

Raw evidence: local Hermes scratch `containment-hosted-failures.log`, `containment-fixture-final-native.log`, `containment-fixture-library-native.log`; guest `C:\Users\tapidtest\AppData\Local\Temp\tapid-containment-fixture-fix`.

No push or hosted rerun was performed. These are Windows 11 native containment results, not full CLI/consumer support acceptance.
