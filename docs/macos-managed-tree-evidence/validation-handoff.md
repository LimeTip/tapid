# Validation handoff after base sync

Read-only inspection on 2026-11-01 examined HEAD
`1230b80` against `origin/main`. The combined feature adds only the disposable
runner example, its private research fixtures, evidence and platform documentation.
Production execution interfaces and macOS strict rejection are unchanged by this
feature diff.

The separate-client-run evidence index still verifies all 42 retained artifact
hashes. Of its 562 recorded source files, 41 differ after the base sync, including
Cargo.lock, integration Cargo.lock and production consumers. None of the recorded
research example sources differ. Historical results remain development evidence;
they do not validate the combined checkout. No repository tests were executed on
the host in this handoff.

Docker now responds and reports Linux/aarch64. The exact configured image check:

```text
docker image inspect sha256:9f1c2c4244625133b85bef1c78f3f3cff7a4aea3a136c70cfe8e5877973b3b73 --format '{{json .}}'
```

returns exit 1, `No such image`. This supersedes the earlier missing-daemon
diagnostic. The engine must provision and verify its pinned worker image using
its prescribed `pnpm run setup`, then run the configured isolated validation
command against the exact source snapshot. This repository has no root
package.json and does not own that engine setup command. No replacement image,
host test fallback or validation receipt was created.

The isolated Linux lane cannot establish native macOS containment. Capable
macOS sandbox/XPC execution and macOS 26 matrix evidence remain required for
platform claims. Historical setup failures are unresolved, not native support
or impossibility verdicts. Independent QA and Acceptance remain outstanding.

`git diff --check origin/main...HEAD` reports whitespace in retained raw logs.
Those bytes were preserved so their recorded digests remain verifiable. This is
not a passing diff check; the reported paths are research evidence, not source.

Next: restore the pinned engine image, obtain current-source portable validation,
and run the documented native positive/adversarial matrix in the isolated macOS
test environment. Keep the strict gate and the distinction between diagnostic
observations and enforcement guarantees throughout.
