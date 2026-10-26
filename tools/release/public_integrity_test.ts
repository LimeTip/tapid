import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { TARGETS, releaseRecord } from './release.ts';
import { renderInstallers } from './bootstrap.ts';
import { PublicTransport } from './public_transport.ts';
import { verifyPublicIntegrity } from './public_integrity.ts';

const commit = 'b'.repeat(40), tag = 'v0.0.11';
const base = `https://github.com/LimeTip/tapid/releases/download/${tag}`;
test('authenticates modern record before comparing exact rows, owned routes and tagged installers', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-integrity-'));
  try {
    for (const target of TARGETS) await writeFile(join(directory, `tapid-0.0.11-${target}.tar.gz`), 'fixture');
    const templates = {
      'install.sh': "@TAPID_BOOTSTRAP_VERSION@\n@TAPID_BOOTSTRAP_BASE_URL@\n# @TAPID_BOOTSTRAP_PINS@\n",
      'install.ps1': "@TAPID_BOOTSTRAP_VERSION@\n@TAPID_BOOTSTRAP_BASE_URL@\n# @TAPID_BOOTSTRAP_PINS@\n",
    };
    const record = await releaseRecord(directory, '0.0.11', base);
    await writeFile(join(directory, 'tapid-release-v1.tsv'), record);
    await writeFile(join(directory, 'tapid-release-v1.tsv.sig'), 'signed fixture');
    await renderInstallers(directory, '0.0.11', base, templates);
    let authenticated = false;
    const verifier = { async inspectArchive() {}, async verifySignature() { authenticated = true; } };
    const transport = (tamper = false) => new PublicTransport({ fetch: async input => {
      assert.equal(authenticated, true, 'authenticate local assets before public correspondence reads');
      const url = new URL(String(input));
      if (url.hostname === 'tapid.dev') return new Response(url.pathname.endsWith('.sig') ? 'signed fixture' : tamper ? 'other record' : record);
      assert.equal(url.hostname, 'raw.githubusercontent.com');
      assert.ok(url.pathname.startsWith(`/LimeTip/tapid/${commit}/scripts/`));
      return new Response(templates[url.pathname.endsWith('.sh') ? 'install.sh' : 'install.ps1']);
    } });
    await verifyPublicIntegrity(transport(), verifier, { directory, tag, commit });
    await assert.rejects(verifyPublicIntegrity(transport(true), verifier, { directory, tag, commit }), /owned release metadata differs/);
    await writeFile(join(directory, 'install.sh'), 'tampered');
    await assert.rejects(verifyPublicIntegrity(transport(), verifier, { directory, tag, commit }), /generated installer differs/);
    assert.equal(await readFile(join(directory, 'install.sh'), 'utf8'), 'tampered');
    await writeFile(join(directory, 'tapid-release-v1.tsv'), record.replace('0.0.11', '0.0.12'));
    await assert.rejects(verifyPublicIntegrity(transport(), verifier, { directory, tag, commit }), /release record differs/);
    await assert.rejects(verifyPublicIntegrity(transport(), { ...verifier, async verifySignature() { throw Error('invalid signature'); } }, { directory, tag, commit }), /invalid signature/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
