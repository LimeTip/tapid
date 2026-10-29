import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { PublicTransport } from './public_transport.ts';
import { verifyPublicSmoke } from './public_smoke.ts';

const commit = 'b'.repeat(40);
const identity = { tag: 'v0.0.11', commit, releaseId: 42, publishedAt: '2026-10-19T00:00:00Z' };
const names = ['resolve', 'Unix installer (ubuntu-latest)', 'Unix installer (macos-latest)', 'Windows installer'];
async function fixture(options: { historicalCoordinator?: boolean; history?: boolean; path?: string; omitAttempt?: boolean; jobHead?: string; jobRun?: number; coordinator?: boolean; event?: string; head?: string; missing?: string; changedWorkflow?: boolean; jobAttempt?: number; jobConclusion?: string } = {}) {
  const workflow = options.coordinator ? 'crates-publication.yml' : 'release-public-smoke.yml';
  const head = options.head ?? commit;
  const run = { id: 77, run_attempt: 2, event: options.event ?? (options.coordinator ? 'push' : 'release'), head_branch: options.coordinator ? 'main' : identity.tag, head_sha: head, path: options.path ?? `.github/workflows/${workflow}`, status: 'completed', conclusion: 'success' };
  const transport = new PublicTransport({ fetch: async input => {
    const url = new URL(String(input));
    if (url.hostname === 'raw.githubusercontent.com') {
      const file = url.pathname.split('/').slice(4).join('/');
      if (file === 'docs/releases/intent.json') return Response.json({ schema: 'tapid-release-intent-v1', version: '0.0.11', baseline: 'v0.0.10', prepared_from: 'c'.repeat(40), notes: 'docs/releases/0.0.11.md', packages: [{ name: 'tapid', version: '0.0.11' }] });
      if (options.historicalCoordinator && file === '.github/workflows/crates-publication.yml') return new Response(await readFile(new URL('./fixtures/crates-publication-before-ledger.yml', import.meta.url)));
      return new Response(options.changedWorkflow ? 'unreviewed workflow' : await readFile(new URL(`../../${file}`, import.meta.url)));
    }
    if (url.pathname.endsWith('/runs')) assert.equal(url.searchParams.get('head_sha'), commit);
    if (url.pathname.endsWith('/runs')) {
      const selected = url.searchParams.get('head_sha') === commit;
      const unrelated = Array.from({ length: 100 }, (_, index) => ({ ...run, id: 100 + index, head_sha: 'c'.repeat(40) }));
      const runs = options.history && (!selected || url.searchParams.get('page') === '1') ? unrelated : [run];
      return Response.json({ workflow_runs: url.pathname.includes(workflow) ? runs : [] });
    }
    if (url.pathname.endsWith('/jobs')) assert.equal(url.pathname, '/repos/LimeTip/tapid/actions/runs/77/attempts/2/jobs');
    if (url.pathname.endsWith('/jobs')) return Response.json({ jobs: names.filter(name => name !== options.missing).map((name, index) => ({ id: index + 1, run_id: options.jobRun ?? 77, ...(options.omitAttempt ? {} : { run_attempt: index === 3 ? options.jobAttempt ?? 2 : 2 }), head_sha: options.jobHead ?? head, name: options.coordinator ? `smoke / ${name}` : name, status: 'completed', conclusion: index === 3 ? options.jobConclusion ?? 'success' : 'success', started_at: '2026-10-20T00:00:00Z' })) });
    throw Error(`unexpected fixture request ${url.pathname}`);
  } });
  return transport;
}

test('binds release-event smoke to tag, commit, reviewed workflow, and one attempt', async () => {
  const evidence = await verifyPublicSmoke(await fixture(), identity);
  assert.equal(evidence.runId, 77);
  assert.equal(evidence.attempt, 2);
  assert.equal(evidence.contract, 'release-event');
  assert.equal(evidence.jobs.length, 4);
});

test('proves push coordinator selection using reviewed wiring and commit-addressed intent', async () => {
  const evidence = await verifyPublicSmoke(await fixture({ coordinator: true }), identity);
  assert.equal(evidence.contract, 'coordinator-push');
  assert.equal(evidence.workflowCommit, commit);
  assert.equal(evidence.workflowSha256, '7edc6be49c0067e53e8f82e35748ac76acf9c2b51b2b0d9d44604ab14e808772');
  assert.deepEqual(evidence.jobs.map(job => job.name), names.map(name => `smoke / ${name}`));
});

test('retains the reviewed historical coordinator and reports its actual digest', async () => {
  const evidence = await verifyPublicSmoke(await fixture({ coordinator: true, historicalCoordinator: true }), identity);
  assert.equal(evidence.contract, 'coordinator-push');
  assert.equal(evidence.workflowSha256, '4eb37a029e390ee4c86a5fd8d44c04036c99155f42ccdb473e497a558c8e8e8c');
});

test('rejects unobservable recovery inputs, wrong commit, schedules, changed wiring, and incomplete jobs', async () => {
  for (const options of [
    { coordinator: true, event: 'workflow_dispatch', head: 'c'.repeat(40) },
    { coordinator: true, event: 'workflow_dispatch' },
    { head: 'c'.repeat(40) }, { event: 'schedule' },
    { changedWorkflow: true }, { coordinator: true, changedWorkflow: true }, { missing: 'resolve' }, { missing: 'Windows installer' },
    { jobAttempt: 1 }, { jobConclusion: 'skipped' }, { jobConclusion: 'failure' },
  ]) await assert.rejects(verifyPublicSmoke(await fixture(options), identity), /no successful public smoke/);
});

test('rejects evidence that may have tested a deleted and recreated release', async () => {
  await assert.rejects(verifyPublicSmoke(await fixture(), { ...identity, publishedAt: '2026-10-21T00:00:00Z' }), /no successful public smoke/);
});

test('accepts exact workflow paths with nonempty ref suffixes for both contracts', async () => {
  for (const coordinator of [false, true]) {
    const workflow = coordinator ? 'crates-publication.yml' : 'release-public-smoke.yml';
    await verifyPublicSmoke(await fixture({ coordinator, path: `.github/workflows/${workflow}@refs/heads/main` }), identity);
  }
});
test('rejects workflow lookalikes and malformed suffixes', async () => {
  for (const path of ['.github/workflows/other.yml@main', '.github/workflows/release-public-smoke.yml.extra@main', '.github/workflows/release-public-smoke.yml@', '.github/workflows/release-public-smoke.yml@main@other']) {
    await assert.rejects(verifyPublicSmoke(await fixture({ path }), identity), /workflow identity/);
  }
});

test('accepts documented jobs without an attempt field and rejects wrong run or head', async () => {
  await verifyPublicSmoke(await fixture({ omitAttempt: true }), identity);
  for (const options of [{ jobRun: 78 }, { jobHead: 'c'.repeat(40) }]) {
    await assert.rejects(verifyPublicSmoke(await fixture(options), identity), /no successful public smoke/);
  }
});

test('filtered multi-page discovery ignores unrelated returned runs and finds release evidence', async () => {
  const evidence = await verifyPublicSmoke(await fixture({ history: true }), identity);
  assert.equal(evidence.runId, 77);
});
