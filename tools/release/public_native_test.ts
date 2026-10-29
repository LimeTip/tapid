import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { buildPublicVerifier } from './public_native.ts';

// This crosses the actual source-build/native-command seam, using no downloaded code.
test('source-built native adapter rejects malformed and wrong-target archives without modifying bytes', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-native-adapter-'));
  try {
    const verifier = await buildPublicVerifier();
    const archive = join(directory, 'release.tar.gz');
    const bytes = Buffer.from('H4sIAAAAAAAAEytJLMhMYaAtMAACc1NTMA0E6DQQmCGxQeJm5uYGDAoGNHYXGJQWlyQWAa2kh12DECRl5iUWVQ60K0bBKBgFo2AU0BsAALAgbeUACAAA', 'base64');
    await writeFile(archive, bytes);
    await verifier.inspectArchive(archive, 'tapid');
    await assert.rejects(verifier.inspectArchive(archive, 'tapid.exe'), /native archive inspection failed/);
    assert.deepEqual(await readFile(archive), bytes);
    await writeFile(archive, 'malformed archive');
    await assert.rejects(verifier.inspectArchive(archive, 'tapid'), /native archive inspection failed/);
    const record = join(directory, 'record'), sidecar = join(directory, 'signature');
    await writeFile(record, 'unauthenticated');
    await writeFile(sidecar, '{}');
    await assert.rejects(verifier.verifySignature(record, sidecar), /native signature verification failed/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('native build honors the shared deadline without exposing child diagnostics', async () => {
  await assert.rejects(buildPublicVerifier(() => 1), { message: 'local native verifier build failed' });
});

test('native build bounds subprocess output and omits ambient tokens', { skip: process.platform === 'win32' ? 'Unix executable fixture; native Windows build remains covered above' : false }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-cargo-output-'));
  const previousPath = process.env.PATH, previousToken = process.env.CARGO_REGISTRY_TOKEN;
  try {
    const receipt = join(directory, 'receipt.json');
    await writeFile(join(directory, 'cargo'), `#!${process.execPath}\nrequire('node:fs').writeFileSync(${JSON.stringify(receipt)}, JSON.stringify({ tokenPresent: Boolean(process.env.CARGO_REGISTRY_TOKEN) }));\nprocess.stdout.write('x'.repeat(3 * 1024 * 1024));\nsetInterval(() => {}, 1000);\n`, { mode: 0o755 });
    process.env.PATH = `${directory}:${previousPath ?? ''}`;
    process.env.CARGO_REGISTRY_TOKEN = 'fixture-secret-never-forward';
    await assert.rejects(buildPublicVerifier(() => 10_000), { message: 'local native verifier build failed' });
    assert.deepEqual(JSON.parse(await readFile(receipt, 'utf8')), { tokenPresent: false });
  } finally {
    if (previousPath === undefined) delete process.env.PATH; else process.env.PATH = previousPath;
    if (previousToken === undefined) delete process.env.CARGO_REGISTRY_TOKEN; else process.env.CARGO_REGISTRY_TOKEN = previousToken;
    await rm(directory, { recursive: true, force: true });
  }
});
