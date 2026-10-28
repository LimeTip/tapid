import { test } from 'node:test';
import { deepStrictEqual, rejects, ok } from 'node:assert';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { buildSkill, verifySkill, type SkillIdentity } from './skill.ts';

const root = new URL('../../', import.meta.url);
const source = async (path: string) => readFile(new URL(path, root), 'utf8');
const identity: SkillIdentity = { tag: 'v0.0.13', commit: 'a'.repeat(40), cliVersion: '0.0.13', schemaVersion: 1 };

test('portable skill regenerates deterministically and extracts outside checkout', async () => {
  const bytes = await buildSkill(source, identity);
  deepStrictEqual(bytes, await buildSkill(source, identity));
  await verifySkill(bytes, source, identity);
  const directory = await mkdtemp(join(tmpdir(), 'tapid-skill-'));
  try {
    const { writeFile } = await import('node:fs/promises');
    await writeFile(join(directory, 'skill.zip'), bytes);
    await promisify(execFile)(process.platform === 'win32' ? 'tar' : 'unzip',
      process.platform === 'win32' ? ['-xf', 'skill.zip'] : ['-q', 'skill.zip'], { cwd: directory });
    const manifest = JSON.parse(await readFile(join(directory, 'tapid/manifest.json'), 'utf8'));
    deepStrictEqual(manifest.supported_cli_versions, ['0.0.13']);
    ok((await readFile(join(directory, 'tapid/references/agent-guide.md'), 'utf8')).includes('json-results.md'));
    ok((await readFile(join(directory, 'tapid/SKILL.md'), 'utf8')).includes('references/agent-guide.md'));
    ok((await readFile(join(directory, 'tapid/references/compatibility.md'), 'utf8')).includes(`/blob/${identity.commit}/`));
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('verification rejects altered bytes, extra data and changed maintained references', async () => {
  const bytes = await buildSkill(source, identity);
  const altered = Buffer.from(bytes); altered[100] ^= 1;
  await rejects(verifySkill(altered, source, identity), /source regeneration/);
  await rejects(verifySkill(Buffer.concat([bytes, Buffer.from('extra')]), source, identity), /source regeneration/);
  await rejects(verifySkill(bytes, async path => (await source(path)) + (path === 'docs/agent-guide.md' ? '\nchanged\n' : ''), identity), /source regeneration/);
});

test('metadata and source compatibility fail closed', async () => {
  for (const invalid of [{ ...identity, tag: 'v0.0.14' }, { ...identity, commit: 'main' }, { ...identity, schemaVersion: 2 }]) {
    await rejects(buildSkill(source, invalid));
  }
  await rejects(buildSkill(async path => path === 'skills/tapid/SKILL.md' ? '---\nname: other\n---\n' : source(path), identity), /frontmatter/);
  await rejects(buildSkill(async path => path === 'docs/agent-guide.md' ? 'broken [link](missing.md#absent)' : source(path), identity));
  await rejects(buildSkill(async path => path === 'skills/tapid/SKILL.md' ? (await source(path)).replace('references/agent-guide.md', 'references/absent.md') : source(path), identity), /missing bundled/);
});

// Historical releases must retain their original inventories.
import { expectedAssets } from './candidate.ts';
test('next releases add one documentation ZIP without changing binary inventory', () => {
  deepStrictEqual(expectedAssets('v0.0.13').filter(name => name.endsWith('.zip')), []);
  deepStrictEqual(expectedAssets('v0.0.14').filter(name => name.endsWith('.zip')), ['tapid-skill-0.0.14.zip']);
  ok(expectedAssets('v0.0.14').filter(name => name.endsWith('.tar.gz')).length === 6);
});

import { writeFile } from 'node:fs/promises';
import { TARGETS, checksumLines, releaseRecord } from './release.ts';
import { renderInstallers } from './bootstrap.ts';
import { attestUnsignedCandidate, validateUnsignedFiles, attestCandidate, publishCandidate, type ReleaseAdapter, type ReleaseState } from './candidate.ts';

test('new candidate attestation binds documentation to reviewed source, not its own manifest', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-skill-candidate-'));
  const next = { ...identity, tag: 'v0.0.14', cliVersion: '0.0.14' };
  // Synthetic future-release inputs are test fixtures, not compatibility evidence.
  const read = async (path: string) => (await source(path)).replaceAll('0.0.13', '0.0.14');
  const notes = join(directory, 'notes'), plan = join(directory, 'plan');
  const assets = join(directory, 'assets');
  const { mkdir } = await import('node:fs/promises');
  await mkdir(assets);
  try {
    const base = `https://github.com/LimeTip/tapid/releases/download/${next.tag}`;
    for (const target of TARGETS) await writeFile(join(assets, `tapid-0.0.14-${target}.tar.gz`), target);
    await writeFile(join(assets, 'SHA256SUMS'), await checksumLines(assets, '0.0.14'));
    await writeFile(join(assets, 'tapid-release-v1.tsv'), await releaseRecord(assets, '0.0.14', base));
    await renderInstallers(assets, '0.0.14', base);
    await writeFile(notes, 'reviewed notes'); await writeFile(plan, 'reviewed plan');
    const name = join(assets, 'tapid-skill-0.0.14.zip');
    const bytes = await buildSkill(read, next);
    await writeFile(name, bytes);
    const candidate = await attestUnsignedCandidate(assets, notes, plan, 'LimeTip/tapid', next.tag, next.commit, 'b'.repeat(40), undefined, read);
    deepStrictEqual(candidate.assets.length, 11);
    await validateUnsignedFiles(assets, notes, plan, candidate, undefined, read);
    await writeFile(join(assets, 'tapid-release-v1.tsv.sig'), 'fixture signature; native verification is a separate gate');
    const state: ReleaseState = { id: 1, tag_name: next.tag, name: next.tag, body: 'reviewed notes', draft: true, prerelease: false,
      assets: await Promise.all(expectedAssets(next.tag).map(async (name, i) => ({ id: i + 1, name, size: (await readFile(join(assets, name))).length }))) };
    const api: ReleaseAdapter = { repository: 'LimeTip/tapid', tag: async () => ({ object: 'b'.repeat(40), commit: next.commit }),
      isOnMain: async () => true, createTag: async () => { throw Error('unexpected tag creation'); },
      releases: async () => [state], release: async () => state, latest: async () => null,
      download: async (asset, destination) => writeFile(destination, await readFile(join(assets, asset.name))),
      promote: async () => { state.draft = false; state.immutable = true; },
      createRelease: async () => { throw Error('unexpected release creation'); }, upload: async () => { throw Error('unexpected upload'); } };
    const signed = await attestCandidate(api, assets, notes, plan, next.tag, next.commit, 1, undefined, read);
    deepStrictEqual(signed.assets.length, 12);
    deepStrictEqual(await publishCandidate(api, signed, notes, plan, undefined, read), { release_id: 1, tag: next.tag, published: true });
    // A fully internally consistent replacement cannot substitute for reviewed source.
    const changed = async (path: string) => (await read(path)) + (path === 'docs/agent-guide.md' ? '\nchanged guidance\n' : '');
    await writeFile(name, await buildSkill(changed, next));
    await rejects(attestUnsignedCandidate(assets, notes, plan, 'LimeTip/tapid', next.tag, next.commit, 'b'.repeat(40), undefined, read), /source regeneration/);
    await writeFile(name, Buffer.alloc(2 * 1024 * 1024 + 1));
    await rejects(attestUnsignedCandidate(assets, notes, plan, 'LimeTip/tapid', next.tag, next.commit, 'b'.repeat(40), undefined, read), /invalid regular file/);
    await rm(name);
    await rejects(attestUnsignedCandidate(assets, notes, plan, 'LimeTip/tapid', next.tag, next.commit, 'b'.repeat(40), undefined, read), /exact expected/);
    await writeFile(name, bytes); await writeFile(join(assets, 'extra.zip'), bytes);
    await rejects(attestUnsignedCandidate(assets, notes, plan, 'LimeTip/tapid', next.tag, next.commit, 'b'.repeat(40), undefined, read), /exact expected/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('compatibility review is separate from the source package version', async () => {
  await rejects(buildSkill(async path => path === 'skills/tapid/compatibility.json' ?
    (await source(path)).replace('0.0.13', '0.0.12') : source(path), identity), /unreviewed CLI compatibility/);
});

test('malformed plain-scalar frontmatter and stale Cargo comments cannot certify a bundle', async () => {
  await rejects(buildSkill(async path => path === 'skills/tapid/SKILL.md' ?
    (await source(path)).replace(/^description: .+$/m, 'description: [unterminated') : source(path), identity), /frontmatter/);
  await rejects(buildSkill(async path => path === 'crates/tapid-cli/Cargo.toml' ?
    '[package]\nname = "tapid"\nversion = "0.0.12"\n# version = "0.0.13"\n' : source(path), identity), /source version/);
});


test('unexpected archive entries and executable or symlink modes fail regeneration', async () => {
  const bytes = await buildSkill(source, identity);
  const central = bytes.readUInt32LE(bytes.length - 22 + 16);
  for (const mode of [0o120777, 0o100755]) {
    const changed = Buffer.from(bytes);
    changed.writeUInt32LE((mode * 65536) >>> 0, central + 38);
    await rejects(verifySkill(changed, source, identity), /source regeneration/);
  }
  const traversal = Buffer.from(bytes);
  traversal.write('../bad', 30);
  await rejects(verifySkill(traversal, source, identity), /source regeneration/);
});
