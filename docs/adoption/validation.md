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

## Recovery continuation — executable preparation

The Docker daemon check was repeated in this run. It still targets
`unix:///Users/doug/.colima/default/docker.sock`, which does not exist. No replacement
remote execution tool or configured isolated validation command is available. The
Colima executable alone does not establish a disposable host; no service was started
and no external npm/application code ran on this host.

New runtime work adds bounded loopback HTTP readiness to the capture interface and
an executable Uptime Kuma command specification. The existing comparator now rejects
ambiguous name/version aggregation as inconclusive instead of reporting parity.
A focused test first reproduced the false parity result (actual code 0, expected 2);
it passes after the change. Workspace/imported placement evidence receives the same
explicit qualification limit. These are tooling defects/limits, not reproduced Rust
package-manager defects.

An offline rehearsal against the already qualified retained inputs executed only
four local helper phases: checkpoint copying, add restoration, deliberate update
preparation, and update restoration. All four passed; the original Day.js declaration
was restored. A separate helper invocation with a pending lifecycle journal failed
and preserved the journal. Temporary copies were removed. No npm, Tapid or application
code was launched; this is helper validation, **not an external rollback result**.

The executable specification still needs a full disposable run, binary identity,
actual native prerequisite receipts, complete inspection/lifecycle evidence and a
reviewed journey report before selecting the remaining four projects. Imported graph
comparison remains inconclusive, not qualified parity. Native macOS/Windows and
qualified Linux lifecycle evidence and participant research are still absent.

Environment diagnosis was narrowed further: `colima status` fails while attempting
to create its required `/Users/doug/.colima/_lima` directory (`operation not permitted`).
That location is outside this workspace's writable roots. No daemon startup or
unsandboxed fallback was attempted. An independent HTTP HEAD request to the selected
public is-odd metadata endpoint returned 200, so registry connectivity is available;
it is not the blocking condition. This diagnostic request is separate from the
previous qualification download counts and did not execute package code.

Final source validation (Node 22.23.3, its directory explicitly prefixed to PATH):

```text
node --experimental-strip-types --test tests/evaluate_adoption_test.ts tests/news_site_fixture_test.ts
node --experimental-strip-types --test tests/*_test.ts tools/release/*_test.ts
```

- Focused capture/comparator suite: 21 tests, 20 passed, 1 skipped, 0 failed.
  The skip is the existing source-built native news-site probe; no probe binary was supplied.
- Final full TypeScript lane: 428 tests, 408 passed, 20 skipped, 0 failed;
  elapsed 117,160 ms. This run followed the final production and test changes.
- Repeated the four-phase offline preparation/restoration rehearsal after the final
  specification changes; all four passed and the pending-journal guard remained intact.
- Reviewed the entire pending diff and new specification, CLI argument ownership,
  backend-suite scope, documentation links/commands, and `git diff --check` (passed).
  No Rust source, dependency manifest, audit policy or shared documentation process
  helper changed, so no Cargo validation lane was required for these changes.

There are still **zero completed external journeys** in this checkout. These local
checks do not supply npm baselines, actual maintenance/rollback outcomes, platform
qualification or participant evidence. The isolated validation handoff is unnecessary
for these passing local tests; no configured disposable pilot command/worker receipt
was supplied. Execution isolation remains the blocker to the first external journey.
