import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { PublicTransport } from './public_transport.ts';
import { verifyPublicSmoke } from './public_smoke.ts';

const commit = 'b'.repeat(40);
const identity = { tag: 'v0.0.11', commit, releaseId: 42, publishedAt: '2026-10-19T00:00:00Z' };
const names = ['resolve', 'Unix installer (ubuntu-latest)', 'Unix installer (macos-latest)', 'Windows installer'];
async function fixture(options: { coordinator?: boolean; event?: string; head?: string; missing?: string; changedWorkflow?: boolean } = {}) {
  const workflow = options.coordinator ? 'crates-publication.yml' : 'release-public-smoke.yml';
  const head = options.head ?? commit;
  const run = { id: 77, run_attempt: 2, event: options.event ?? (options.coordinator ? 'push' : 'release'), head_branch: options.coordinator ? 'main' : identity.tag, head_sha: head, path: `.github/workflows/${workflow}`, status: 'completed', conclusion: 'success' };
  const transport = new PublicTransport({ fetch: async input => {
    const url = new URL(String(input));
    if (url.hostname === 'raw.githubusercontent.com') {
      const file = url.pathname.split('/').slice(4).join('/');
      if (file === 'docs/releases/intent.json') return Response.json({ schema: 'tapid-release-intent-v1', version: '0.0.11', baseline: 'v0.0.10', prepared_from: 'c'.repeat(40), notes: 'docs/releases/0.0.11.md', packages: [{ name: 'tapid', version: '0.0.11' }] });
      return new Response(options.changedWorkflow ? 'unreviewed workflow' : await readFile(new URL(`../../${file}`, import.meta.url)));
    }
    if (url.pathname.endsWith('/runs')) return Response.json({ workflow_runs: url.pathname.includes(workflow) ? [run] : [] });
    if (url.pathname.endsWith('/jobs')) return Response.json({ jobs: names.filter(name => name !== options.missing).map((name, index) => ({ id: index + 1, run_id: 77, run_attempt: 2, head_sha: head, name: options.coordinator ? `smoke / ${name}` : name, status: 'completed', conclusion: 'success', started_at: '2026-10-20T00:00:00Z' })) });
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
  assert.deepEqual(evidence.jobs.map(job => job.name), names.map(name => `smoke / ${name}`));
});

test('rejects unobservable recovery inputs, wrong commit, schedules, changed wiring, and incomplete jobs', async () => {
  for (const options of [
    { coordinator: true, event: 'workflow_dispatch', head: 'c'.repeat(40) },
    { coordinator: true, event: 'workflow_dispatch' },
    { head: 'c'.repeat(40) }, { event: 'schedule' },
    { changedWorkflow: true }, { missing: 'resolve' }, { missing: 'Windows installer' },
  ]) await assert.rejects(verifyPublicSmoke(await fixture(options), identity), /no successful public smoke/);
});

test('rejects evidence that may have tested a deleted and recreated release', async () => {
  await assert.rejects(verifyPublicSmoke(await fixture(), { ...identity, publishedAt: '2026-10-21T00:00:00Z' }), /no successful public smoke/);
});
