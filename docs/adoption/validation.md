# Implementation validation — 2026-10-28

The current implementation adds capture tooling and static qualification; it does not complete #307.

Commands used the supplied absolute Node 22.23.3 executable, with its directory explicitly prefixed to PATH:

```text
node --experimental-strip-types --test tests/evaluate_adoption_test.ts
node --experimental-strip-types --test tests/*_test.ts tools/release/*_test.ts
```

- Focused capture suite: 5 passed, 0 failed, 0 skipped. It covers explicit environments, dependency accounting, independent continuation, nonzero/launch/signal failures, deadlines, process-group descendant termination, bounded output, chunk-spanning redaction, terminal controls, digest recording and assertion integrity. A regression test first demonstrated that redaction could cause a false pass; assertions now inspect raw bounded output. Another first demonstrated ambient inheritance when an environment was omitted; such specifications now reject before launching.
- Latest full TypeScript lane: 423 tests, 403 passed, 20 skipped, 0 failed, 284,777 ms. The subsequent test-only `fileURLToPath` portability adjustment passed the focused suite; production capture code was unchanged. Native Windows behavior remains unverified here. Existing optional-tool/native-probe skips do not establish platform or pilot evidence.
- Qualification capture: five actual launches passed. Download identity matches the pinned plan. SQLite bytes matched the original npm lock's SHA-512 integrity before reading the hook manifest. These commands executed no downloaded package code.
- Reviewed all new source, tests, documentation and public qualification receipt content. Input/license identities, JSON parseability and receipt accounting were checked. Relative adoption links and listed CLI flags were reviewed against source. `git diff --check` passed (new files additionally reviewed directly).
- No Rust code or shared helper contract changed; no Cargo lane was required. The existing documentation runner is unchanged. Its `digest` export is reused. Its `boundedProcess` contract omits signal/launch receipts, so evaluation supervision remains separate rather than changing documentation behavior.

Docker CLI exists, but its daemon socket is unavailable. No disposable external-code execution environment, qualified Linux lifecycle host, native Windows run or complete macOS pilot is established. No npm baseline, migration, maintenance, application check or rollback succeeded or failed in this checkpoint: those phases have not run. Graph comparison is unqualified, not a parity pass. No product defect was reproduced, so historical Rust regression claims were not reconstructed.

Continue with the first pilot's executable run specification (bounded loopback readiness, test-service prerequisites and independent maintenance/rollback checkpoints), qualify the existing graph comparator, then execute the full journey in disposable isolation. Complete that report before selecting four further projects. Human-participant metrics remain unmeasured. No tracker publication, merge, deployment or production-support promotion occurred.
