import assert from 'node:assert/strict';
import test from 'node:test';
import { access } from 'node:fs/promises';
import { withPublicSource } from './public_source.ts';
import { PublicTransport } from './public_transport.ts';

const commit = 'b'.repeat(40);
function fixture(entries: unknown[]) {
  return new PublicTransport({ fetch: async input => {
    const url = new URL(String(input));
    if (url.hostname === 'api.github.com') return Response.json({ truncated: false, tree: entries });
    const path = url.pathname.split(`/${commit}/`)[1];
    if (path === 'Cargo.toml') return new Response('[package]\nname="tapid"\nversion="0.0.11"\nedition="2024"\n');
    if (path === 'Cargo.lock') return new Response('version = 4\n[[package]]\nname = "tapid"\nversion = "0.0.11"\n');
    if (path === 'src/main.rs') return new Response('fn main() {}\n');
    if (path === 'build.rs') return new Response('compile_error!("tagged code must never compile");\n');
    assert.fail(`unexpected source read: ${path}`);
  } });
}
const files = [
  { path: 'Cargo.toml', type: 'blob', mode: '100644', size: 55 },
  { path: 'Cargo.lock', type: 'blob', mode: '100644', size: 58 },
  { path: 'src/main.rs', type: 'blob', mode: '100644', size: 13 },
  { path: 'build.rs', type: 'blob', mode: '100644', size: 50 },
];
test('materializes commit-addressed Cargo source and runs offline metadata without tagged scripts', async () => {
  let source = '';
  await withPublicSource(fixture(files), commit, async (metadata, directory) => {
    source = directory;
    assert.equal(metadata.packages.length, 1);
    assert.equal(metadata.packages[0].name, 'tapid');
    assert.equal(metadata.packages[0].version, '0.0.11');
  });
  await assert.rejects(access(source), { code: 'ENOENT' });
});
test('rejects source traversal, duplicate manifests, links and incomplete discovery before metadata', async () => {
  for (const bad of [
    [...files, { ...files[0], path: '../Cargo.toml' }],
    [...files, files[0]],
    files.map(file => ({ ...file, mode: '120000' })),
    [],
  ]) await assert.rejects(withPublicSource(fixture(bad), commit, async () => assert.fail('no callback')));
});

test('rejects truncated and over-limit source evidence and accounts retried source bodies', async () => {
  for (const response of [
    { truncated: true, tree: files },
    { truncated: false, tree: Array(100_001).fill(files[0]) },
    { truncated: false, tree: files.map(file => ({ ...file, size: 2 * 1024 * 1024 + 1 })) },
    { truncated: false, tree: [...files, ...Array.from({ length: 513 }, (_, index) => ({ path: `file${index}.rs`, type: 'blob', mode: '100644', size: 2 * 1024 * 1024 }))] },
  ]) {
    const transport = new PublicTransport({ fetch: async () => Response.json(response) });
    await assert.rejects(withPublicSource(transport, commit, async () => assert.fail('no metadata callback')));
  }
  let calls = 0;
  const transport = new PublicTransport({ sourceTransferBudgetBytes: 5, delay: async () => {}, fetch: async () => {
    calls++;
    return new Response('123', { status: 503 });
  } });
  await assert.rejects(transport.bytes(`https://raw.githubusercontent.com/LimeTip/tapid/${commit}/Cargo.toml`, 10), /source transfer limit/);
  assert.equal(calls, 2);
});
