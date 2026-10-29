import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { collectAuthorization } from './evidence.ts';
import { expectedAssets, type UnsignedCandidate } from './candidate.ts';

const candidate: UnsignedCandidate = {
  schema: 'tapid-release-unsigned-candidate-v1', repository: 'LimeTip/tapid', tag: 'v0.0.12',
  commit: 'a'.repeat(40), tag_object: 'b'.repeat(40), title: 'v0.0.12',
  notes_sha256: 'c'.repeat(64), plan_sha256: 'd'.repeat(64),
  assets: expectedAssets('v0.0.12').filter(name => !name.endsWith('.sig')).map(name => ({ name, size: 10, sha256: 'e'.repeat(64) })),
};
const context = { repository: 'LimeTip/tapid', run_id: 42, attempt: 1, workflow_sha: 'f'.repeat(40) };
const run = { id: 42, run_attempt: 1, head_sha: context.workflow_sha, head_branch: 'main', path: '.github/workflows/crates-publication.yml', event: 'push', actor: { login: 'operator' }, triggering_actor: { login: 'operator' } };
const approval = { state: 'approved', environments: [{ id: 12, name: 'stable-release' }], user: { login: 'reviewer', id: 7, type: 'User' } };
const job = { id: 99, run_id: 42, run_attempt: 1, head_sha: context.workflow_sha, name: 'candidate / draft-release', status: 'in_progress' };
const command = (changes: Record<string, unknown> = {}) => async (args: string[]) => {
  const path = args[1];
  if (path.endsWith('/approvals')) return JSON.stringify(changes.approvals ?? [[approval]]);
  if (path.endsWith('/jobs?per_page=100')) return JSON.stringify(changes.jobs ?? [{ jobs: [job] }]);
  return JSON.stringify(changes.run ?? run);
};

test('collects one independent first-attempt authorization without inventing a timestamp', async () => {
  const result = await collectAuthorization(context, candidate, command());
  assert.equal(result.actor, 'reviewer');
  assert.equal(result.job_id, 99);
  assert.equal(result.attempt, 1);
  assert.equal(result.review_url, 'https://api.github.com/repos/LimeTip/tapid/actions/runs/42/approvals');
  assert(!('timestamp' in result));
});

test('rejects absent, rejected, ambiguous, self and wrong-environment reviews across pages', async () => {
  for (const approvals of [
    [[]], [[{ ...approval, state: 'rejected' }]], [[approval], [approval]],
    [[{ ...approval, user: { login: 'operator', id: 7, type: 'User' } }]],
    [[{ ...approval, environments: [{ id: 12, name: 'crates-io-release' }] }]],
    [[{ ...approval, user: { login: 'bot', id: 7, type: 'Bot' } }]],
    [[{ ...approval, environments: [{ id: 0, name: 'stable-release' }] }]],
  ]) await assert.rejects(collectAuthorization(context, candidate, command({ approvals })));
  const result = await collectAuthorization(context, candidate, command({ approvals: [[], [approval]], jobs: [{ jobs: [] }, { jobs: [job] }] }));
  assert.equal(result.actor, 'reviewer');
});

test('rejects stale attempts, other sources and protected-job identity mismatches', async () => {
  await assert.rejects(collectAuthorization({ ...context, attempt: 2 }, candidate, command()), /attempt cannot be proven/);
  await assert.rejects(collectAuthorization(context, { ...candidate, repository: 'other/repo' }, command()), /repository mismatch/);
  for (const changed of [ { run_attempt: 2 }, { head_sha: candidate.commit }, { head_branch: 'feature' }, { event: 'pull_request' }, { path: '.github/workflows/other.yml' } ]) {
    await assert.rejects(collectAuthorization(context, candidate, command({ run: { ...run, ...changed } })), /identity mismatch/);
  }
  for (const changed of [ { run_attempt: 2 }, { head_sha: candidate.commit }, { status: 'completed' }, { name: 'draft-release' } ]) {
    await assert.rejects(collectAuthorization(context, candidate, command({ jobs: [{ jobs: [{ ...job, ...changed }] }] })), /job identity mismatch/);
  }
  await assert.rejects(collectAuthorization(context, candidate, async () => { throw Error('unavailable'); }), /unavailable/);
});

test('fingerprint retains notes, plan and asset identity and excludes untrusted review text', async () => {
  const original = await collectAuthorization(context, candidate, command({ approvals: [[{ ...approval, comment: 'secret example <script>' }]] }));
  assert(!JSON.stringify(original).includes('secret'));
  for (const modified of [
    { ...candidate, notes_sha256: '1'.repeat(64) },
    { ...candidate, plan_sha256: '1'.repeat(64) },
    { ...candidate, assets: candidate.assets.map((asset, i) => i === 0 ? { ...asset, sha256: '1'.repeat(64) } : asset) },
  ]) assert.notEqual((await collectAuthorization(context, modified, command())).unsigned_sha256, original.unsigned_sha256);
  await assert.rejects(collectAuthorization(context, { ...candidate, assets: [] }, command()), /asset/);
});

test('early failure produces an incomplete ledger with recovery links and no fabricated candidate', async () => {
  const { completionLedger, renderLedger } = await import('./evidence.ts');
  const ledger = completionLedger(context, { preflight: { result: 'failure' } }, [], {});
  assert.equal(ledger.state, 'incomplete');
  assert.equal(ledger.candidate, null);
  assert(ledger.missing.includes('unsigned candidate unavailable'));
  assert.match(renderLedger(ledger), /Recover/);
  assert(!renderLedger(ledger).includes('Release complete'));
});

test('complete ledger binds signed assets, authorization, plan, platforms and registry; failures stay incomplete', async () => {
  const { completionLedger, renderLedger } = await import('./evidence.ts');
  const { createHash } = await import('node:crypto');
  const intent = JSON.stringify({ schema: 'tapid-release-intent-v1', version: '0.0.12', baseline: 'v0.0.11', prepared_from: '1'.repeat(40), notes: 'docs/releases/0.0.12.md', packages: [{ name: 'tapid-core', version: '0.0.3' }, { name: 'tapid', version: '0.0.12' }] });
  const unsigned = { ...candidate, notes_sha256: createHash('sha256').update('reviewed notes').digest('hex'), plan_sha256: createHash('sha256').update(intent).digest('hex') };
  const signed = { ...unsigned, schema: 'tapid-release-candidate-v1', release_id: 25, assets: [...unsigned.assets, { name: 'tapid-release-v1.tsv.sig', size: 10, sha256: 'a'.repeat(64) }].sort((a,b) => a.name.localeCompare(b.name, 'en')).map((asset, i) => ({ ...asset, id: i + 1 })) };
  // Use the canonical release asset order, independent of host collation.
  signed.assets.sort((a, b) => expectedAssets(unsigned.tag).indexOf(a.name) - expectedAssets(unsigned.tag).indexOf(b.name));
  const needs = Object.fromEntries(['preflight', 'select', 'candidate', 'verify', 'promote', 'smoke', 'publish'].map(stage => [stage, { result: 'success' }]));
  const names = [ ...['ubuntu-latest', 'ubuntu-24.04-arm', 'macos-15-intel', 'macos-14', 'windows-latest', 'windows-11-vs2026-arm'].map(runner => `verify / Install and execute (${runner})`), 'smoke / Unix installer (ubuntu-latest)', 'smoke / Unix installer (macos-latest)', 'smoke / Windows installer' ];
  const jobs = names.map((name, i) => ({ ...job, id: i + 1, name, conclusion: 'success' }));
  const files = {
    preparation_pr: 115,
    artifacts: [{ id: 400, name: 'signed-release-candidate-1', digest: 'sha256:' + '1'.repeat(64), size_in_bytes: 100 }],
    public_readback: { tag: { object: unsigned.tag_object, commit: unsigned.commit }, release: { id: 25, tag_name: unsigned.tag, name: unsigned.tag, body: 'reviewed notes', draft: false, prerelease: false, immutable: true, assets: signed.assets.map(asset => ({ ...asset, digest: 'sha256:' + asset.sha256 })) } },
    unsigned, signed, intent, authorization: await collectAuthorization(context, unsigned, command()), clean_install: { version: 'tapid 0.0.12', help: true }, registry: { confirmed: JSON.parse(intent).packages, remaining: [] },
  };
  const ledger = completionLedger(context, needs, jobs, files);
  assert.equal(ledger.state, 'complete');
  assert.match(renderLedger(ledger), /Release complete/);
  const draftFailure = completionLedger(context, { ...needs, verify: { result: 'failure' } }, jobs, { ...files, public_readback: { ...files.public_readback, release: { ...files.public_readback.release, draft: true, immutable: false } } });
  assert.equal(draftFailure.state, 'incomplete');
  assert.equal(draftFailure.signed_candidate?.release_id, 25);
  assert.equal(draftFailure.authorization?.actor, 'reviewer');
  assert.equal(completionLedger(context, needs, [], files).state, 'incomplete');
  assert.equal(completionLedger(context, { ...needs, smoke: { result: 'failure' } }, jobs, files).state, 'incomplete');
  assert.equal(completionLedger(context, needs, jobs, { ...files, clean_install: { version: 'tapid 0.0.12', help: false } }).state, 'incomplete');
  const packages = JSON.parse(intent).packages;
  const partial = completionLedger(context, needs, jobs, { ...files, registry: { confirmed: packages.slice(0, 1), remaining: packages.slice(1) } });
  assert.equal(partial.state, 'incomplete');
  assert.equal(partial.registry?.confirmed.length, 1);
  assert.throws(() => completionLedger(context, needs, jobs, { ...files, registry: { confirmed: packages.slice(1), remaining: packages.slice(0, 1) } }), /suffix/);
  assert.throws(() => completionLedger(context, needs, jobs, { ...files, intent: intent + '\n' }), /plan mismatch/);
  assert.throws(() => completionLedger(context, needs, jobs, { ...files, authorization: { ...files.authorization, unsigned_sha256: '0'.repeat(64) } }), /candidate\/run mismatch/);
  assert.throws(() => completionLedger(context, needs, jobs, { ...files, signed: { ...signed, notes_sha256: '0'.repeat(64) } }), /authorized unsigned/);
  const safe = completionLedger(context, needs, jobs, { ...files, authorization: { ...files.authorization, comment: '<script>secret</script>' }, clean_install: { ...files.clean_install, token: 'secret' } });
  assert(!JSON.stringify(safe).includes('secret'));
});

test('malformed review records cannot be hidden beside a valid approval', async () => {
  await assert.rejects(collectAuthorization(context, candidate, command({ approvals: [[approval, { state: 'rejected' }]] })), /malformed review/);
});

test('within-run retries require retained first-attempt authorization of identical bytes', async () => {
  const prior = await collectAuthorization(context, candidate, command());
  const retryContext = { ...context, attempt: 2 };
  const retryCommand = command({ run: { ...run, run_attempt: 2 }, jobs: [{ jobs: [{ ...job, run_attempt: 2 }] }] });
  const result = await collectAuthorization(retryContext, candidate, retryCommand, { authorization: prior, artifact_id: 300 });
  assert.equal(result.approved_attempt, 1);
  assert.equal(result.reused_authorization_artifact_id, 300);
  assert.equal(result.attempt, 2);
  await assert.rejects(collectAuthorization(retryContext, { ...candidate, notes_sha256: '0'.repeat(64) }, retryCommand, { authorization: prior, artifact_id: 300 }), /retained authorization mismatch/);
  await assert.rejects(collectAuthorization(retryContext, candidate, retryCommand, { authorization: { ...prior, run_id: 43 }, artifact_id: 300 }), /retained authorization mismatch/);
});

test('a run without preparation, artifact or final public provenance remains incomplete', async () => {
  const { completionLedger } = await import('./evidence.ts');
  const ledger = completionLedger(context, {}, [], {});
  for (const missing of ['preparation PR unavailable', 'artifact identities unavailable', 'final public read-back unavailable']) assert(ledger.missing.includes(missing));
});

test('an interrupted registry read-back preserves confirmed exact versions without claiming completion', async () => {
  const { completionLedger } = await import('./evidence.ts');
  const intent = JSON.stringify({ schema: 'tapid-release-intent-v1', version: '0.0.12', baseline: 'v0.0.11', prepared_from: '1'.repeat(40), notes: 'docs/releases/0.0.12.md', packages: [{ name: 'tapid-core', version: '0.0.3' }, { name: 'tapid', version: '0.0.12' }] });
  const ledger = completionLedger(context, {}, [], { intent, registry_observations: [{ name: 'tapid-core', version: '0.0.3', state: 'published' }, { name: 'tapid', version: '0.0.12', state: 'unavailable' }] });
  assert.equal(ledger.state, 'incomplete');
  assert.equal(ledger.registry_observations[0].state, 'published');
  assert.equal(ledger.registry_observations[1].state, 'unavailable');
});

test('report command retains an incomplete ledger when artifacts and API tooling are unavailable', async () => {
  const { mkdtemp, readFile, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const { execFile } = await import('node:child_process');
  const { promisify } = await import('node:util');
  const directory = await mkdtemp(join(tmpdir(), 'tapid-evidence-'));
  try {
    await promisify(execFile)(process.execPath, ['--experimental-strip-types', fileURLToPath(new URL('./evidence.ts', import.meta.url)), 'report', join(directory, 'absent'), join(directory, 'report')], {
      env: { PATH: '', HOME: directory, SystemRoot: process.env.SystemRoot, GITHUB_REPOSITORY: context.repository, GITHUB_RUN_ID: '42', GITHUB_RUN_ATTEMPT: '1', GITHUB_SHA: context.workflow_sha, RELEASE_NEEDS: JSON.stringify({ preflight: { result: 'failure' } }) },
    });
    const result = JSON.parse(await readFile(join(directory, 'report', 'release-evidence.json'), 'utf8'));
    assert.equal(result.state, 'incomplete');
    assert.equal(result.candidate, null);
    assert(result.missing.includes('attempt-specific job read-back unavailable'));
    assert.match(await readFile(join(directory, 'report', 'summary.md'), 'utf8'), /preflight: failure/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('attempt-scoped job endpoint supports the documented job response without a run_attempt field', async () => {
  const { run_attempt: _attempt, ...documentedJob } = job;
  const result = await collectAuthorization(context, candidate, command({ jobs: [{ jobs: [documentedJob] }] }));
  assert.equal(result.attempt, 1);
  assert.equal(result.job_id, 99);
});

test('summary renders missing-evidence reasons as text and links failed stage logs', async () => {
  const { completionLedger, renderLedger } = await import('./evidence.ts');
  const ledger = completionLedger(context, { preflight: { result: 'failure' } }, [{ ...job, name: 'preflight', conclusion: 'failure' }], {});
  ledger.missing.push('<script>unsafe</script>\n[click](https://example.invalid)');
  const summary = renderLedger(ledger);
  assert.match(summary, /unsigned candidate unavailable/);
  assert.match(summary, /job\/99/);
  assert(!summary.includes('<script>'));
  assert(!summary.includes('[click](https://example.invalid)'));
});
