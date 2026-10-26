import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { promisify } from 'node:util';
import { pathToFileURL } from 'node:url';
import { candidateFingerprint, unwrapCandidate, validateCandidate, validateReleaseIntent } from './candidate.ts';
import { checkApprovedPlan } from './automation.ts';
import { cargoMetadataEnv } from './publish.ts';

const exec = promisify(execFile);
type Run = (command: string, args: string[], options: { cwd: string; env: NodeJS.ProcessEnv }) => Promise<{ stdout: string; stderr: string }>;
type Evidence = {
  schema: 'tapid-registry-verification-v1'; status: 'failed' | 'success';
  tag?: string; sourceSha?: string; candidateFingerprint?: string;
  expected: { resolver?: string; registryClient?: string };
  resolved?: { resolver: string; registryClient: string; resolverSource: string; registryClientSource: string; dependencyEdge: string };
  commands: { command: string; args: string[]; cwd: string; attempt: number; status: string; stdout: string; stderr: string }[];
  error?: string;
};
const runCommand: Run = (command, args, options) => exec(command, args, { ...options, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, timeout: 10 * 60 * 1000 });

// Reject config tables that could substitute a registry, patch an artifact, or
// execute a configured wrapper. Ignore comments; conservatively reject unknown
// TOML constructs rather than trying to implement a second TOML parser.
async function checkCargoConfig(cwd: string): Promise<void> {
  for (let directory = await realpath(cwd); ; directory = dirname(directory)) {
    for (const name of ['config', 'config.toml']) {
      const path = join(directory, '.cargo', name);
      let text: string;
      try { text = await readFile(path, 'utf8'); }
      catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue; throw error; }
      for (const line of text.split('\n').map(line => line.trim()).filter(line => line && !line.startsWith('#'))) {
        if (line === '[build]' || /^target-dir\s*=\s*"[^"\n]+"\s*(?:#.*)?$/.test(line)) continue;
        throw Error(`unsafe Cargo configuration: ${path}`);
      }
    }
    if (dirname(directory) === directory) break;
  }
}

type Package = { name: string; version: string; id: string; source: string | null; dependencies?: { name: string; req: string; kind: string | null }[] };
type Metadata = { packages: Package[]; resolve?: { nodes: { id: string; deps: { name: string; pkg: string }[] }[] } };
function metadata(text: string): Metadata {
  const value = JSON.parse(text);
  if (!value || !Array.isArray(value.packages) || value.packages.some((p: Package) => !p || typeof p.name !== 'string' || typeof p.version !== 'string')) throw Error('malformed Cargo metadata');
  return value;
}
function one(packages: Package[], name: string): Package {
  const matches = packages.filter(p => p.name === name);
  if (matches.length !== 1 || !/^\d+\.\d+\.\d+$/.test(matches[0].version)) throw Error(`absent or ambiguous package: ${name}`);
  return matches[0];
}
function registrySource(source: unknown): source is string {
  return source === 'registry+https://github.com/rust-lang/crates.io-index' || source === 'sparse+https://index.crates.io/';
}

/** Credential-free post-publication proof; never invokes cargo publish. */
export async function verifyRegistry(options: {
  candidate: unknown; intent: unknown; sourceDir: string; evidenceDir: string;
  run?: Run; sleep?: (ms: number) => Promise<void>; environment?: NodeJS.ProcessEnv;
}): Promise<Evidence> {
  const evidence: Evidence = { schema: 'tapid-registry-verification-v1', status: 'failed', expected: {}, commands: [] };
  const evidenceDir = resolve(options.evidenceDir);
  await mkdir(evidenceDir, { recursive: true });
  let temporary: string | undefined;
  const run = options.run ?? runCommand;
  const sleep = options.sleep ?? (ms => new Promise(done => setTimeout(done, ms)));
  try {
    const candidate = validateCandidate(unwrapCandidate(options.candidate));
    const intent = validateReleaseIntent(options.intent);
    evidence.tag = candidate.tag;
    evidence.sourceSha = candidate.commit;
    evidence.candidateFingerprint = candidateFingerprint(candidate);
    const sourceDir = await realpath(options.sourceDir);
    const intentBytes = await readFile(join(sourceDir, 'docs/releases/intent.json'));
    if (candidate.tag !== `v${intent.version}` || createHash('sha256').update(intentBytes).digest('hex') !== candidate.plan_sha256 || JSON.stringify(JSON.parse(intentBytes.toString())) !== JSON.stringify(intent)) throw Error('candidate/source/intent disagreement');
    temporary = await mkdtemp(join(tmpdir(), 'tapid-registry-verification-'));
    const consumer = join(temporary, 'consumer');
    const home = join(temporary, 'cargo-home');
    await mkdir(join(consumer, 'src'), { recursive: true });
    await mkdir(home);
    await checkCargoConfig(sourceDir);
    await checkCargoConfig(consumer);
    const env = { ...cargoMetadataEnv(options.environment ?? process.env, home), CARGO_TARGET_DIR: join(temporary, 'target') };
    async function command(executable: string, args: string[], cwd: string, attempt = 1): Promise<string> {
      const record = { command: executable, args, cwd, attempt, status: 'failed', stdout: '', stderr: '' };
      evidence.commands.push(record);
      try {
        const result = await run(executable, args, { cwd, env });
        record.stdout = result.stdout; record.stderr = result.stderr; record.status = 'success';
        return result.stdout;
      } catch (error) {
        const failure = error as Error & { stdout?: string; stderr?: string };
        record.stdout = failure.stdout ?? ''; record.stderr = failure.stderr ?? failure.message;
        throw error;
      }
    }
    if ((await command('git', ['rev-parse', 'HEAD'], sourceDir)).trim() !== candidate.commit || (await command('git', ['status', '--porcelain', '--untracked-files=all'], sourceDir)).trim()) throw Error('tagged source identity or cleanliness mismatch');
    const tagged = metadata(await command('cargo', ['metadata', '--no-deps', '--format-version', '1', '--locked'], sourceDir));
    checkApprovedPlan(intent, { packages: [], blockers: [] }, tagged);
    const resolver = one(tagged.packages, 'tapid-resolver');
    const client = one(tagged.packages, 'tapid-registry-client');
    const dependencies = resolver.dependencies?.filter(d => d.name === client.name && d.kind === null);
    if (!dependencies || dependencies.length !== 1 || ![client.version, `^${client.version}`, `=${client.version}`].includes(dependencies[0].req)) throw Error('resolver declared registry-client dependency mismatch');
    evidence.expected = { resolver: resolver.version, registryClient: client.version };
    await writeFile(join(consumer, 'Cargo.toml'), `[package]\nname = "tapid-release-registry-consumer"\nversion = "0.0.0"\nedition = "2021"\n[workspace]\n[dependencies]\ntapid-resolver = "=${resolver.version}"\ntapid-registry-client = "=${client.version}"\n`);
    await writeFile(join(consumer, 'src/main.rs'), 'fn main() { let _: tapid_resolver::Requirement = "^1.0.0".parse().expect("public range parser"); }\n');
    for (let attempt = 1; attempt <= 6; attempt++) {
      try {
        await command('cargo', ['package', '-p', 'tapid-resolver', '--locked'], sourceDir, attempt);
        // Metadata acquires registry artifacts before compilation, so compilation
        // errors never fall into the propagation retry path.
        const graphText = await command('cargo', ['metadata', '--format-version', '1'], consumer, attempt);
        await writeFile(join(evidenceDir, 'metadata.json'), graphText);
        await copyFile(join(consumer, 'Cargo.lock'), join(evidenceDir, 'Cargo.lock'));
        const graph = metadata(graphText);
        const resolvedResolver = one(graph.packages, resolver.name);
        const resolvedClient = one(graph.packages, client.name);
        if (![resolvedResolver.id, resolvedClient.id].every(id => typeof id === 'string' && id.length > 0) || resolvedResolver.id === resolvedClient.id) throw Error('malformed resolved package identity');
        if (resolvedResolver.version !== resolver.version || resolvedClient.version !== client.version || !registrySource(resolvedResolver.source) || !registrySource(resolvedClient.source)) throw Error('resolved version or registry source mismatch');
        const nodes = graph.resolve?.nodes?.filter(n => n.id === resolvedResolver.id);
        const edges = nodes?.length === 1 ? nodes[0].deps.filter(d => d.name === 'tapid_registry_client') : [];
        if (edges.length !== 1 || edges[0].pkg !== resolvedClient.id) throw Error('resolved resolver dependency edge mismatch');
        evidence.resolved = { resolver: resolvedResolver.version, registryClient: resolvedClient.version, resolverSource: resolvedResolver.source, registryClientSource: resolvedClient.source, dependencyEdge: `${resolvedResolver.id} -> ${resolvedClient.id}` };
      } catch (error) {
        const last = evidence.commands.at(-1)!;
        // Only explicit Cargo registry acquisition diagnostics qualify. Package
        // verification/compilation failures and unclassified errors fail closed.
        const diagnostic = last.stderr;
        const acquisition = last.status === 'failed' && /(?:failed to download from|failed to get successful HTTP response from|no matching package named|failed to select a version for the requirement)/.test(diagnostic) && !/(?:failed to verify package|could not compile|error\[E\d+\])/.test(diagnostic);
        if (!acquisition || attempt === 6) throw error;
        await sleep(attempt * 10000);
        continue;
      }
      await command('cargo', ['check', '--locked'], consumer, attempt);
      evidence.status = 'success';
      return evidence;
    }
    throw Error('registry verification attempts exhausted');
  } catch (error) {
    evidence.error = (error as Error).message;
    throw error;
  } finally {
    try {
      await writeFile(join(evidenceDir, 'verification.json'), JSON.stringify(evidence, null, 2) + '\n');
    } finally {
      if (temporary) await rm(temporary, { recursive: true, force: true });
    }
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [candidatePath, intentPath, sourceDir, evidenceDir, ...extra] = process.argv.slice(2);
  if (!evidenceDir || extra.length) throw Error('usage: verify_registry.ts CANDIDATE INTENT TAGGED_SOURCE EVIDENCE_DIR');
  const result = await verifyRegistry({ candidate: JSON.parse(await readFile(candidatePath, 'utf8')), intent: JSON.parse(await readFile(intentPath, 'utf8')), sourceDir, evidenceDir });
  console.log(JSON.stringify(result, null, 2));
}
