# External adoption evaluation

This work addresses [#307](https://github.com/LimeTip/tapid/issues/307). No complete pilot journey has been evaluated in this checkout. Historical implementation reports are not current evidence. Pilot results do not change [production support](../production-adoption.md).

The opt-in `evaluate` export in `scripts/evaluate-adoption.ts` accepts a version-1 specification with explicit project/run identity, ordered phases and an evidence destination. Each phase supplies executable/arguments, working directory, explicit environment, deadline in milliseconds, byte output limit, optional required output strings, input/output digest paths, manual steps and preceding prerequisite IDs. `unsupported` records a diagnosed limitation without launching a command. Importing the module executes nothing.

Receipts are written after each phase to `receipts.json`; `report.md` provides an outcome index. Failed prerequisites suppress only dependent phases. Launch counts include failed launches, excluding internal subprocesses. Exit, signal, timeout, output overflow, assertions and file SHA-256 identities remain distinct. A successful exit without failed assertions passes only that command, not application correctness or graph parity.

The runner is **not an isolation boundary**. Execute external code only in disposable qualified environments with isolated homes/stores and no host credentials. Explicit environments prevent ambient environment inheritance, but do not restrict filesystem or network access. On Unix the runner kills its process group; deliberately detached descendants are outside this guarantee. Windows root-only cleanup is insufficient for unattended external evaluation. No hook authority is granted by captured identity or output.

Output capture stops at the configured raw byte cap and terminates the process on overflow. Sanitization redacts caller-supplied sensitive strings across chunks, including truncated suffixes, and escapes terminal controls. Sanitized output may be larger than the raw byte cap. Commands, paths and metadata are sanitized too. No raw log is written by the runner. This is not exhaustive secret discovery: keep destinations outside tracked files until publication review. Receipts are incremental, not a crash-safe transaction journal.

Run offline regression tests:

```text
node --experimental-strip-types --test tests/evaluate_adoption_test.ts
```

See [selection and qualification](selection.md). Remaining work: first baseline/migration/replay/application/maintenance/rollback journey, comparator qualification, four subsequent projects, qualified Linux lifecycle and native macOS/Windows evidence, defect reductions and corpus synthesis. Participant metrics remain unmeasured; no participant contact or private access is authorized.
