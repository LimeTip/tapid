import assert from 'node:assert/strict';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
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
    execFile(process.execPath, ['--experimental-strip-types', fileURLToPath(new URL('./verify-public.ts', import.meta.url)), 'v01.0.0', '--json'], { env: {}, timeout: 10_000 }, (error, stdout, stderr) => resolve({ code: Number(error?.code ?? 0), stdout, stderr }));
  });
  assert.equal(result.code, 1);
  assert.equal(JSON.parse(result.stdout).checks[0].status, 'fail');
  assert.match(result.stderr, /tag=fail/);
});

test('command inspects all six target-specific archives before reporting archive safety', async () => {
  const { createHash } = await import('node:crypto');
  const { TARGETS } = await import('./release.ts');
  const { readdir } = await import('node:fs/promises');
  const { dirname, basename } = await import('node:path');
  const tag = 'v0.0.10';
  const names = TARGETS.map(target => `tapid-0.0.10-${target}.tar.gz`);
  const hash = createHash('sha256').update('archive fixture').digest('hex');
  const checksums = names.map(name => `${hash}  ${name}\n`).join('');
  const assets = [...names, 'SHA256SUMS'].map((name, i) => ({ id: i + 1, name,
    size: name === 'SHA256SUMS' ? Buffer.byteLength(checksums) : 15,
    browser_download_url: `https://github.com/LimeTip/tapid/releases/download/${tag}/${name}` }));
  const makeTransport = () => new PublicTransport({ fetch: async input => {
    const path = new URL(String(input)).pathname;
    if (path.includes('/releases/download/')) return new Response(path.endsWith('/SHA256SUMS') ? checksums : 'archive fixture');
    if (path.endsWith(`/git/ref/tags/${tag}`)) return Response.json({ ref: `refs/tags/${tag}`, object: { type: 'tag', sha: object } });
    if (path.endsWith(`/git/tags/${object}`)) return Response.json({ sha: object, tag, object: { type: 'commit', sha: commit } });
    if (path.endsWith('/releases')) return Response.json([{ id: 42, tag_name: tag, draft: false, prerelease: false, immutable: true, assets }]);
    return Response.json({ workflow_runs: [] });
  } });
  const inspected: string[] = [];
  let directory = '';
  const report = await verifyPublicRelease(tag, { transport: makeTransport(), verifier: async () => ({
    async inspectArchive(path, executable) {
      directory = dirname(path);
      inspected.push(basename(path));
      assert.equal(executable, path.includes('windows') ? 'tapid.exe' : 'tapid');
    }, async verifySignature() { assert.fail('historical contract has no signature'); },
  }) });
  assert.deepEqual(inspected.sort(), names.sort());
  assert.equal(report.checks.find(check => check.name === 'archives')?.status, 'pass');
  await assert.rejects(readdir(directory), { code: 'ENOENT' });
  const rejected = await verifyPublicRelease(tag, { transport: makeTransport(), verifier: async () => ({
    async inspectArchive() { throw Error('native archive inspection failed'); }, async verifySignature() {},
  }) });
  assert.equal(rejected.checks.find(check => check.name === 'archives')?.status, 'fail');
  assert.equal(rejected.ok, false);
});

test('JSON CLI test executes from a checkout path containing spaces', async () => {
  const { cp, mkdtemp, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { execFile } = await import('node:child_process');
  const { promisify } = await import('node:util');
  const directory = await mkdtemp(join(tmpdir(), 'tapid spaced checkout '));
  try {
    const release = join(directory, 'tools', 'release');
    await cp(fileURLToPath(new URL('./', import.meta.url)), release, { recursive: true });
    await promisify(execFile)(process.execPath, [
      '--experimental-strip-types', '--test',
      '--test-name-pattern=^JSON CLI reserves stdout',
      join(release, 'verify-public_test.ts'),
    ], { env: {}, timeout: 20_000 });
  } finally { await rm(directory, { recursive: true, force: true }); }
});
