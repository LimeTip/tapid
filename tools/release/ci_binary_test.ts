import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtemp, mkdir, writeFile, readFile, readdir, realpath, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { prepare, verify } from './ci_binary.ts';

const sha = 'a'.repeat(40);
const env = { GITHUB_REPOSITORY: 'owner/repo', GITHUB_SHA: sha, GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2', GITHUB_JOB: 'platform-consumer-validation', RUNNER_OS: 'Windows', RUNNER_ARCH: 'X64', ImageOS: 'win25', ImageVersion: '20261001.1', RUSTFLAGS: '-Dwarnings' };
const receipts: Record<string, string> = { git: sha, cargo: 'cargo 1.90.0 (840b83a10 2025-07-30)', rustc: 'rustc 1.90.0 (1159e78c4 2025-09-14)\nbinary: rustc\ncommit-hash: 1159e78c4747b02ef996e55082b704c09b970588\ncommit-date: 2025-09-14\nhost: x86_64-pc-windows-msvc\nrelease: 1.90.0\nLLVM version: 20.1.8', rustup: 'stable-x86_64-pc-windows-msvc (default)' };
const consumerContext = { env, readCommand: async (command: string) => receipts[command] };
const context = { ...consumerContext, env: { ...env, CARGO_INCREMENTAL: '0' } };
async function fixture(run: (root: string) => Promise<void>) {
  // Resolve platform temp aliases before the verifier's strict ancestor checks.
  const root = await realpath(await mkdtemp(join(tmpdir(), 'tapid-ci-binary-')));
  try {
    await mkdir(join(root, 'crates/tapid-cli'), { recursive: true });
    await writeFile(join(root, 'crates/tapid-cli/Cargo.toml'), '[package]\nname = "tapid"\nversion = "1.2.3"\n');
    const bytes = Buffer.alloc(256); bytes.write('MZ'); bytes.writeUInt32LE(128, 60); bytes.write('PE\0\0', 128); bytes.writeUInt16LE(0x8664, 132);
    await writeFile(join(root, 'source.exe'), bytes);
    await run(root);
  } finally { await rm(root, { recursive: true, force: true }); }
}

test('pinned Rust action incremental setting is recorded and toolchain-free consumer verifies', async () => fixture(async root => {
  const directory = join(root, 'handoff');
  await prepare(directory, join(root, 'source.exe'), { ...context, root, env: { ...env, CARGO_INCREMENTAL: '0' } });
  const provenance = JSON.parse(await readFile(join(directory, 'provenance.json'), 'utf8'));
  assert.equal(provenance.settings.CARGO_INCREMENTAL, '0', 'retain actual producer receipt');
  const readCommand = async (command: string) => { assert.equal(command, 'git', 'consumer must not discover Rust'); return sha; };
  assert.equal(await verify(directory, join(root, 'target/debug/tapid.exe'), { ...context, root, env: { ...env, CARGO_INCREMENTAL: undefined }, readCommand }), '1.2.3');
}));

test('Windows handoff prepares two files and verifies exact identity before materializing', async () => fixture(async root => {
  const directory = join(root, 'handoff');
  await prepare(directory, join(root, 'source.exe'), { ...context, root });
  assert.deepEqual((await readdir(directory)).sort(), ['provenance.json', 'tapid.exe']);
  const destination = join(root, 'target/debug/tapid.exe');
  assert.equal(await verify(directory, destination, { ...consumerContext, root }), '1.2.3');
  assert.deepEqual(await readFile(destination), await readFile(join(root, 'source.exe')));
}));
for (const [name, badEnv] of Object.entries({ 'incremental zero': { CARGO_INCREMENTAL: '0' }, 'incremental enabled': { CARGO_INCREMENTAL: '1' }, 'encoded flags': { CARGO_ENCODED_RUSTFLAGS: '' }, 'target directory': { CARGO_TARGET_DIR: 'other' }, 'target override': { CARGO_BUILD_TARGET: 'x86_64-pc-windows-msvc' }, 'wrong flags': { RUSTFLAGS: '' } })) test(`consumer rejects ${name} before destination effects`, async () => fixture(async root => {
  const directory = join(root, 'handoff');
  await prepare(directory, join(root, 'source.exe'), { ...context, root });
  for (const existing of [false, true]) {
    const destination = join(root, existing ? 'existing.exe' : 'absent/target.exe');
    if (existing) await writeFile(destination, 'unchanged');
    await assert.rejects(() => verify(directory, destination, { ...consumerContext, root, env: { ...env, ...badEnv } }), /unexpected build flags\/target settings/);
    if (existing) assert.equal(await readFile(destination, 'utf8'), 'unchanged');
    else await assert.rejects(() => readdir(join(root, 'absent')), { code: 'ENOENT' });
  }
}));
for (const mode of ['prepare', 'verify']) test(`${mode} rejects high-bit DOS magic before destination effects`, async () => fixture(async root => {
  const directory = join(root, 'handoff');
  if (mode === 'verify') await prepare(directory, join(root, 'source.exe'), { ...context, root });
  const binary = join(root, mode === 'verify' ? 'handoff/tapid.exe' : 'source.exe');
  const bytes = await readFile(binary); bytes[0] = 0xcd; bytes[1] = 0xda;
  await writeFile(binary, bytes);
  if (mode === 'prepare') {
    await assert.rejects(() => prepare(directory, binary, { ...context, root }), /invalid PE DOS header/);
    await assert.rejects(() => readdir(directory), { code: 'ENOENT' });
  } else {
    const path = join(directory, 'provenance.json'); const p = JSON.parse(await readFile(path, 'utf8'));
    p.sha256 = (await import('node:crypto')).createHash('sha256').update(bytes).digest('hex');
    await writeFile(path, JSON.stringify(p));
    for (const existing of [false, true]) {
      const destination = join(root, existing ? 'existing.exe' : 'absent/target.exe');
      if (existing) await writeFile(destination, 'unchanged');
      await assert.rejects(() => verify(directory, destination, { ...consumerContext, root }), /invalid PE DOS header/);
      if (existing) assert.equal(await readFile(destination, 'utf8'), 'unchanged');
      else await assert.rejects(() => readdir(join(root, 'absent')), { code: 'ENOENT' });
    }
  }
}));
const mutations: Record<string, (p: any) => void> = {
  schema: p => p.schema = 2, 'extra field': p => p.extra = true,
  repository: p => p.repository = 'other/repo', source: p => p.sourceSha = 'b'.repeat(40),
  'PR head for merge': p => { p.sourceSha = 'b'.repeat(40); p.eventSha = p.sourceSha; },
  event: p => p.eventSha = 'b'.repeat(40), run: p => p.runId = '124', attempt: p => p.runAttempt = '1',
  job: p => p.producerJob = 'test', leg: p => p.producerLeg = 'ubuntu-latest',
  OS: p => p.runnerOS = 'Linux', arch: p => p.runnerArch = 'ARM64',
  imageOS: p => p.imageOS = 'win22', imageVersion: p => p.imageVersion = 'stale',
  target: p => p.target = 'aarch64-pc-windows-msvc', profile: p => p.profile = 'release',
  features: p => p.features = 'all', argv: p => p.argv.push('--all-features'), flags: p => p.rustflags = '',
  settings: p => p.settings.CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc',
  'incremental enabled': p => p.settings.CARGO_INCREMENTAL = '1',
  'incremental absent': p => p.settings.CARGO_INCREMENTAL = null,
  'incremental missing': p => delete p.settings.CARGO_INCREMENTAL,
  'extra setting': p => p.settings.EXTRA = null,
  'encoded flags': p => p.settings.CARGO_ENCODED_RUSTFLAGS = '',
  'target directory': p => p.settings.CARGO_TARGET_DIR = 'other',
  version: p => p.version = '1.2.4', size: p => p.size++, digest: p => p.sha256 = '0'.repeat(64),
  cargo: p => p.cargo = '', nightly: p => p.rustc = p.rustc.replaceAll('1.90.0', '1.90.0-nightly'),
  host: p => p.rustc = p.rustc.replace('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'),
  rustc: p => p.rustc = 'rustc 1.90.0', toolchain: p => p.toolchain = 'beta-x86_64-pc-windows-msvc (default)',
};
for (const [name, mutate] of Object.entries(mutations)) test(`reject ${name} provenance before destination effects`, async () => fixture(async root => {
  const directory = join(root, 'handoff');
  await prepare(directory, join(root, 'source.exe'), { ...context, root });
  const path = join(directory, 'provenance.json'); const provenance = JSON.parse(await readFile(path, 'utf8'));
  mutate(provenance); await writeFile(path, JSON.stringify(provenance));
  for (const existing of [false, true]) {
    const destination = join(root, existing ? 'existing.exe' : 'absent/target.exe');
    if (existing) await writeFile(destination, 'unchanged');
    await assert.rejects(() => verify(directory, destination, { ...consumerContext, root }));
    if (existing) assert.equal(await readFile(destination, 'utf8'), 'unchanged');
    else await assert.rejects(() => readdir(join(root, 'absent')), { code: 'ENOENT' });
  }
}));
for (const name of ['missing artifact', 'missing binary', 'missing provenance', 'extra file', 'extra directory', 'binary symlink', 'provenance symlink', 'directory symlink', 'malformed JSON', 'null JSON', 'oversized JSON', 'changed byte', 'truncated binary', 'PE machine', 'PE signature', 'missing field']) test(`reject ${name} before destination effects`, async () => fixture(async root => {
  let directory = join(root, 'handoff');
  await prepare(directory, join(root, 'source.exe'), { ...context, root });
  const binary = join(directory, 'tapid.exe'); const path = join(directory, 'provenance.json');
  if (name === 'missing artifact') await rm(directory, { recursive: true });
  if (name === 'missing binary') await rm(binary);
  if (name === 'missing provenance') await rm(path);
  if (name === 'extra file') await writeFile(join(directory, 'extra'), 'extra');
  if (name === 'extra directory') await mkdir(join(directory, 'extra'));
  if (name === 'directory symlink') { const alias = join(root, 'alias'); await symlink(directory, alias, 'dir'); directory = alias; }
  if (name.endsWith('symlink') && name !== 'directory symlink') {
    const input = name === 'binary symlink' ? binary : path;
    const copy = join(root, 'linked'); await writeFile(copy, await readFile(input)); await rm(input); await symlink(copy, input);
  }
  if (name === 'malformed JSON') await writeFile(path, '{');
  if (name === 'null JSON') await writeFile(path, 'null');
  if (name === 'oversized JSON') await writeFile(path, ' '.repeat(65537));
  if (name === 'missing field') { const p = JSON.parse(await readFile(path, 'utf8')); delete p.runAttempt; await writeFile(path, JSON.stringify(p)); }
  if (['changed byte', 'truncated binary', 'PE machine', 'PE signature'].includes(name)) {
    let bytes = await readFile(binary);
    if (name === 'changed byte') bytes[255]++;
    if (name === 'truncated binary') bytes = bytes.subarray(0, 10);
    if (name === 'PE machine') bytes.writeUInt16LE(0xaa64, 132);
    if (name === 'PE signature') bytes[128] = 0;
    await writeFile(binary, bytes);
    // PE tests retain matching digest to prove structural validation is independent.
    if (name.startsWith('PE')) { const p = JSON.parse(await readFile(path, 'utf8')); p.sha256 = (await import('node:crypto')).createHash('sha256').update(bytes).digest('hex'); await writeFile(path, JSON.stringify(p)); }
  }
  const destination = join(root, 'absent/target.exe');
  await assert.rejects(() => verify(directory, destination, { ...consumerContext, root }));
  await assert.rejects(() => readdir(join(root, 'absent')), { code: 'ENOENT' });
}));
for (const name of ['destination', 'parent']) test(`reject ${name} symlink without touching redirected files`, async () => fixture(async root => {
  const directory = join(root, 'handoff'); await prepare(directory, join(root, 'source.exe'), { ...context, root });
  await mkdir(join(root, 'real')); const sentinel = join(root, 'real/tapid.exe'); await writeFile(sentinel, 'unchanged');
  const destination = join(root, 'redirect/tapid.exe');
  if (name === 'parent') await symlink(join(root, 'real'), join(root, 'redirect'), 'dir');
  else { await mkdir(join(root, 'redirect')); await symlink(sentinel, destination); }
  await assert.rejects(() => verify(directory, destination, { ...consumerContext, root }));
  assert.equal(await readFile(sentinel, 'utf8'), 'unchanged');
}));
for (const [name, badEnv] of Object.entries({ 'missing repository': { GITHUB_REPOSITORY: undefined }, 'malformed SHA': { GITHUB_SHA: 'a' }, 'malformed run': { GITHUB_RUN_ID: '01' }, 'missing attempt': { GITHUB_RUN_ATTEMPT: undefined }, 'wrong runner': { RUNNER_OS: 'Linux' }, 'wrong arch': { RUNNER_ARCH: 'ARM64' }, 'missing image': { ImageVersion: undefined }, 'wrong job': { GITHUB_JOB: 'test' }, 'wrong flags': { RUSTFLAGS: '' }, 'encoded flags': { CARGO_ENCODED_RUSTFLAGS: '' }, 'target directory': { CARGO_TARGET_DIR: 'other' }, 'target override': { CARGO_BUILD_TARGET: '' }, 'incremental enabled': { CARGO_INCREMENTAL: '1' }, 'incremental absent': { CARGO_INCREMENTAL: undefined }, 'incremental empty': { CARGO_INCREMENTAL: '' }, 'incremental alias': { CARGO_INCREMENTAL: 'false' } })) test(`prepare rejects ${name} without handoff effects`, async () => fixture(async root => {
  const directory = join(root, 'handoff');
  await assert.rejects(() => prepare(directory, join(root, 'source.exe'), { ...context, root, env: { ...context.env, ...badEnv } }));
  await assert.rejects(() => readdir(directory), { code: 'ENOENT' });
}));
test('Windows CRLF manifest works without requiring Rust on verifier', async () => fixture(async root => {
  await writeFile(join(root, 'crates/tapid-cli/Cargo.toml'), '[package]\r\nname = "tapid"\r\nversion = "1.2.3"\r\n');
  const directory = join(root, 'handoff'); await prepare(directory, join(root, 'source.exe'), { ...context, root });
  const readCommand = async (command: string) => { assert.equal(command, 'git', 'verifier must not discover Rust'); return sha; };
  assert.equal(await verify(directory, join(root, 'target/debug/tapid.exe'), { ...consumerContext, root, readCommand }), '1.2.3');
}));
for (const name of ['source symlink', 'wrong checkout', 'discovery error', 'invalid compiler', 'missing binary', 'invalid PE', 'dirty handoff']) test(`prepare rejects ${name} without replacing existing handoff`, async () => fixture(async root => {
  const directory = join(root, 'handoff'); let binary = join(root, 'source.exe'); let readCommand = context.readCommand;
  if (name === 'source symlink') { binary = join(root, 'alias.exe'); await symlink(join(root, 'source.exe'), binary); }
  if (name === 'wrong checkout') readCommand = async command => command === 'git' ? 'b'.repeat(40) : receipts[command];
  if (name === 'discovery error') readCommand = async command => { if (command === 'cargo') throw new Error('discovery failed'); return receipts[command]; };
  if (name === 'invalid compiler') readCommand = async command => command === 'rustc' ? '' : receipts[command];
  if (name === 'missing binary') await rm(binary);
  if (name === 'invalid PE') await writeFile(binary, 'not PE');
  if (name === 'dirty handoff') { await mkdir(directory); await writeFile(join(directory, 'sentinel'), 'unchanged'); }
  await assert.rejects(() => prepare(directory, binary, { ...context, root, readCommand }));
  if (name === 'dirty handoff') { assert.deepEqual(await readdir(directory), ['sentinel']); assert.equal(await readFile(join(directory, 'sentinel'), 'utf8'), 'unchanged'); }
  else await assert.rejects(() => readdir(directory), { code: 'ENOENT' });
}));
