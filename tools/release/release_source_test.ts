import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { execFile } from 'node:child_process';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
const exec = promisify(execFile);
const workflowPath = new URL('../../.github/workflows/release-publication.yml', import.meta.url);
const verifier = fileURLToPath(new URL('./verify-source.sh', import.meta.url));

test('candidate jobs start from trusted main and validate before executing selected source', async () => {
  const workflow = await readFile(workflowPath, 'utf8');
  const prepare = workflow.slice(workflow.indexOf('\n  prepare:'), workflow.indexOf('\n  build:'));
  const build = workflow.slice(workflow.indexOf('\n  build:'), workflow.indexOf('\n  assemble:'));
  assert.match(prepare, /if: github.ref == 'refs\/heads\/main'/);
  for (const job of [prepare, build]) {
    assert.match(job, /ref: \$\{\{ github.sha \}\}/);
    assert.doesNotMatch(job, /ref: \$\{\{ inputs.commit_sha \}\}/);
    assert.match(job, /verify-source.sh/);
    assert.match(job, /persist-credentials: false/);
    assert.match(job, /fetch-depth: 0/);
  }
  assert(prepare.indexOf('verify-source.sh') < prepare.indexOf('tools/release/release.ts'));
  assert(build.indexOf('verify-source.sh') < build.indexOf('git checkout --detach'));
  assert.match(prepare, /tag_commit: \$\{\{ steps.version.outputs.tag_commit \}\}/);
});

test('source verification permits reviewed historical commits and rejects arbitrary or changed refs', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-release-source-'));
  const remote = join(directory, 'remote');
  const checkout = join(directory, 'checkout');
  await mkdir(remote);
  const git = async (cwd: string, ...args: string[]) => (await exec('git', args, { cwd })).stdout.trim();
  try {
    await git(remote, 'init', '-b', 'main');
    await git(remote, 'config', 'user.name', 'Fixture');
    await git(remote, 'config', 'user.email', 'fixture@example.test');
    await writeFile(join(remote, 'source'), 'reviewed release');
    await git(remote, 'add', '.');
    await git(remote, 'commit', '-m', 'reviewed source');
    const commit = await git(remote, 'rev-parse', 'HEAD');
    await git(remote, 'tag', '-a', 'v0.0.12', '-m', 'reviewed release');
    const object = await git(remote, 'rev-parse', 'v0.0.12');
    await writeFile(join(remote, 'source'), 'later main');
    await git(remote, 'commit', '-am', 'advance main');
    await git(remote, 'checkout', '-b', 'unreviewed', commit);
    await writeFile(join(remote, 'source'), 'unreviewed code');
    await git(remote, 'commit', '-am', 'unreviewed change');
    const unreviewed = await git(remote, 'rev-parse', 'HEAD');
    await git(remote, 'tag', '-a', 'v0.0.13', '-m', 'unreviewed tag');
    const unreviewedObject = await git(remote, 'rev-parse', 'v0.0.13');
    await git(remote, 'tag', 'v0.0.14', commit);
    await git(remote, 'tag', '-a', 'v0.0.15', 'v0.0.12', '-m', 'nested tag');
    const nestedObject = await git(remote, 'rev-parse', 'v0.0.15');
    await git(remote, 'checkout', 'main');
    await git(directory, 'clone', '--no-tags', remote, checkout);
    const before = await git(checkout, 'rev-parse', 'HEAD');
    const verify = (tag = 'v0.0.12', sha = commit, tagObject = object, ref = 'refs/heads/main') => exec('bash', [verifier, tag, sha, tagObject], { cwd: checkout, env: { ...process.env, GITHUB_REF: ref } });
    await assert.rejects(git(checkout, 'rev-parse', '--verify', 'refs/tags/v0.0.12'));
    assert.equal((await verify()).stdout.trim(), commit);
    assert.equal(await git(checkout, 'rev-parse', 'refs/tags/v0.0.12'), object);
    await assert.rejects(verify('v0.0.13', unreviewed, unreviewedObject));
    await assert.rejects(verify('v0.0.14', commit, commit));
    await assert.rejects(verify('v0.0.15', commit, nestedObject));
    await assert.rejects(verify('v0.0.12', unreviewed));
    await assert.rejects(verify('v0.0.12', commit, 'f'.repeat(40)));
    await assert.rejects(verify('v0.0.12', commit, object, 'refs/heads/unreviewed'));
    await assert.rejects(verify('v0.0.12', 'main'));
    await assert.rejects(verify('--upload-pack=bad'));
    await git(remote, 'tag', '-fa', 'v0.0.12', unreviewed, '-m', 'moved tag');
    await assert.rejects(verify());
    assert.equal(await git(checkout, 'rev-parse', 'HEAD'), before, 'validation must not check out candidate code');
  } finally { await rm(directory, { recursive: true, force: true }); }
});
