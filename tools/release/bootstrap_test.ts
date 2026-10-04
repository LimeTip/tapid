import { rejects } from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { renderInstallers } from './bootstrap.ts';

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
  match(workflow, /release\/install.sh release\/install.ps1; do/);
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
