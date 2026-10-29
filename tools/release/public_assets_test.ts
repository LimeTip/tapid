import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readdir } from 'node:fs/promises';
import test from 'node:test';
import { TARGETS } from './release.ts';
import { verifyPublicAssets } from './public_assets.ts';
import { PublicTransport } from './public_transport.ts';

const tag = 'v0.0.10';
const archiveNames = TARGETS.map(target => `tapid-0.0.10-${target}.tar.gz`);
const hash = createHash('sha256').update('archive fixture').digest('hex');
function fixture(change: (assets: { id: number; name: string; size: number; browser_download_url: string }[]) => void = () => {}, checksumHash = hash) {
  const checksums = archiveNames.map(name => `${checksumHash}  ${name}\n`).join('');
  const assets = [...archiveNames, 'SHA256SUMS'].map((name, i) => ({ id: i + 1, name, size: name === 'SHA256SUMS' ? Buffer.byteLength(checksums) : 15, browser_download_url: `https://github.com/LimeTip/tapid/releases/download/${tag}/${name}` }));
  change(assets);
  return { assets, transport: new PublicTransport({ fetch: async input => new Response(String(input).endsWith('/SHA256SUMS') ? checksums : 'archive fixture') }) };
}

test('downloads the exact historical contract and compares its checksum set in temporary storage', async () => {
  const { assets, transport } = fixture();
  let directory = '';
  const result = await verifyPublicAssets(transport, tag, assets, async evidence => {
    directory = evidence.directory;
    assert.deepEqual((await readdir(directory)).sort(), ['SHA256SUMS', ...archiveNames].sort());
  });
  assert.equal(result.length, 7);
  assert.equal(result.find(asset => asset.name === archiveNames[0])?.sha256, hash);
  await assert.rejects(readdir(directory), { code: 'ENOENT' });
});

test('rejects missing, extra, duplicate, oversized, and incorrectly routed assets before downloading', async () => {
  for (const change of [
    (assets: any[]) => { assets.pop(); },
    (assets: any[]) => { assets.push({ ...assets[0], id: 99, name: 'unexpected' }); },
    (assets: any[]) => { assets[1] = { ...assets[0] }; },
    (assets: any[]) => { assets[0].size = 512 * 1024 * 1024 + 1; },
    (assets: any[]) => { assets[0].browser_download_url = 'https://evil.example/archive'; },
  ]) {
    const { assets, transport } = fixture(change);
    await assert.rejects(verifyPublicAssets(transport, tag, assets, async () => assert.fail('inspection must not run')));
  }
});

test('rejects corrupt checksums and provider size mismatches', async () => {
  const corrupt = fixture(() => {}, '0'.repeat(64));
  await assert.rejects(verifyPublicAssets(corrupt.transport, tag, corrupt.assets, async () => assert.fail('inspection must not run')), /SHA256SUMS mismatch/);
  const truncated = fixture(assets => { assets[0].size++; });
  await assert.rejects(verifyPublicAssets(truncated.transport, tag, truncated.assets, async () => assert.fail('inspection must not run')), /provider asset size/);
});

test('cleans temporary bytes when offline inspection fails', async () => {
  const { assets, transport } = fixture();
  let directory = '';
  await assert.rejects(verifyPublicAssets(transport, tag, assets, async evidence => {
    directory = evidence.directory;
    throw Error('native verification rejected fixture');
  }), /native verification rejected/);
  await assert.rejects(readdir(directory), { code: 'ENOENT' });
});

test('modern asset selection requires all eleven names without historical fallback', async () => {
  const modernTag = 'v0.0.11';
  const modernArchives = archiveNames.map(name => name.replace('0.0.10', '0.0.11'));
  const checksums = modernArchives.map(name => `${hash}  ${name}\n`).join('');
  const metadata = ['tapid-release-v1.tsv', 'tapid-release-v1.tsv.sig', 'install.sh', 'install.ps1'];
  const assets = [...modernArchives, 'SHA256SUMS', ...metadata].map((name, i) => ({ id: i + 1, name, size: name === 'SHA256SUMS' ? Buffer.byteLength(checksums) : metadata.includes(name) ? 7 : 15, browser_download_url: `https://github.com/LimeTip/tapid/releases/download/${modernTag}/${name}` }));
  const transport = new PublicTransport({ fetch: async input => {
    const name = new URL(String(input)).pathname.split('/').at(-1)!;
    return new Response(name === 'SHA256SUMS' ? checksums : metadata.includes(name) ? 'fixture' : 'archive fixture');
  } });
  const observed = await verifyPublicAssets(transport, modernTag, assets, async () => {});
  assert.equal(observed.length, 11);
  assert.equal(observed.find(asset => asset.name === 'tapid-release-v1.tsv')?.digestEvidence, 'observed');
  await assert.rejects(verifyPublicAssets(transport, modernTag, assets.filter(asset => !metadata.includes(asset.name)), async () => {}), /asset set/);
});
