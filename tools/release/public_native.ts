import { execFile, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { isAbsolute, join, resolve, sep } from 'node:path';
import { cargoMetadataEnv } from './publish.ts';
import { homedir } from 'node:os';
import { realpath } from 'node:fs/promises';
import type { CargoMetadata } from './publish.ts';

export type PublicVerifier = {
  inspectArchive(path: string, executable: 'tapid' | 'tapid.exe'): Promise<void>;
  verifySignature(record: string, sidecar: string): Promise<void>;
};

const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const MAX_OUTPUT = 2 * 1024 * 1024;
const PROCESS_MS = 5 * 60_000;

function cargoEnvironment(cargoHome: string): NodeJS.ProcessEnv {
  const environment = cargoMetadataEnv(process.env, cargoHome);
  if (process.platform === 'win32') {
    environment.SystemRoot = process.env.SystemRoot;
    environment.USERPROFILE = homedir();
  }
  return environment;
}

/** Metadata runs from trusted cwd, with no downloaded .cargo configuration. */
export async function taggedCargoMetadata(source: string, cargoHome: string, remainingMs: () => number = () => PROCESS_MS): Promise<CargoMetadata> {
  const environment = cargoEnvironment(cargoHome);
  environment.CARGO_TARGET_DIR = join(source, 'target');
  const output = await run(process.execPath, [
    '--experimental-strip-types', join(ROOT, 'scripts/dev.ts'),
    'metadata', '--manifest-path', join(source, 'Cargo.toml'),
    '--offline', '--locked', '--no-deps', '--format-version', '1',
  ], environment, 'tagged offline Cargo metadata', remainingMs());
  remainingMs();
  try {
    const metadata = JSON.parse(output);
    if (!Array.isArray(metadata.packages)) throw Error();
    const sourceRoot = await realpath(source);
    for (const pkg of metadata.packages) {
      if (typeof pkg.manifest_path !== 'string' || !(await realpath(pkg.manifest_path)).startsWith(`${sourceRoot}${sep}`)) throw Error();
    }
    return metadata;
  } catch { throw Error('invalid tagged Cargo metadata'); }
}

// Never forward subprocess diagnostics: they can contain paths or environment secrets.
async function run(program: string, args: string[], env: NodeJS.ProcessEnv, operation: string, remainingMs = PROCESS_MS): Promise<string> {
  return new Promise((accept, reject) => {
    const child = spawn(program, args, { cwd: ROOT, env, detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'] });
    let stopped = false, stdoutBytes = 0, stderrBytes = 0;
    const chunks: Buffer[] = [];
    const stop = () => {
      if (stopped) return;
      stopped = true;
      if (!child.pid) return;
      if (process.platform === 'win32' && process.env.SystemRoot) {
        execFile(join(process.env.SystemRoot, 'System32', 'taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { timeout: 5000, maxBuffer: MAX_OUTPUT }, () => {});
      } else {
        try { process.kill(-child.pid, 'SIGKILL'); } catch { child.kill('SIGKILL'); }
      }
    };
    const timer = setTimeout(stop, Math.max(1, Math.min(PROCESS_MS, remainingMs)));
    child.stdout.on('data', (chunk: Buffer) => {
      stdoutBytes += chunk.length;
      if (stdoutBytes > MAX_OUTPUT) stop();
      else chunks.push(chunk);
    });
    child.stderr.on('data', (chunk: Buffer) => {
      stderrBytes += chunk.length;
      if (stderrBytes > MAX_OUTPUT) stop();
    });
    child.on('error', () => { clearTimeout(timer); reject(Error(`${operation} failed`)); });
    child.on('close', code => {
      clearTimeout(timer);
      if (stopped || code !== 0) reject(Error(`${operation} failed`));
      else accept(Buffer.concat(chunks).toString('utf8'));
    });
  });
}

/** Builds only the trusted checkout; Cargo JSON supplies the executable identity. */
export async function buildPublicVerifier(remainingMs: () => number = () => PROCESS_MS): Promise<PublicVerifier> {
  const environment = cargoEnvironment(process.env.CARGO_HOME ?? join(homedir(), '.cargo'));
  // Keep all generated artifacts worktree-local, independent of ambient configuration.
  environment.CARGO_TARGET_DIR = join(ROOT, 'target');
  const output = await run(process.execPath, [
    '--experimental-strip-types', join(ROOT, 'scripts/dev.ts'),
    'build', '-p', 'tapid', '--bin', 'tapid', '--locked', '--message-format=json',
  ], environment, 'local native verifier build', remainingMs());
  remainingMs();
  let executable: string | undefined;
  try {
    for (const line of output.trim().split('\n')) {
      const message = JSON.parse(line);
      if (message.reason === 'compiler-artifact' && message.target?.name === 'tapid'
        && message.target.kind?.includes('bin') && typeof message.executable === 'string') {
        if (executable) throw Error('duplicate native executable');
        executable = message.executable;
      }
    }
  } catch { throw Error('invalid local native build metadata'); }
  if (!executable || !isAbsolute(executable) || !resolve(executable).startsWith(`${join(ROOT, 'target')}${sep}`)) throw Error('missing local native build executable');
  const binary = executable;
  // Native offline operations need neither Cargo, HOME, credentials nor PATH.
  const nativeEnv: NodeJS.ProcessEnv = process.platform === 'win32' && process.env.SystemRoot
    ? { SystemRoot: process.env.SystemRoot } : {};
  return {
    async inspectArchive(path, executable) {
      await run(binary, ['__inspect-release-archive', path, executable], nativeEnv, 'native archive inspection', remainingMs());
      remainingMs();
    },
    async verifySignature(record, sidecar) {
      await run(binary, ['__verify-release-record', record, sidecar], nativeEnv, 'native signature verification', remainingMs());
      remainingMs();
    },
  };
}
