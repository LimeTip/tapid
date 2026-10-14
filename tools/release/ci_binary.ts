// Same-run Windows debug CLI handoff, not release attestation.
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstat, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
type Context = { root?: string; env?: NodeJS.ProcessEnv; readCommand?: (command: string, args: string[]) => Promise<string> };
const argv = ['cargo', 'build', '--bin', 'tapid', '--locked'];
const settings = ['CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET', 'CARGO_INCREMENTAL'];
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }
function validateIdentity(record: Awaited<ReturnType<typeof identity>>['record'], role: 'producer' | 'consumer') {
  check(typeof record.repository === 'string' && /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(record.repository) && record.repository.length <= 200, 'invalid repository');
  check(typeof record.eventSha === 'string' && /^[a-f0-9]{40}$/.test(record.eventSha) && record.sourceSha === record.eventSha, 'checkout/event SHA mismatch');
  for (const value of [record.runId, record.runAttempt]) check(typeof value === 'string' && /^[1-9][0-9]{0,19}$/.test(value), 'invalid run identity');
  check(record.runnerOS === 'Windows' && record.runnerArch === 'X64', 'expected Windows X64');
  for (const value of [record.imageOS, record.imageVersion]) check(typeof value === 'string' && /^[A-Za-z0-9_.-]{1,100}$/.test(value), 'invalid image identity');
  // The pinned Rust action disables incremental compilation only in the producer.
  check(record.rustflags === '-Dwarnings' && settings.every(key => record.settings[key] === (key === 'CARGO_INCREMENTAL' && role === 'producer' ? '0' : null)), 'unexpected build flags/target settings');
  check(/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(record.version), 'invalid CLI version');
}
function validateCompiler(p: { cargo: string; rustc: string; toolchain: string }) {
  for (const value of [p.cargo, p.rustc, p.toolchain]) check(typeof value === 'string' && value.length > 0 && value.length <= 4096 && !value.includes('\r'), 'invalid compiler receipt');
  const cargo = p.cargo.match(/^cargo ([0-9]+\.[0-9]+\.[0-9]+) \([a-f0-9]{7,40} [0-9]{4}-[0-9]{2}-[0-9]{2}\)$/);
  const rustc = p.rustc.match(/^rustc ([0-9]+\.[0-9]+\.[0-9]+) \([a-f0-9]{7,40} [0-9]{4}-[0-9]{2}-[0-9]{2}\)\nbinary: rustc\ncommit-hash: [a-f0-9]{40}\ncommit-date: [0-9]{4}-[0-9]{2}-[0-9]{2}\nhost: x86_64-pc-windows-msvc\nrelease: ([0-9]+\.[0-9]+\.[0-9]+)\nLLVM version: [0-9]+\.[0-9]+\.[0-9]+$/);
  check(cargo && rustc && cargo[1] === rustc[1] && rustc[1] === rustc[2], 'expected stable native Cargo/rustc receipts');
  check(p.toolchain === `stable-x86_64-pc-windows-msvc (default)` || p.toolchain === `${rustc[1]}-x86_64-pc-windows-msvc (default)`, 'expected stable native active toolchain');
}
async function noRedirection(path: string) {
  let current = resolve(path);
  while (true) {
    try { check(!(await lstat(current)).isSymbolicLink(), `symlink redirection: ${current}`); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    const parent = dirname(current); if (parent === current) break; current = parent;
  }
}
async function regularFile(path: string, limit: number) {
  const stat = await lstat(path);
  check(stat.isFile() && !stat.isSymbolicLink() && stat.size > 0 && stat.size <= limit, 'expected bounded regular file');
}
function validatePE(bytes: Buffer) {
  check(bytes.length >= 64 && bytes[0] === 0x4d && bytes[1] === 0x5a, 'invalid PE DOS header');
  const offset = bytes.readUInt32LE(60);
  check(offset >= 64 && offset <= bytes.length - 6 && bytes.subarray(offset, offset + 4).equals(Buffer.from('PE\0\0')) && bytes.readUInt16LE(offset + 4) === 0x8664, 'expected AMD64 PE binary');
}
async function identity(context: Context) {
  const root = context.root ?? process.cwd();
  const env = context.env ?? process.env;
  const readCommand = context.readCommand ?? (async (command, args) => (await execFileAsync(command, args, { cwd: root, env, timeout: 30_000 })).stdout.trim());
  const sourceSha = await readCommand('git', ['rev-parse', 'HEAD']);
  const manifest = await readFile(resolve(root, 'crates/tapid-cli/Cargo.toml'), 'utf8');
  const version = manifest.match(/^version = "([^"]+)"$/m)?.[1];
  check(version, 'missing CLI manifest version');
  return { root, readCommand, record: {
    schema: 1, repository: env.GITHUB_REPOSITORY, sourceSha, eventSha: env.GITHUB_SHA,
    runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT,
    producerJob: 'platform-consumer-validation', producerLeg: 'windows-latest',
    runnerOS: env.RUNNER_OS, runnerArch: env.RUNNER_ARCH,
    imageOS: env.ImageOS, imageVersion: env.ImageVersion,
    target: 'x86_64-pc-windows-msvc', profile: 'debug', features: 'default', argv,
    rustflags: env.RUSTFLAGS, settings: Object.fromEntries(settings.map(key => [key, env[key] ?? null])), version,
  } };
}
export async function prepare(directory: string, binary: string, context: Context = {}) {
  const { record, readCommand } = await identity(context);
  validateIdentity(record, 'producer');
  check((context.env ?? process.env).GITHUB_JOB === record.producerJob, 'unexpected producer job');
  await noRedirection(binary);
  await regularFile(binary, 512 * 1024 * 1024);
  const bytes = await readFile(binary);
  validatePE(bytes);
  const provenance = { ...record, cargo: await readCommand('cargo', ['--version']), rustc: await readCommand('rustc', ['-vV']), toolchain: await readCommand('rustup', ['show', 'active-toolchain']), size: bytes.length, sha256: hash(bytes) };
  validateCompiler(provenance);
  await noRedirection(directory);
  await mkdir(directory);
  await writeFile(resolve(directory, 'tapid.exe'), bytes, { flag: 'wx' });
  await writeFile(resolve(directory, 'provenance.json'), JSON.stringify(provenance, null, 2) + '\n', { flag: 'wx' });
}
export async function verify(directory: string, destination: string, context: Context = {}): Promise<string> {
  const { record } = await identity(context);
  validateIdentity(record, 'consumer');
  await noRedirection(directory);
  check(JSON.stringify((await readdir(directory)).sort()) === JSON.stringify(['provenance.json', 'tapid.exe']), 'expected exactly two handoff files');
  await regularFile(resolve(directory, 'provenance.json'), 65536);
  await regularFile(resolve(directory, 'tapid.exe'), 512 * 1024 * 1024);
  const provenance = JSON.parse(await readFile(resolve(directory, 'provenance.json'), 'utf8'));
  check(provenance && typeof provenance === 'object' && !Array.isArray(provenance), 'invalid provenance object');
  check(JSON.stringify(Object.keys(provenance).sort()) === JSON.stringify([...Object.keys(record), 'cargo', 'rustc', 'toolchain', 'size', 'sha256'].sort()), 'invalid provenance fields');
  // Compare the declared producer contract, not the toolchain-free consumer's absent build-only setting.
  const producerRecord = { ...record, settings: { ...record.settings, CARGO_INCREMENTAL: '0' } };
  for (const [key, value] of Object.entries(producerRecord)) check(JSON.stringify(provenance[key]) === JSON.stringify(value), `provenance mismatch: ${key}`);
  validateCompiler(provenance);
  const bytes = await readFile(resolve(directory, 'tapid.exe'));
  check(Number.isSafeInteger(provenance.size) && provenance.size === bytes.length && typeof provenance.sha256 === 'string' && /^[a-f0-9]{64}$/.test(provenance.sha256) && provenance.sha256 === hash(bytes), 'binary digest/size mismatch');
  validatePE(bytes);
  await noRedirection(destination);
  try { await regularFile(destination, 512 * 1024 * 1024); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
  await mkdir(dirname(resolve(destination)), { recursive: true });
  await writeFile(destination, bytes);
  console.log(`Verified Windows CLI ${record.sourceSha} run ${record.runId} attempt ${record.runAttempt}: ${hash(bytes)} (${bytes.length} bytes)`);
  return record.version;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [mode, directoryFlag, directory, fileFlag, file, ...extra] = process.argv.slice(2);
    check((mode === 'prepare' || mode === 'verify') && directoryFlag === '--directory' && directory && fileFlag === (mode === 'prepare' ? '--binary' : '--destination') && file && extra.length === 0, 'invalid handoff arguments');
    if (mode === 'prepare') await prepare(directory, file);
    else console.log(`Expected native version: tapid ${await verify(directory, file)}`);
  } catch (error) { console.error(error); process.exitCode = 1; }
}
