import { rejects } from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { renderInstallers } from './bootstrap.ts';
import { checksumLines, releaseRecord } from './release.ts';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';

import { deepStrictEqual, strictEqual, match } from 'node:assert/strict';

import { test } from 'node:test';

test('binary installers use authenticated native verification without a language runtime', async () => {
  for (const name of ['install.sh', 'install.ps1']) {
    const source = await readFile(new URL(`../../scripts/${name}`, import.meta.url), 'utf8');
    strictEqual(/python|verify-release-record\.py/i.test(source), false);
    match(source, /__verify-release-record/);
    match(source, /bootstrap archive checksum mismatch/);
  }
});

test('release CI generates literal platform pins from exact archive bytes', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-bootstrap-generation-'));
  const targets = ['aarch64-apple-darwin', 'x86_64-apple-darwin', 'aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu', 'aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc'];
  try {
    for (const target of targets) await writeFile(join(directory, `tapid-1.2.3-${target}.tar.gz`), `archive ${target}`);
    await renderInstallers(directory, '1.2.3', 'https://github.com/LimeTip/tapid/releases/download/v1.2.3');
    for (const name of ['install.sh', 'install.ps1']) {
      const source = await readFile(join(directory, name), 'utf8');
      strictEqual(source.includes('@TAPID_BOOTSTRAP_'), false);
      for (const target of targets) {
        const digest = createHash('sha256').update(`archive ${target}`).digest('hex');
        strictEqual(source.includes(`${target}${name.endsWith('.sh') ? ") printf '%s\\n' '" : "' = '"}${digest}'`), true);
      }
    }
    await rejects(() => renderInstallers(directory, '1.2.3', 'https://example.test/$(touch-bad)'), /safe literal/);
    await rm(join(directory, 'tapid-1.2.3-x86_64-apple-darwin.tar.gz'));
    await rejects(() => renderInstallers(directory, '1.2.3', 'https://example.test/v1.2.3'), /exactly these archives/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});


test('publication and draft verification automate bootstrap pin generation and readback', async () => {
  const workflow = await readFile(new URL('../../.github/workflows/release-publication.yml', import.meta.url), 'utf8');
  const draft = await readFile(new URL('../../.github/workflows/release-draft-verify.yml', import.meta.url), 'utf8');
  strictEqual(workflow.indexOf('tools/release/bootstrap.ts release') < workflow.indexOf('name: Create draft GitHub release'), true);
  match(workflow, /bootstrap\.ts release [^\n]+ "\$SOURCE_SHA"/);
  match(workflow, /tools\/release\/candidate.ts unsigned release/);
  match(workflow, /tools\/release\/candidate.ts validate-unsigned release/);
  match(workflow, /tools\/release\/candidate.ts create-draft release/);
  match(draft, /tools\/release\/bootstrap.ts/);
  match(draft, /cmp "\$generated\/install.sh"/);
  match(draft, /cmp "\$generated\/install.ps1"/);
  match(draft, /exactly eleven draft assets/);
  match(draft, /__verify-release-record/);
});

test('draft verification grants repository write access only to the draft-reading job', async () => {
  const draft = await readFile(new URL('../../.github/workflows/release-draft-verify.yml', import.meta.url), 'utf8');
  match(draft, /^permissions:\n  contents: read\n/m);
  const jobs = [...draft.slice(draft.indexOf('\njobs:\n')).matchAll(/^  ([\w-]+):\n([\s\S]*?)(?=^  [\w-]+:|$(?![\s\S]))/gm)];
  strictEqual(jobs.some((job) => job[1] === 'install'), true);
  for (const [_, name, body] of jobs) {
    const permissions = body.match(/^    permissions:[^\n]*(?:\n {6}[^\n]*)*/m)?.[0];
    if (permissions !== undefined) {
      strictEqual(permissions, `    permissions:\n      contents: ${name === 'resolve-and-verify' ? 'write' : 'read'}`);
    }
  }
  const writeJobs = jobs.filter((job) => /^    permissions:\n      contents: write\n/m.test(job[2]));
  deepStrictEqual(writeJobs.map((job) => job[1]), ['resolve-and-verify']);
  strictEqual(draft.match(/^\s*contents: write\s*$/gm)?.length, 1);
});


test('recovery generation and attestation use the same tagged templates after main changes', async () => {
  const directory = await realpath(await mkdtemp(join(tmpdir(), 'tapid-bootstrap-recovery-')));
  const exec = promisify(execFile);
  const environment = { PATH: process.env.PATH, HOME: directory, GIT_CONFIG_NOSYSTEM: '1', GITHUB_REPOSITORY: 'LimeTip/tapid' };
  const git = async (...args: string[]) => (await exec('git', args, { cwd: directory, env: environment })).stdout.trim();
  try {
    await git('init', '-b', 'main');
    await git('config', 'user.name', 'Fixture');
    await git('config', 'user.email', 'fixture@example.test');
    await mkdir(join(directory, 'scripts'));
    await mkdir(join(directory, 'tools'));
    await cp(fileURLToPath(new URL('.', import.meta.url)), join(directory, 'tools/release'), { recursive: true });
    for (const name of ['install.sh', 'install.ps1']) {
      await writeFile(join(directory, 'scripts', name), '# tagged template\n' + await readFile(new URL(`../../scripts/${name}`, import.meta.url), 'utf8'));
    }
    await git('add', '.');
    await git('commit', '-m', 'tagged release source');
    const source = await git('rev-parse', 'HEAD');
    await git('tag', '-a', 'v1.2.3', '-m', 'release');
    const object = await git('rev-parse', 'v1.2.3');
    for (const name of ['install.sh', 'install.ps1']) {
      await writeFile(join(directory, 'scripts', name), '# current main template\n' + await readFile(new URL(`../../scripts/${name}`, import.meta.url), 'utf8'));
    }
    await git('commit', '-am', 'change main templates');
    const assets = join(directory, 'release');
    await mkdir(assets);
    const targets = ['aarch64-apple-darwin', 'x86_64-apple-darwin', 'aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu', 'aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc'];
    for (const target of targets) await writeFile(join(assets, `tapid-1.2.3-${target}.tar.gz`), `archive ${target}`);
    const base = 'https://github.com/LimeTip/tapid/releases/download/v1.2.3';
    await writeFile(join(assets, 'SHA256SUMS'), await checksumLines(assets, '1.2.3'));
    await writeFile(join(assets, 'tapid-release-v1.tsv'), await releaseRecord(assets, '1.2.3', base));
    const notes = join(directory, 'notes.md'), plan = join(directory, 'plan.json');
    await writeFile(notes, 'Reviewed release notes\n');
    await writeFile(plan, JSON.stringify({ packages: [{ name: 'tapid', version: '1.2.3' }] }));
    const node = (...args: string[]) => exec(process.execPath, ['--experimental-strip-types', ...args], { cwd: directory, env: environment });
    const bootstrap = join(directory, 'tools/release/bootstrap.ts');
    const attest = () => node(join(directory, 'tools/release/candidate.ts'), 'unsigned', assets, notes, plan, 'v1.2.3', source, object);
    // Current-main generation demonstrates the original recovery mismatch.
    await node(bootstrap, assets, '1.2.3', base);
    await rejects(attest(), /generated installer mismatch/);
    await node(bootstrap, assets, '1.2.3', base, source);
    for (const name of ['install.sh', 'install.ps1']) {
      strictEqual((await readFile(join(assets, name), 'utf8')).startsWith('# tagged template\n'), true);
    }
    strictEqual(JSON.parse((await attest()).stdout).candidate.assets.length, 10);
    await rejects(node(bootstrap, assets, '1.2.3', base, 'main'), /invalid template source commit/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
