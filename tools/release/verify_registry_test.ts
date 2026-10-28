import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtemp, mkdir, writeFile, readFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { expectedAssets } from './candidate.ts';
import { verifyRegistry } from './verify_registry.ts';

const registry = 'registry+https://github.com/rust-lang/crates.io-index';
async function fixture(version = '0.0.6', client = '0.0.6') {
  const root = await mkdtemp(join(tmpdir(), 'tapid-registry-test-'));
  const source = join(root, 'source');
  await mkdir(join(source, 'docs/releases'), { recursive: true });
  const intent = { schema: 'tapid-release-intent-v1', version: '0.0.9', baseline: 'v0.0.8', prepared_from: 'a'.repeat(40), notes: 'docs/releases/0.0.9.md', packages: [{ name: 'tapid-resolver', version }, { name: 'tapid', version: '0.0.9' }] };
  const bytes = JSON.stringify(intent);
  await writeFile(join(source, 'docs/releases/intent.json'), bytes);
  const candidate = { schema: 'tapid-release-candidate-v1', repository: 'LimeTip/tapid', tag: 'v0.0.9', title: 'v0.0.9', commit: 'b'.repeat(40), tag_object: 'c'.repeat(40), release_id: 1, notes_sha256: 'd'.repeat(64), plan_sha256: createHash('sha256').update(bytes).digest('hex'), assets: expectedAssets('v0.0.9').map((name, i) => ({ name, id: i + 1, size: 1, sha256: 'e'.repeat(64) })) };
  const metadata = { packages: [
    { name: 'tapid-resolver', version, id: 'resolver', source: null, dependencies: [{ name: 'tapid-registry-client', req: `^${client}`, kind: null }] },
    { name: 'tapid-registry-client', version: client, id: 'client', source: null },
    { name: 'tapid', version: '0.0.9' },
  ] };
  const graph = { packages: metadata.packages.slice(0, 2).map(p => ({ ...p, source: registry })), resolve: { nodes: [{ id: 'resolver', deps: [{ name: 'tapid_registry_client', pkg: 'client' }] }] } };
  const calls: { command: string; args: string[]; cwd: string; env: NodeJS.ProcessEnv }[] = [];
  const run = async (command: string, args: string[], options: { cwd: string; env: NodeJS.ProcessEnv }) => {
    calls.push({ command, args, ...options });
    if (command === 'git') return { stdout: args[0] === 'rev-parse' ? candidate.commit : '', stderr: '' };
    if (args[0] === 'metadata' && !args.includes('--no-deps')) await writeFile(join(options.cwd, 'Cargo.lock'), '# registry consumer lock');
    if (args[0] === 'metadata') return { stdout: JSON.stringify(args.includes('--no-deps') ? metadata : graph), stderr: '' };
    if (args[0] === 'check') await writeFile(join(options.cwd, 'Cargo.lock'), '# registry consumer lock');
    return { stdout: '', stderr: '' };
  };
  return { root, source, intent, candidate, metadata, graph, calls, run, options: { candidate, intent, sourceDir: source, evidenceDir: join(root, 'evidence'), run }, cleanup: () => rm(root, { recursive: true, force: true }) };
}

test('packages tagged resolver and compiles an isolated registry consumer with candidate-bound evidence', async () => {
  const f = await fixture();
  try {
    const result = await verifyRegistry(f.options);
    assert.equal(result.status, 'success');
    assert.equal(result.expected.resolver, '0.0.6');
    assert.equal(result.expected.registryClient, '0.0.6');
    assert.equal(result.resolved?.dependencyEdge, 'resolver -> client');
    assert(f.calls.some(c => c.args.join(' ') === 'package -p tapid-resolver --locked'));
    const check = f.calls.find(c => c.args[0] === 'check')!;
    assert(!check.cwd.startsWith(f.source));
    assert.equal(check.env.CARGO_REGISTRY_TOKEN, undefined);
    await assert.rejects(access(check.cwd));
    assert.match(await readFile(join(f.options.evidenceDir, 'Cargo.lock'), 'utf8'), /registry consumer lock/);
  } finally { await f.cleanup(); }
});

for (const [label, mutate, expected] of [
  ['wrong source', (f: Awaited<ReturnType<typeof fixture>>) => { f.graph.packages[0].source = 'registry+https://example.invalid/index'; }, /registry source/],
  ['path substitution', (f: Awaited<ReturnType<typeof fixture>>) => { f.graph.packages[1].source = null as unknown as string; }, /registry source/],
  ['wrong edge despite expected client present', (f: Awaited<ReturnType<typeof fixture>>) => { f.graph.resolve.nodes[0].deps[0].pkg = 'other-client'; }, /dependency edge/],
  ['ambiguous resolver', (f: Awaited<ReturnType<typeof fixture>>) => { f.metadata.packages.push(f.metadata.packages[0]); }, /differs from source/],
  ['declared dependency mismatch', (f: Awaited<ReturnType<typeof fixture>>) => { f.metadata.packages[0].dependencies![0].req = '^0.0.1'; }, /declared/],
  ['intent source disagreement', (f: Awaited<ReturnType<typeof fixture>>) => { f.metadata.packages[0].version = '0.0.5'; }, /differs from source/],
] as const) {
  test(`rejects ${label} without propagation retries`, async () => {
    const f = await fixture();
    try {
      mutate(f);
      await assert.rejects(verifyRegistry({ ...f.options, sleep: async () => assert.fail('semantic failure must not retry') }), expected);
      const evidence = JSON.parse(await readFile(join(f.options.evidenceDir, 'verification.json'), 'utf8'));
      assert.equal(evidence.status, 'failed');
      assert.match(evidence.error, expected);
      assert(!f.calls.some(c => c.args[0] === 'check'));
    } finally { await f.cleanup(); }
  });
}

test('current versions succeed with an already-published client absent from the approved missing set', async () => {
  const f = await fixture('0.0.8', '0.0.7');
  try {
    const evidence = await verifyRegistry(f.options);
    assert.deepEqual(evidence.expected, { resolver: '0.0.8', registryClient: '0.0.7' });
    assert(!f.intent.packages.some(p => p.name === 'tapid-registry-client'));
  } finally { await f.cleanup(); }
});

for (const wrapped of [false, true]) for (const exhausted of [false, true]) test(`${wrapped ? 'wrapped ' : ''}registry propagation ${exhausted ? 'exhausts six attempts' : 'retries successfully'} and retains diagnostics`, async () => {
  const f = await fixture();
  let failures = 0;
  const delays: number[] = [];
  try {
    const operation = verifyRegistry({ ...f.options, sleep: async ms => { delays.push(ms); }, run: async (cmd, args, options) => {
      if (args[0] === 'package' && (exhausted || failures === 0)) {
        failures++;
        throw Object.assign(Error('Cargo registry acquisition failed'), { stderr: wrapped ? 'error: failed to verify package tarball\nCaused by:\n  no matching package named `tapid-registry-client` found' : 'error: failed to download from https://static.crates.io/crates/tapid-resolver' });
      }
      return f.run(cmd, args, options);
    } });
    if (exhausted) await assert.rejects(operation, /acquisition/); else assert.equal((await operation).status, 'success');
    assert.deepEqual(delays, exhausted ? [10000, 20000, 30000, 40000, 50000] : [10000]);
    const evidence = JSON.parse(await readFile(join(f.options.evidenceDir, 'verification.json'), 'utf8'));
    assert.equal(evidence.commands.filter((c: { status: string }) => c.status === 'failed').length, exhausted ? 6 : 1);
    assert.match(evidence.commands.find((c: { status: string }) => c.status === 'failed').stderr, wrapped ? /no matching package named/ : /static.crates.io/);
    assert.equal(evidence.status, exhausted ? 'failed' : 'success');
    const isolated = f.calls.find(c => c.command === 'cargo');
    assert(isolated);
    await assert.rejects(access(isolated.env.CARGO_HOME!));
  } finally { await f.cleanup(); }
});

for (const stage of ['package', 'check']) test(`${stage} compilation failure terminates immediately and retains evidence`, async () => {
  const f = await fixture();
  try {
    await assert.rejects(verifyRegistry({ ...f.options, sleep: async () => assert.fail('must not retry compilation'), run: async (cmd, args, options) => {
      if (args[0] === stage) throw Object.assign(Error('compilation failed'), { stderr: 'error[E0425]: cannot find value' });
      return f.run(cmd, args, options);
    } }), /compilation failed/);
    const evidence = JSON.parse(await readFile(join(f.options.evidenceDir, 'verification.json'), 'utf8'));
    assert.match(evidence.commands.at(-1).stderr, /E0425/);
    if (stage === 'check') await access(join(f.options.evidenceDir, 'Cargo.lock'));
    const consumerCall = f.calls.find(c => c.args[0] === 'metadata' && !c.args.includes('--no-deps'));
    if (consumerCall) await assert.rejects(access(consumerCall.cwd));
  } finally { await f.cleanup(); }
});

test('strips credentials, registry overrides, compiler wrappers and inherited targets', async () => {
  const f = await fixture();
  try {
    await verifyRegistry({ ...f.options, environment: { PATH: process.env.PATH, HOME: process.env.HOME, CARGO_REGISTRY_TOKEN: 'secret', GH_TOKEN: 'secret', CARGO_REGISTRIES_CRATES_IO_INDEX: 'bad', RUSTC_WRAPPER: 'bad', RUSTFLAGS: 'bad', CARGO_TARGET_DIR: 'bad', CARGO_HOME: 'bad' }, run: async (cmd, args, options) => {
      assert.equal(options.env.CARGO_REGISTRY_TOKEN, undefined);
      assert.equal(options.env.GH_TOKEN, undefined);
      assert.equal(options.env.CARGO_REGISTRIES_CRATES_IO_INDEX, undefined);
      assert.equal(options.env.RUSTC_WRAPPER, undefined);
      assert.equal(options.env.RUSTFLAGS, undefined);
      assert.notEqual(options.env.CARGO_TARGET_DIR, 'bad');
      assert.notEqual(options.env.CARGO_HOME, 'bad');
      if (args[0] === 'check') {
        assert.match(await readFile(join(options.cwd, 'Cargo.toml'), 'utf8'), /tapid-resolver = "=0.0.6"/);
        assert.match(await readFile(join(options.cwd, 'src/main.rs'), 'utf8'), /tapid_resolver::Requirement/);
      }
      return f.run(cmd, args, options);
    } });
  } finally { await f.cleanup(); }
});

for (const config of ['[source.crates-io]\nreplace-with="other"', '[patch.crates-io]\ntapid-resolver={path="bad"}', '[build]\nrustc-wrapper="bad"']) test(`rejects ancestor Cargo configuration: ${config.split('\n')[0]}`, async () => {
  const f = await fixture();
  try {
    await mkdir(join(f.root, '.cargo'));
    await writeFile(join(f.root, '.cargo/config.toml'), config);
    await assert.rejects(verifyRegistry(f.options), /unsafe Cargo configuration/);
    assert.equal(f.calls.length, 0);
  } finally { await f.cleanup(); }
});

test('rejects candidate fingerprint, source identity, and malformed metadata', async () => {
  const f = await fixture();
  try {
    await assert.rejects(verifyRegistry({ ...f.options, candidate: { candidate: f.candidate, candidate_sha256: '0'.repeat(64) } }), /digest mismatch/);
    await assert.rejects(verifyRegistry({ ...f.options, run: async (cmd, args, options) => cmd === 'git' ? { stdout: 'wrong', stderr: '' } : f.run(cmd, args, options) }), /identity/);
    await assert.rejects(verifyRegistry({ ...f.options, run: async (cmd, args, options) => args[0] === 'metadata' ? { stdout: '{"packages":null}', stderr: '' } : f.run(cmd, args, options) }), /malformed/);
  } finally { await f.cleanup(); }
});

test('rejects malformed resolve identities even if undefined identifiers appear to match', async () => {
  const f = await fixture();
  try {
    delete (f.graph.packages[0] as { id?: string }).id;
    delete (f.graph.resolve.nodes[0] as { id?: string }).id;
    await assert.rejects(verifyRegistry(f.options), /metadata|identity/);
  } finally { await f.cleanup(); }
});

test('retains consumer lockfile when a resolved registry identity is rejected', async () => {
  const f = await fixture();
  try {
    f.graph.packages[0].source = 'path+file:///substituted';
    await assert.rejects(verifyRegistry(f.options), /registry source/);
    await access(join(f.options.evidenceDir, 'Cargo.lock'));
  } finally { await f.cleanup(); }
});

for (const diagnostic of ['error: failed to verify package tarball', 'error: an unclassified Cargo failure', 'error: could not compile x\nno matching package named x found', 'error[E0425]: cannot find value\nfailed to download from registry']) test(`does not retry ${diagnostic}`, async () => {
  const f = await fixture();
  try {
    await assert.rejects(verifyRegistry({ ...f.options, sleep: async () => assert.fail('must not retry'), run: async (cmd, args, options) => {
      if (args[0] === 'package') throw Object.assign(Error('package failed'), { stderr: diagnostic });
      return f.run(cmd, args, options);
    } }), /package failed/);
    const evidence = JSON.parse(await readFile(join(f.options.evidenceDir, 'verification.json'), 'utf8'));
    assert.equal(evidence.commands.filter((c: { status: string }) => c.status === 'failed').length, 1);
  } finally { await f.cleanup(); }
});

test('rejects absent client, changed candidate intent digest, wrong resolved versions, and missing graph', async () => {
  const absent = await fixture();
  const digest = await fixture();
  const version = await fixture();
  const graph = await fixture();
  try {
    absent.metadata.packages.splice(1, 1);
    await assert.rejects(verifyRegistry(absent.options), /absent or ambiguous/);
    digest.candidate.plan_sha256 = 'f'.repeat(64);
    await assert.rejects(verifyRegistry(digest.options), /candidate\/source\/intent disagreement/);
    version.graph.packages[0].version = '0.0.5';
    await assert.rejects(verifyRegistry(version.options), /resolved version/);
    delete (graph.graph as { resolve?: unknown }).resolve;
    await assert.rejects(verifyRegistry(graph.options), /dependency edge/);
  } finally { await Promise.all([absent.cleanup(), digest.cleanup(), version.cleanup(), graph.cleanup()]); }
});

test('CLI rejects an invalid candidate without executing Cargo and records failure evidence', async () => {
  const { execFile } = await import('node:child_process');
  const { promisify } = await import('node:util');
  const f = await fixture();
  try {
    const candidatePath = join(f.root, 'candidate.json');
    const intentPath = join(f.root, 'intent.json');
    await writeFile(candidatePath, '{}');
    await writeFile(intentPath, JSON.stringify(f.intent));
    await assert.rejects(promisify(execFile)(process.execPath, ['--experimental-strip-types', fileURLToPath(new URL('./verify_registry.ts', import.meta.url)), candidatePath, intentPath, f.source, f.options.evidenceDir], { env: { PATH: '' } }), /unexpected release fields/);
    const evidence = JSON.parse(await readFile(join(f.options.evidenceDir, 'verification.json'), 'utf8'));
    assert.equal(evidence.status, 'failed');
    assert.deepEqual(evidence.commands, []);
  } finally { await f.cleanup(); }
});
