# External adoption evaluation

This work addresses [#307](https://github.com/LimeTip/tapid/issues/307). The first pilot has a [completed capture of an unsuccessful attempt](uptime-kuma/attempt-2026-11-01/report.md); no successful application journey is established. Historical implementation reports are not current evidence. Pilot results do not change [production support](../production-adoption.md).

The opt-in `evaluate` export in `scripts/evaluate-adoption.ts` accepts a version-1 specification with explicit project/run identity, ordered phases and an evidence destination. Each phase supplies executable/arguments, working directory, explicit environment, deadline in milliseconds, byte output limit, optional required output strings, input/output digest paths, manual steps and preceding prerequisite IDs. `unsupported` records a diagnosed limitation without launching a command. Importing the module executes nothing.

Receipts are written after each phase to `receipts.json`; `report.md` provides an outcome index. Failed prerequisites suppress only dependent phases. Launch counts include failed launches, excluding internal subprocesses. Exit, signal, timeout, output overflow, assertions and file SHA-256 identities remain distinct. A successful exit without failed assertions passes only that command, not application correctness or graph parity.

The runner is **not an isolation boundary**. Execute external code only in disposable qualified environments with isolated homes/stores and no host credentials. Explicit environments prevent ambient environment inheritance, but do not restrict filesystem or network access. On Unix the runner kills its process group; deliberately detached descendants are outside this guarantee. Windows root-only cleanup is insufficient for unattended external evaluation. No hook authority is granted by captured identity or output.

Output capture stops at the configured raw byte cap and terminates the process on overflow. Sanitization redacts caller-supplied sensitive strings across chunks, including truncated suffixes, and escapes terminal controls. Sanitized output may be larger than the raw byte cap. Commands, paths and metadata are sanitized too. No raw log is written by the runner. This is not exhaustive secret discovery: keep destinations outside tracked files until publication review. Receipts are incremental, not a crash-safe transaction journal.

Run offline regression tests:

```text
node --experimental-strip-types --test tests/evaluate_adoption_test.ts
```

See [selection and qualification](selection.md). Remaining work: first baseline/migration/replay/application/maintenance/rollback journey, comparator qualification, four subsequent projects, qualified Linux lifecycle and native macOS/Windows evidence, defect reductions and corpus synthesis. Participant metrics remain unmeasured; no participant contact or private access is authorized.

The first pilot now has an executable `uptimeKumaRun` specification in
[`scripts/evaluate-uptime-kuma.ts`](../../scripts/evaluate-uptime-kuma.ts).
It prepares five independent copies, checks retained manifest/lock/license hashes,
records tool identities, and attempts npm baseline, status, import, frozen/offline
replay, native install, controlled add/update, application checks and npm recovery.
A failed import does not suppress independent native maintenance attempts or npm
restoration. Successful recovery after a failed mutation must be reported separately
from rollback after a successful mutation. Pending lifecycle journals stop restoration;
recover through Tapid before retrying, without deleting the journal.
Restoration verifies all retained input bytes before deleting installation state or
overwriting checkpoint inputs. Missing or changed retained inputs leave that checkpoint
intact. This preflight is not an atomic recovery guarantee for later filesystem failures.

Before using it, qualify a disposable environment with no host credentials and a
private network namespace, verify/extract the pinned source archive, mount the
retained source read-only, and provide source-matched tools. The specification checks
three retained input files; it does not authenticate the rest of an arbitrary extracted
tree or qualify isolation. Reserve loopback port 3001 inside that environment. The
current supervisor does not support unattended Windows evaluation. Retain a separate
host/image/prerequisite receipt. The lifecycle phase remains explicitly unsupported
until a qualified Linux host and reviewed exact authority policy are provided.

Supply a JSON configuration outside public evidence with absolute `source`, `root`
(a new nonexistent disposable directory), `node`, `npmCli` (npm's JS entrypoint),
`tapid`, full `tapidRevision`, and an explicit `env` allowlist. Include a PATH with the
specified Node and required native tools; do not copy the host environment. Then,
**inside that qualified environment**, invoke:

```sh
node --experimental-strip-types --input-type=module -e '
import { readFileSync } from "node:fs";
import { evaluate } from "./scripts/evaluate-adoption.ts";
import { uptimeKumaRun } from "./scripts/evaluate-uptime-kuma.ts";
await evaluate(uptimeKumaRun(JSON.parse(readFileSync(process.argv[1], "utf8"))));
' /isolated/pilot-config.json
```

Receipts go to the sibling `<root>-evidence` directory. Review them before publication.
The [first attempt](uptime-kuma/attempt-2026-11-01/report.md) reached the end of this
specification, including explicit unsupported and prerequisite-blocked phases.
Reaching the end of capture does **not** establish a successful external journey.

Startup uses a fresh per-check data directory, binds `127.0.0.1:3001`, requires the
application's listening message plus HTTP 200 containing its HTML title, and terminates
the process group on readiness or after 60 seconds. `readiness` assertions are also
available to other capture phases; they require literal loopback HTTP, do not follow
redirects, and bound response bytes by the phase output limit. A clean exit without
readiness remains a failure. Probe requests are internal and excluded from command
launch counts. A readiness pass records intentional termination rather than a normal
application exit. It establishes startup only, not browser or end-to-end correctness.

The selected `jest-backend` command runs `test/backend.spec.js` directly. The inspected
suite exercises certificate parsing, secret generation, password-reset helpers,
notification payloads, string filtering and client-IP handling. It is not the full
monitor/service or browser suite. The specification does not invoke `npm test`'s
preparation script, Cypress, database-service helpers, deployment or release scripts.
Native compiler/download prerequisites remain to be measured in the disposable run.

The existing graph comparator now returns code 2 and explicit `inconclusive` reasons
for multiple equal-version instances, declared workspace/link topology and imported
schema 8 placement records. It preserves the existing code 0/1 comparison for its
unambiguous news-site representation. Code 0 describes only those reported dimensions;
it does not certify application behavior, lifecycle execution or general instance
parity. This deliberately prevents overclaiming while instance-level qualification
remains unfinished. No second graph model or resolver exception was added.
