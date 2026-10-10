import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { execFile } from 'node:child_process';
import { chmod, cp, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
const exec = promisify(execFile);

test('actual source preflight accepts a new release without a tag and rejects version drift', async () => {
  const directory = await realpath(await mkdtemp(join(tmpdir(), 'tapid-preflight-')));
  const remote = join(directory, 'remote'), checkout = join(directory, 'checkout');
  await mkdir(remote);
  const git = async (cwd: string, ...args: string[]) => (await exec('git', args, { cwd, env: { PATH: process.env.PATH, HOME: directory, GIT_CONFIG_NOSYSTEM: '1' } })).stdout.trim();
  try {
    await git(remote, 'init', '-b', 'main');
    await git(remote, 'config', 'user.name', 'Fixture');
    await git(remote, 'config', 'user.email', 'fixture@example.test');
    await mkdir(join(remote, 'src'));
    await writeFile(join(remote, 'src/main.rs'), 'fn main() {}\n');
    await writeFile(join(remote, 'Cargo.toml'), '[package]\nname = "tapid"\nversion = "0.0.12"\nedition = "2021"\n');
    await writeFile(join(remote, 'Cargo.lock'), 'version = 3\n\n[[package]]\nname = "tapid"\nversion = "0.0.12"\n');
    await mkdir(join(remote, 'tools'));
    await cp(fileURLToPath(new URL('.', import.meta.url)), join(remote, 'tools/release'), { recursive: true });
    await mkdir(join(remote, 'docs/examples'), { recursive: true });
    // This release is an isolated fixture, independent of the real release inventory.
    await writeFile(join(remote, 'docs/examples/contracts.json'), JSON.stringify({
      schema_version: 1,
      examples: ['upgrade', 'upgrade-help'].map(id => ({ id, release_expectations: { 'v0.0.12': { exit_code: 0 } } })),
      capabilities: [{ id: 'self-upgrade', expected_releases: ['v0.0.12'] }],
    }));
    await git(remote, 'add', '.');
    await git(remote, 'commit', '-m', 'baseline');
    const base = await git(remote, 'rev-parse', 'HEAD');
    await mkdir(join(remote, 'docs/releases'), { recursive: true });
    const intent = { schema: 'tapid-release-intent-v1', version: '0.0.12', baseline: 'v0.0.11', prepared_from: base, notes: 'docs/releases/0.0.12.md', packages: [{ name: 'tapid', version: '0.0.12' }] };
    await writeFile(join(remote, intent.notes), 'Reviewed notes\n');
    await writeFile(join(remote, 'docs/releases/intent.json'), JSON.stringify(intent));
    await git(remote, 'add', '.');
    await git(remote, 'commit', '-m', 'prepare release');
    const source = await git(remote, 'rev-parse', 'HEAD');
    const bin = join(directory, 'bin');
    await mkdir(bin);
    // Offline Cargo metadata fixture; all Git resolution and release tooling are real.
    await writeFile(join(bin, 'cargo'), `#!/usr/bin/env node
const fs = require('node:fs');
const assert = require('node:assert/strict');
assert.deepEqual(process.argv.slice(2), ['metadata', '--no-deps', '--format-version', '1', '--locked']);
const version = /^version = "([^"]+)"$/m.exec(fs.readFileSync('Cargo.toml', 'utf8'))[1];
console.log(JSON.stringify({packages: [{name: 'tapid', version}]}));
`);
    await chmod(join(bin, 'cargo'), 0o755);
    await git(directory, 'clone', remote, checkout);
    const workflow = await readFile(new URL('../../.github/workflows/crates-publication.yml', import.meta.url), 'utf8');
    const block = workflow.split('      - name: Resolve reviewed main source and release intent\n')[1].split('      - name: Wait for successful CI')[0];
    const script = block.split('        run: |\n')[1].split('\n').map(line => line.replace(/^ {10}/, '')).join('\n');
    const check = async (label: string, requestedTag = '', sha = source) => {
      const runner = join(directory, label);
      await mkdir(runner);
      const output = join(runner, 'outputs');
      await exec('bash', ['-c', script], { cwd: checkout, env: { PATH: `${bin}:${process.env.PATH}`, HOME: directory, CARGO_HOME: join(directory, 'cargo-home'), CARGO_NET_OFFLINE: 'true', GITHUB_SHA: sha, REQUESTED_TAG: requestedTag, GITHUB_WORKSPACE: checkout, GITHUB_OUTPUT: output, RUNNER_TEMP: runner } });
      return readFile(output, 'utf8');
    };
    await assert.rejects(git(checkout, 'rev-parse', '--verify', 'refs/tags/v0.0.12'));
    assert.match(await check('new-release'), /tag=v0.0.12\nversion=0.0.12/);
    await assert.rejects(git(checkout, 'rev-parse', '--verify', 'refs/tags/v0.0.12'), 'preflight must not create a tag');
    await git(remote, 'tag', '-a', 'v0.0.12', source, '-m', 'release');
    assert.match(await check('recovery', 'v0.0.12'), /version=0.0.12/);
    await git(remote, 'checkout', '-b', 'missing-contract', source);
    const contractPath = join(remote, 'docs/examples/contracts.json');
    const contracts = JSON.parse(await readFile(contractPath, 'utf8'));
    delete contracts.examples.find(example => example.id === 'upgrade-help').release_expectations['v0.0.12'];
    await writeFile(contractPath, JSON.stringify(contracts));
    await writeFile(join(remote, 'docs/releases/intent.json'), JSON.stringify({ ...intent, prepared_from: source }));
    await git(remote, 'add', '.');
    await git(remote, 'commit', '-m', 'omit release smoke expectation');
    const missing = await git(remote, 'rev-parse', 'HEAD');
    await git(remote, 'branch', '-f', 'main', missing);
    await git(checkout, 'fetch', '--force', 'origin', 'refs/heads/main:refs/remotes/origin/main');
    await assert.rejects(check('missing-contract', '', missing), /upgrade-help.*v0.0.12/);
    await git(remote, 'checkout', '-b', 'drift', base);
    await mkdir(join(remote, 'docs/releases'), { recursive: true });
    await writeFile(join(remote, 'docs/releases/intent.json'), JSON.stringify({ ...intent, version: '0.0.13', notes: 'docs/releases/0.0.13.md', packages: [{ name: 'tapid', version: '0.0.13' }] }));
    await writeFile(join(remote, 'docs/releases/0.0.13.md'), 'Drifted notes\n');
    await git(remote, 'add', '.');
    await git(remote, 'commit', '-m', 'mismatched release version');
    const drifted = await git(remote, 'rev-parse', 'HEAD');
    await git(remote, 'branch', '-f', 'main', drifted);
    // The fixture intentionally advances main to a sibling, fetching it explicitly.
    await git(checkout, 'fetch', '--force', 'origin', 'refs/heads/main:refs/remotes/origin/main');
    await assert.rejects(check('drift', '', drifted), /source package version/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
