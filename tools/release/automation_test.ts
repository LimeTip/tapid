import { strictEqual, throws, rejects } from "node:assert/strict";
import { test } from "node:test";
import { checkApprovedPlan, evaluateCi, waitForCi } from "./automation.ts";

const sha = "a".repeat(40);
const run = (overrides: Record<string, unknown> = {}) => ({ id: 10, run_number: 2, head_sha: sha, head_branch: "main", event: "push", status: "completed", conclusion: "success", ...overrides });
const codeqlNames = ['Analyze (actions)', 'Analyze (rust)', 'Analyze (javascript-typescript)', 'Analyze (python)'];
const codeqlChecks = () => codeqlNames.map((name, index) => ({ id: index + 1, name, head_sha: sha, app: { id: 15368 }, status: 'completed', conclusion: 'success' }));

test('exact main CI owns command help without an obsolete standalone check', async () => {
  const { requiredChecks } = await import('./automation.ts');
  // Spell out the retained checks independently of the production list.
  strictEqual(JSON.stringify(requiredChecks), JSON.stringify(codeqlNames));
  await waitForCi({ sha, readRuns: async () => [run()], readChecks: async () => codeqlChecks() });
});

test('migrated command-help gate still blocks non-success CI and every missing or failed CodeQL scope', async () => {
  for (const conclusion of ['failure', 'cancelled']) {
    await rejects(waitForCi({ sha, readRuns: async () => [run({ conclusion })], readChecks: async () => codeqlChecks() }), /CI did not succeed/);
  }
  for (const [index, name] of codeqlNames.entries()) {
    let elapsed = 0;
    await rejects(waitForCi({ sha, readRuns: async () => [run()], readChecks: async () => codeqlChecks().filter((_, i) => i !== index), now: () => elapsed, sleep: async ms => { elapsed += ms; }, timeoutMs: 10 }), /timed out/);
    const escapedName = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    await rejects(waitForCi({ sha, readRuns: async () => [run()], readChecks: async () => codeqlChecks().map((check, i) => i === index ? { ...check, conclusion: 'failure' } : check) }), new RegExp(`required check did not succeed.*${escapedName}`));
  }
  for (const overrides of [{ head_sha: 'b'.repeat(40) }, { event: 'pull_request' }, { head_branch: 'release/prepare' }]) {
    let elapsed = 0;
    await rejects(waitForCi({ sha, readRuns: async () => [run(overrides)], readChecks: async () => codeqlChecks(), now: () => elapsed, sleep: async ms => { elapsed += ms; }, timeoutMs: 10 }), /timed out/);
  }
});

const packages = [{ name: "tapid-core", version: "0.0.7" }, { name: "tapid", version: "0.0.12" }];
const intent = { schema: "tapid-release-intent-v1", version: "0.0.12", baseline: "v0.0.11", prepared_from: "b".repeat(40), notes: "docs/releases/0.0.12.md", packages };
const metadata = { packages };

test("CI accepts only the exact main push and waits for missing or running evidence", () => {
  strictEqual(evaluateCi([], sha), "waiting");
  strictEqual(evaluateCi([run()], sha), "success");
  strictEqual(evaluateCi([run({ event: "pull_request" }), run({ head_sha: "b".repeat(40) }), run({ head_branch: "release/prepare" })], sha), "waiting");
  strictEqual(evaluateCi([run({ status: "in_progress", conclusion: null })], sha), "waiting");
  throws(() => evaluateCi([], "bad"), /commit/);
});

test("latest exact CI attempt blocks stale success and rejects every completed non-success", () => {
  strictEqual(evaluateCi([run({ id: 9, run_number: 1 }), run({ status: "queued", conclusion: null })], sha), "waiting");
  for (const conclusion of ["failure", "cancelled", "timed_out", "skipped", "neutral", "action_required", null]) {
    throws(() => evaluateCi([run({ conclusion })], sha), /CI did not succeed/);
  }
  throws(() => evaluateCi([run({ status: "unexpected" })], sha), /status/);
});

test("CI poll waits, then succeeds without requiring external services", async () => {
  let requests = 0, elapsed = 0;
  await waitForCi({ sha, readRuns: async () => ++requests === 1 ? [] : [run()], now: () => elapsed, sleep: async (ms) => { elapsed += ms; }, pollIntervalMs: 10, timeoutMs: 20 });
  strictEqual(requests, 2);
  strictEqual(elapsed, 10);
});

test("CI polling stops on failure, transport error, and bounded timeout", async () => {
  await rejects(waitForCi({ sha, readRuns: async () => [run({ conclusion: "failure" })] }), /CI did not succeed/);
  await rejects(waitForCi({ sha, readRuns: async () => { throw Error("API unavailable"); } }), /API unavailable/);
  let elapsed = 0;
  await rejects(waitForCi({ sha, readRuns: async () => [], now: () => elapsed, sleep: async (ms) => { elapsed += ms; }, pollIntervalMs: 10, timeoutMs: 20 }), /timed out/);
  strictEqual(elapsed, 20);
});

test("pending publication must be the full approved order or its remaining suffix", () => {
  checkApprovedPlan(intent, { packages, blockers: [] }, metadata);
  checkApprovedPlan(intent, { packages: packages.slice(1), blockers: [] }, metadata);
  checkApprovedPlan(intent, { packages: [], blockers: [] }, metadata);
  for (const pending of [packages.slice(0, 1), [...packages].reverse(), [{ name: "extra", version: "0.0.1" }], [{ name: "tapid", version: "0.0.13" }], [packages[1], packages[1]]]) {
    throws(() => checkApprovedPlan(intent, { packages: pending, blockers: [] }, metadata), /approved suffix/);
  }
});

test("every approved package remains bound to source metadata after partial publication", () => {
  throws(() => checkApprovedPlan(intent, { packages: packages.slice(1), blockers: [] }, { packages: packages.slice(1) }), /source metadata/);
  throws(() => checkApprovedPlan(intent, { packages: packages.slice(1), blockers: [] }, { packages: [{ ...packages[0], version: "0.0.8" }, packages[1]] }), /source metadata/);
  throws(() => checkApprovedPlan(intent, { packages, blockers: [] }, { packages: [...packages, packages[0]] }), /source metadata/);
  throws(() => checkApprovedPlan(intent, { packages, blockers: ["unavailable registry"] }, metadata), /blockers/);
  throws(() => checkApprovedPlan(intent, { packages }, metadata), /blockers/);
  throws(() => checkApprovedPlan({ ...intent, version: "0.0.13" }, { packages, blockers: [] }, metadata));
});

test('exact-source CodeQL checks reject foreign, stale and failed evidence', async () => {
  const { evaluateChecks, requiredChecks } = await import('./automation.ts');
  const checks = requiredChecks.map((name, index) => ({ id: index + 1, name, head_sha: sha, check_suite: { head_branch: 'main' }, app: { id: 15368 }, status: 'completed', conclusion: 'success' }));
  strictEqual(evaluateChecks(checks, sha), 'success');
  strictEqual(evaluateChecks(checks.slice(1), sha), 'waiting');
  strictEqual(evaluateChecks(checks.map(c => ({ ...c, app: { id: 9 } })), sha), 'waiting');
  strictEqual(evaluateChecks(checks.map(c => ({ ...c, head_sha: 'c'.repeat(40) })), sha), 'waiting');
  strictEqual(evaluateChecks([...checks, { ...checks[0], id: 100, status: 'queued', conclusion: null }], sha), 'waiting');
  throws(() => evaluateChecks([...checks, { ...checks[0], id: 100, conclusion: 'failure' }], sha), /did not succeed/);
  let elapsed = 0;
  await rejects(waitForCi({ sha, readRuns: async () => [run()], readChecks: async () => [], now: () => elapsed, sleep: async ms => { elapsed += ms; }, timeoutMs: 10 }), /timed out/);
});
