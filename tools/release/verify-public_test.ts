import assert from 'node:assert/strict';
import test from 'node:test';
import { verifyPublicRelease } from './verify-public.ts';
import { PublicTransport } from './public_transport.ts';

const object = 'a'.repeat(40), commit = 'b'.repeat(40);
function fixture(overrides: Record<string, unknown> = {}) {
  const requests: string[] = [];
  const responses: Record<string, unknown> = {
    '/repos/LimeTip/tapid/git/ref/tags/v0.0.11': { ref: 'refs/tags/v0.0.11', object: { type: 'tag', sha: object } },
    [`/repos/LimeTip/tapid/git/tags/${object}`]: { sha: object, tag: 'v0.0.11', object: { type: 'commit', sha: commit } },
    '/repos/LimeTip/tapid/releases': [{ id: 42, tag_name: 'v0.0.11', draft: false, prerelease: false, immutable: true, assets: [] }],
    ...overrides,
  };
  const transport = new PublicTransport({ fetch: async url => {
    const path = new URL(String(url)).pathname;
    requests.push(path);
    if (path.endsWith('/runs')) return Response.json({ workflow_runs: [] });
    assert.ok(path in responses, path);
    return Response.json(responses[path]);
  } });
  return { requests, transport };
}

test('canonical input is validated before any public request', async () => {
  for (const tag of ['0.0.11', 'v01.0.0', 'v1.0.0-beta', 'v1.0.0+build', 'v18446744073709551616.0.0']) {
    const { requests, transport } = fixture();
    const report = await verifyPublicRelease(tag, { transport });
    assert.equal(report.ok, false);
    assert.equal(report.checks[0].name, 'tag');
    assert.equal(report.checks[0].status, 'fail');
    assert.deepEqual(requests, []);
  }
});

test('binds annotated object and direct commit to exactly one immutable release', async () => {
  const { transport } = fixture();
  const report = await verifyPublicRelease('v0.0.11', { transport });
  assert.deepEqual(report.identity, { tagObject: object, commit, releaseId: 42 });
  assert.equal(report.contract, 'signed-record-v1');
  assert.equal(report.checks.find(check => check.name === 'identity')?.status, 'pass');
  // Identity alone cannot establish full release verification.
  assert.equal(report.ok, false);
});

test('rejects lightweight, indirectly tagged, and inconsistent annotated identities', async () => {
  for (const overrides of [
    { '/repos/LimeTip/tapid/git/ref/tags/v0.0.11': { ref: 'refs/tags/v0.0.11', object: { type: 'commit', sha: commit } } },
    { [`/repos/LimeTip/tapid/git/tags/${object}`]: { sha: object, tag: 'v0.0.11', object: { type: 'tag', sha: commit } } },
    { [`/repos/LimeTip/tapid/git/tags/${object}`]: { sha: commit, tag: 'v0.0.11', object: { type: 'commit', sha: commit } } },
    { [`/repos/LimeTip/tapid/git/tags/${object}`]: { sha: object, tag: 'v0.0.10', object: { type: 'commit', sha: commit } } },
  ]) {
    const report = await verifyPublicRelease('v0.0.11', { transport: fixture(overrides).transport });
    assert.equal(report.identity, undefined);
    assert.equal(report.checks.find(check => check.name === 'identity')?.status, 'fail');
  }
});

test('fails closed on missing, duplicate, mutable, draft, and prerelease releases', async () => {
  const release = { id: 42, tag_name: 'v0.0.11', draft: false, prerelease: false, immutable: true, assets: [] };
  for (const releases of [[], [release, release], [release, { ...release, id: 43 }],
    [{ ...release, immutable: false }], [{ ...release, draft: true }], [{ ...release, prerelease: true }]]) {
    const report = await verifyPublicRelease('v0.0.11', { transport: fixture({ '/repos/LimeTip/tapid/releases': releases }).transport });
    assert.equal(report.identity, undefined);
    assert.equal(report.checks.find(check => check.name === 'identity')?.status, 'fail');
  }
});

test('identical evidence produces byte-identical JSON with no temporary paths', async () => {
  const a = await verifyPublicRelease('v0.0.11', { transport: fixture().transport });
  const b = await verifyPublicRelease('v0.0.11', { transport: fixture().transport });
  assert.equal(JSON.stringify(a), JSON.stringify(b));
  assert.ok(!JSON.stringify(a).includes('/tapid-public-'));
});

test('JSON CLI reserves stdout for the report and exits nonzero for invalid tags', async () => {
  const { execFile } = await import('node:child_process');
  const result = await new Promise<{ code: number; stdout: string; stderr: string }>(resolve => {
    execFile(process.execPath, ['--experimental-strip-types', new URL('./verify-public.ts', import.meta.url).pathname, 'v01.0.0', '--json'], { env: {}, timeout: 10_000 }, (error, stdout, stderr) => resolve({ code: Number(error?.code ?? 0), stdout, stderr }));
  });
  assert.equal(result.code, 1);
  assert.equal(JSON.parse(result.stdout).checks[0].status, 'fail');
  assert.match(result.stderr, /tag=fail/);
});
