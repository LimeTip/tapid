import { deepStrictEqual, rejects, strictEqual } from 'node:assert/strict';
import { test } from 'node:test';
import { expectedAssets } from './candidate.ts';
import { capturePublicIdentity } from './public_smoke.ts';

const sha = 'a'.repeat(40);
const release = (tag = 'v1.2.3') => ({ id: 42, tag_name: tag, draft: false, prerelease: false,
  assets: expectedAssets(tag).map((name, index) => ({ id: index + 1, name, size: 100 })) });
function transport(metadata = release()) {
  const urls: string[] = [];
  return { urls, fetch: async (url: string, options: RequestInit) => {
    urls.push(url);
    strictEqual(options.credentials, 'omit');
    strictEqual(new Headers(options.headers).has('authorization'), false);
    return new Response(JSON.stringify(url.includes('/commits/') ? { sha } : metadata));
  } };
}
test('empty selection captures one public stable identity for selected and latest', async () => {
  const api = transport();
  const result = await capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux', 'Windows'] }, api.fetch);
  strictEqual(result.selected.tag, 'v1.2.3');
  deepStrictEqual(result.selected, result.latest);
  strictEqual(result.selected.commit, sha);
  strictEqual(api.urls.length, 2);
});
test('public identity fails closed for invalid inventory and release state', async () => {
  for (const metadata of [
    { ...release(), draft: true }, { ...release(), prerelease: true },
    { ...release(), tag_name: 'v01.2.3' },
    { ...release(), assets: release().assets.slice(1) },
    { ...release(), assets: [...release().assets, release().assets[0]] },
    { ...release(), assets: [...release().assets.slice(1), { id: 99, name: 'extra', size: 1 }] },
  ]) await rejects(capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'] }, transport(metadata).fetch));
});
test('coordinator identity mismatch and malformed requested tags fail', async () => {
  for (const options of [{ tag: '../latest' }, { releaseId: '43', commit: sha }, { releaseId: '42', commit: 'b'.repeat(40) }]) {
    await rejects(capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'], ...options }, transport().fetch));
  }
});

test('public discovery cannot silently replace captured latest or selected source', async () => {
  for (const captured of [{ expectedLatest: { tag: 'v1.2.2', commit: sha } },
    { expectedLatest: { tag: 'v1.2.3', commit: 'b'.repeat(40) } }, { expectedSelectedCommit: 'b'.repeat(40) }]) {
    await rejects(capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'], ...captured }, transport().fetch), /changed after capture/);
  }
});

test('historical selection remains separate from latest identity', async () => {
  const latest = release('v2.0.0');
  const selected = { ...release('v1.2.3'), id: 41 };
  const fetcher = async (url: string) => new Response(JSON.stringify(url.includes('/commits/')
    ? { sha: url.endsWith('v1.2.3') ? 'b'.repeat(40) : sha }
    : url.endsWith('/latest') ? latest : selected));
  const result = await capturePublicIdentity({ tag: 'v1.2.3', runnerSha: sha, run: '123/1', platforms: ['Linux'] }, fetcher);
  strictEqual(result.selected.commit, 'b'.repeat(40));
  strictEqual(result.latest.commit, sha);
});

test('public discovery rejects failures, oversized metadata and unsafe redirects', async () => {
  for (const response of [new Response('', { status: 404 }), new Response('x'.repeat(256 * 1024 + 1)),
    new Response('', { status: 302, headers: { location: 'http://example.com' } }),
    new Response('', { status: 302, headers: { location: 'https://user:secret@example.com' } })]) {
    await rejects(capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'] }, async () => response));
  }
});

test('inventory authenticates record before accepting archive metadata', async () => {
  const { verifyPublicInventory } = await import('./public_smoke.ts');
  const { mkdtemp, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const directory = await mkdtemp(join(tmpdir(), 'tapid-public-smoke-'));
  try {
    const identity = await capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'] }, transport().fetch);
    let archives = 0;
    await rejects(verifyPublicInventory(identity, directory, {
      fetch: async () => new Response('untrusted'),
      verifyRecord: async () => { throw Error('invalid signature'); },
      verifyArchive: async () => { archives++; },
      templates: async () => { throw Error('must not reach templates'); },
    }), /invalid signature/);
    strictEqual(archives, 0);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('aggregation fails for absent, unsuccessful or mismatched platform evidence', async () => {
  const { aggregateSmoke } = await import('./public_smoke.ts');
  const identity = await capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux', 'macOS', 'Windows'] }, transport().fetch);
  const report = (platform: string) => ({ schema: 'tapid-public-smoke-platform-v1', identity, platform,
    checks: { install_published: 'success', selected_consumer: 'success', install_public: 'success', install_latest: 'success', latest_consumer: 'success', upgrade: 'success', previous_repeat: 'success' } });
  const inventory = { schema: 'tapid-public-smoke-inventory-v1', status: 'passed', identity, assets: expectedAssets('v1.2.3').map(name => ({ tag: 'v1.2.3', name, size: 100, sha256: 'c'.repeat(64), url: `https://github.com/LimeTip/tapid/releases/download/v1.2.3/${name}` })) };
  strictEqual(aggregateSmoke(inventory, [report('Linux'), report('macOS'), report('Windows')], { resolve: 'success', 'public-readback': 'success', unix: 'success', windows: 'success' }, '123/1').status, 'passed');
  for (const reports of [[report('Linux')], [report('Linux'), report('macOS'), { ...report('Windows'), identity: { ...identity, run: '123/2' } }],
    [report('Linux'), report('macOS'), { ...report('Windows'), checks: { ...report('Windows').checks, latest_consumer: 'failure' } }]]) {
    strictEqual(aggregateSmoke(inventory, reports, { resolve: 'success', 'public-readback': 'success', unix: 'success', windows: 'success' }, '123/1').status, 'failed');
  }
  strictEqual(aggregateSmoke(inventory, [report('Linux'), report('macOS'), report('Windows')], { resolve: 'success', 'public-readback': 'failure', unix: 'success', windows: 'success' }, '123/1').status, 'failed');
  const scheduledIdentity = { ...identity, platforms: ['Linux'] };
  const scheduledReports = [{ ...report('Linux'), identity: scheduledIdentity }];
  const scheduled = aggregateSmoke({ ...inventory, identity: scheduledIdentity }, scheduledReports,
    { resolve: 'success', 'public-readback': 'success', unix: 'success', windows: 'skipped' }, '123/1', true);
  strictEqual(scheduled.status, 'passed');
  strictEqual(scheduled.coverage, 'scheduled Linux only');
  strictEqual(aggregateSmoke({ ...inventory, identity: { ...identity, platforms: [] } }, [],
    { resolve: 'success', 'public-readback': 'success', unix: 'success', windows: 'skipped' }, '123/1').status, 'failed');
});

test('public inventory binds generated records, checksums, discovery and installers to archive bytes', async () => {
  const { verifyPublicInventory } = await import('./public_smoke.ts');
  const { mkdtemp, readFile, writeFile, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { TARGETS, releaseRecord, checksumLines } = await import('./release.ts');
  const { renderInstallers } = await import('./bootstrap.ts');
  const fixture = await mkdtemp(join(tmpdir(), 'tapid-public-fixture-'));
  const base = 'https://github.com/LimeTip/tapid/releases/download/v1.2.3';
  try {
    for (const target of TARGETS) await writeFile(join(fixture, `tapid-1.2.3-${target}.tar.gz`), 'fixture archive');
    await writeFile(join(fixture, 'tapid-release-v1.tsv'), await releaseRecord(fixture, '1.2.3', base));
    await writeFile(join(fixture, 'tapid-release-v1.tsv.sig'), 'fixture signature');
    await writeFile(join(fixture, 'SHA256SUMS'), await checksumLines(fixture, '1.2.3'));
    const templates = { 'install.sh': await readFile(new URL('../../scripts/install.sh', import.meta.url), 'utf8'),
      'install.ps1': await readFile(new URL('../../scripts/install.ps1', import.meta.url), 'utf8') };
    await renderInstallers(fixture, '1.2.3', base, templates);
    const bytes = new Map<string, Buffer>();
    for (const name of expectedAssets('v1.2.3')) bytes.set(`${base}/${name}`, await readFile(join(fixture, name)));
    for (const route of ['v1.2.3', 'latest']) for (const suffix of ['', '.sig']) bytes.set(`https://tapid.dev/releases/v1/${route}.tsv${suffix}`, bytes.get(`${base}/tapid-release-v1.tsv${suffix}`)!);
    for (const name of ['install.sh', 'install.ps1']) bytes.set(`https://tapid.dev/${name}`, bytes.get(`${base}/${name}`)!);
    bytes.set('https://tapid.dev/', Buffer.from('<a href="/docs/releases/latest/">Releases</a>'));
    bytes.set('https://tapid.dev/docs/releases/latest/', Buffer.from('<h1>Release documentation</h1>'));
    const metadata = release();
    metadata.assets = metadata.assets.map(asset => ({ ...asset, size: bytes.get(`${base}/${asset.name}`)!.length }));
    const identity = await capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'] }, transport(metadata).fetch);
    const scenarios: (string | undefined)[] = [undefined, `${base}/SHA256SUMS`, `${base}/tapid-release-v1.tsv`, `${base}/tapid-1.2.3-x86_64-apple-darwin.tar.gz`,
      'https://tapid.dev/releases/v1/latest.tsv', 'https://tapid.dev/releases/v1/latest.tsv.sig',
      `${base}/install.ps1`, 'https://tapid.dev/install.sh', 'https://tapid.dev/', 'https://tapid.dev/docs/releases/latest/'];
    for (const corrupt of scenarios) {
      const output = await mkdtemp(join(tmpdir(), 'tapid-public-inventory-'));
      try {
        const operation = verifyPublicInventory(identity, output, {
          fetch: async (url, options) => {
            strictEqual(options.credentials, 'omit');
            strictEqual(new Headers(options.headers).has('authorization'), false);
            const body = bytes.get(url);
            return body ? new Response(corrupt === url ? (url.endsWith('.tar.gz') ? Buffer.from(body).fill(0, 0, 1) : Buffer.from('tampered')) : body) : new Response('', { status: 404 });
          },
          verifyRecord: async () => {}, verifyArchive: async () => {}, templates: async () => templates,
        });
        if (corrupt) await rejects(operation);
        else strictEqual((await operation).length, 11);
      } finally { await rm(output, { recursive: true, force: true }); }
    }
  } finally { await rm(fixture, { recursive: true, force: true }); }
});

test('artifact reporter retains a failed summary when frozen replay evidence fails or reports are corrupt', async () => {
  const { mkdtemp, mkdir, writeFile, readFile, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const { execFile } = await import('node:child_process');
  const { promisify } = await import('node:util');
  const run = promisify(execFile);
  const root = await mkdtemp(join(tmpdir(), 'tapid-public-report-'));
  try {
    await mkdir(join(root, 'public-release-inventory'));
    await mkdir(join(root, 'public-smoke-platform-Linux'));
    const identity = await capturePublicIdentity({ runnerSha: sha, run: '123/1', platforms: ['Linux'] }, transport().fetch);
    await writeFile(join(root, 'public-release-inventory', 'inventory.json'), JSON.stringify({ schema: 'tapid-public-smoke-inventory-v1', status: 'passed', identity,
      assets: expectedAssets('v1.2.3').map(name => ({ tag: 'v1.2.3', name, size: 100, sha256: 'c'.repeat(64), url: `https://github.com/LimeTip/tapid/releases/download/v1.2.3/${name}` })) }));
    const checks = { install_published: 'success', selected_consumer: 'success', install_public: 'success', install_latest: 'success', latest_consumer: 'failure', upgrade: 'success', previous_repeat: 'success' };
    const file = join(root, 'public-smoke-platform-Linux', 'platform.json');
    await writeFile(file, JSON.stringify({ schema: 'tapid-public-smoke-platform-v1', identity, platform: 'Linux', checks }));
    const args = ['--experimental-strip-types', fileURLToPath(new URL('./public_smoke_report.ts', import.meta.url)), 'aggregate', root];
    const env = { GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1', EVENT_NAME: 'schedule',
      SMOKE_JOBS: JSON.stringify({ resolve: { result: 'success' }, 'public-readback': { result: 'success' }, unix: { result: 'success' }, windows: { result: 'skipped' } }) };
    await rejects(run(process.execPath, args, { env }));
    const summary = JSON.parse(await readFile(join(root, 'summary.json'), 'utf8'));
    strictEqual(summary.status, 'failed');
    strictEqual(summary.failures.includes('Linux: latest_consumer did not succeed'), true);
    await writeFile(file, '{');
    await rejects(run(process.execPath, args, { env }));
    strictEqual(JSON.parse(await readFile(join(root, 'summary.json'), 'utf8')).status, 'failed');
  } finally { await rm(root, { recursive: true, force: true }); }
});
