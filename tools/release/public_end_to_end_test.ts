import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { TARGETS, checksumLines, releaseRecord } from './release.ts';
import { renderInstallers } from './bootstrap.ts';
import { PublicTransport } from './public_transport.ts';
import { verifyPublicRelease } from './verify-public.ts';

const commit = 'b'.repeat(40), tagObject = 'a'.repeat(40);
for (const modern of [false, true]) test(`complete ${modern ? 'signed-record' : 'historical'} public verification with offline tagged metadata`, async () => {
  const version = modern ? '0.0.11' : '0.0.10', tag = `v${version}`;
  const directory = await mkdtemp(join(tmpdir(), 'tapid-public-e2e-'));
  try {
    const base = `https://github.com/LimeTip/tapid/releases/download/${tag}`;
    const templates = {
      'install.sh': await readFile(new URL('../../scripts/install.sh', import.meta.url), 'utf8'),
      'install.ps1': await readFile(new URL('../../scripts/install.ps1', import.meta.url), 'utf8'),
    };
    const names: string[] = TARGETS.map(target => `tapid-${version}-${target}.tar.gz`);
    for (const name of names) await writeFile(join(directory, name), 'archive fixture');
    await writeFile(join(directory, 'SHA256SUMS'), await checksumLines(directory, version));
    names.push('SHA256SUMS');
    if (modern) {
      await writeFile(join(directory, 'tapid-release-v1.tsv'), await releaseRecord(directory, version, base));
      await writeFile(join(directory, 'tapid-release-v1.tsv.sig'), 'native-signature-fixture');
      await renderInstallers(directory, version, base, templates);
      names.push('tapid-release-v1.tsv', 'tapid-release-v1.tsv.sig', 'install.sh', 'install.ps1');
    }
    const contents = new Map(await Promise.all(names.map(async name => [name, await readFile(join(directory, name))] as const)));
    const assets = names.map((name, index) => ({ id: index + 1, name, size: contents.get(name)!.length, browser_download_url: `${base}/${name}` }));
    const source = new Map([
      ['Cargo.toml', `[package]\nname="tapid"\nversion="${version}"\nedition="2024"\n`],
      ['Cargo.lock', `version = 4\n[[package]]\nname = "tapid"\nversion = "${version}"\n`],
      ['src/main.rs', 'fn main() {}\n'],
    ]);
    const jobs = ['resolve', 'Unix installer (ubuntu-latest)', 'Unix installer (macos-latest)', 'Windows installer'];
    const makeTransport = () => new PublicTransport({ delay: async () => {}, fetch: async (input, init) => {
      assert.equal(init?.method, 'GET');
      assert.equal(new Headers(init?.headers).has('authorization'), false);
      const url = new URL(String(input)), path = url.pathname;
      if (url.hostname === 'crates.io') return Response.json({ version: { crate: 'tapid', num: version, yanked: false } });
      if (url.hostname === 'tapid.dev') return new Response(contents.get(path.endsWith('.sig') ? 'tapid-release-v1.tsv.sig' : 'tapid-release-v1.tsv'));
      if (url.hostname === 'raw.githubusercontent.com') {
        const file = path.split(`/${commit}/`)[1];
        if (source.has(file)) return new Response(source.get(file));
        if (file.startsWith('scripts/')) return new Response(templates[file.slice(8) as keyof typeof templates]);
        return new Response(await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url)));
      }
      if (path.includes('/releases/download/')) return new Response(contents.get(path.split('/').at(-1)!));
      if (path.includes('/git/trees/')) return Response.json({ truncated: false, tree: [...source].map(([path, bytes]) => ({ path, size: Buffer.byteLength(bytes), mode: '100644', type: 'blob' })) });
      if (path.endsWith(`/git/ref/tags/${tag}`)) return Response.json({ ref: `refs/tags/${tag}`, object: { type: 'tag', sha: tagObject } });
      if (path.endsWith(`/git/tags/${tagObject}`)) return Response.json({ sha: tagObject, tag, object: { type: 'commit', sha: commit } });
      if (path.endsWith('/releases')) return Response.json([{ id: 42, tag_name: tag, draft: false, prerelease: false, immutable: true, published_at: '2026-10-19T00:00:00Z', assets }]);
      if (path.endsWith('/runs')) return Response.json({ workflow_runs: path.includes('crates-publication') ? [] : [{ id: 77, run_attempt: 1, event: 'release', head_branch: tag, head_sha: commit, path: '.github/workflows/release-public-smoke.yml', status: 'completed', conclusion: 'success' }] });
      if (path.endsWith('/jobs')) return Response.json({ jobs: jobs.map((name, index) => ({ id: index + 1, run_id: 77, run_attempt: 1, head_sha: commit, name, status: 'completed', conclusion: 'success', started_at: '2026-10-20T00:00:00Z' })) });
      assert.fail(`unexpected request: ${path}`);
    } });
    let archiveCalls = 0, signatureCalls = 0;
    const verifier = async () => ({
      async inspectArchive() { archiveCalls++; },
      async verifySignature() { signatureCalls++; },
    });
    const first = await verifyPublicRelease(tag, { transport: makeTransport(), verifier });
    assert.equal(first.ok, true, JSON.stringify(first));
    assert.equal(archiveCalls, 6);
    assert.equal(signatureCalls, modern ? 1 : 0);
    assert.deepEqual(first.packages, [{ name: 'tapid', version, yanked: false }]);
    assert.equal(first.assets?.find(asset => asset.name.endsWith('.tar.gz'))?.digestEvidence, modern ? 'authenticated-record' : 'provider-checksum');
    const second = await verifyPublicRelease(tag, { transport: makeTransport(), verifier });
    assert.equal(JSON.stringify(first), JSON.stringify(second));
  } finally { await rm(directory, { recursive: true, force: true }); }
});
