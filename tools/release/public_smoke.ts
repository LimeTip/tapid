/** Public read-back identity. This module never publishes or authenticates requests. */
import { expectedAssets } from './candidate.ts';
import { TARGETS, checksumLines, releaseRecord, releaseVersion } from './release.ts';
import { compareStableVersions } from './versions.ts';
import { createHash } from 'node:crypto';
import { mkdir, open, readFile, copyFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { pathToFileURL } from 'node:url';
import { renderInstallers, templatesForCommit, type InstallerTemplates } from './bootstrap.ts';

export type PublicReleaseIdentity = {
  tag: string; commit: string; releaseId: number; protocol: 'record-v1' | 'legacy';
  assets: { id: number; name: string; size: number }[];
};
export type SmokeIdentity = {
  schema: 'tapid-public-smoke-identity-v1'; run: string; runnerSha: string;
  platforms: string[]; selected: PublicReleaseIdentity; latest: PublicReleaseIdentity;
};
type Options = { tag?: string; releaseId?: string; commit?: string; runnerSha: string; run: string; platforms: string[];
  expectedLatest?: { tag: string; commit: string }; expectedSelectedCommit?: string };
type PublicFetch = (url: string, options: RequestInit) => Promise<Response>;

/** Every hop is HTTPS and credential-free; the deadline also bounds body reads. */
async function publicResponse(url: string, fetcher: PublicFetch, timeout: number, headers?: HeadersInit): Promise<Response> {
  const signal = AbortSignal.timeout(timeout);
  for (let hop = 0; hop <= 3; hop++) {
    const parsed = new URL(url);
    if (parsed.protocol !== 'https:' || parsed.username || parsed.password) throw Error('unsafe public URL');
    const response = await fetcher(url, { credentials: 'omit', redirect: 'manual', signal,
      headers });
    if ([301, 302, 303, 307, 308].includes(response.status)) {
      await response.body?.cancel();
      const location = response.headers.get('location');
      if (!location) throw Error('public redirect missing location');
      url = new URL(location, url).href;
      continue;
    }
    if (!response.ok || !response.body) {
      await response.body?.cancel();
      throw Error(`public endpoint unavailable: ${response.status}`);
    }
    return response;
  }
  throw Error('public redirect limit exceeded');
}
async function publicJson(path: string, fetcher: PublicFetch): Promise<any> {
  const response = await publicResponse(`https://api.github.com/repos/LimeTip/tapid/${path}`, fetcher, 30_000, { accept: 'application/vnd.github+json' });
  const chunks: Uint8Array[] = []; let size = 0;
  const reader = response.body!.getReader();
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > 256 * 1024) throw Error('public metadata exceeds size limit');
      chunks.push(value);
    }
  } finally { await reader.cancel(); }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}
function tag(value: unknown): asserts value is string {
  if (typeof value !== 'string') throw Error('invalid public tag');
  releaseVersion(value, value.slice(1));
}
function commit(value: unknown): asserts value is string {
  if (typeof value !== 'string' || !/^[a-f0-9]{40}$/.test(value)) throw Error('invalid public commit');
}
function releaseProtocol(value: string): PublicReleaseIdentity['protocol'] {
  return compareStableVersions(value.slice(1), '0.0.10')! > 0 ? 'record-v1' : 'legacy';
}
async function identity(metadata: any, fetcher: PublicFetch): Promise<PublicReleaseIdentity> {
  tag(metadata.tag_name);
  if (metadata.draft !== false || metadata.prerelease !== false || !Number.isSafeInteger(metadata.id) || metadata.id <= 0) throw Error('release must be public stable');
  const protocol = releaseProtocol(metadata.tag_name);
  if (!Array.isArray(metadata.assets)) throw Error('missing public assets');
  const ids = new Set<number>();
  for (const asset of metadata.assets) {
    if (!Number.isSafeInteger(asset.id) || asset.id <= 0 || ids.has(asset.id)) throw Error('invalid or duplicate public asset ID');
    ids.add(asset.id);
    const max = typeof asset.name === 'string' && asset.name.endsWith('.tar.gz') ? 512 * 1024 * 1024 : 256 * 1024;
    if (typeof asset.name !== 'string' || !Number.isSafeInteger(asset.size) || asset.size <= 0 || asset.size > max) throw Error('invalid public asset');
  }
  if (protocol === 'record-v1' && JSON.stringify(metadata.assets.map((a: any) => a.name).sort()) !== JSON.stringify(expectedAssets(metadata.tag_name))) throw Error('public asset inventory mismatch');
  const source = await publicJson(`commits/${metadata.tag_name}`, fetcher);
  commit(source.sha);
  return { tag: metadata.tag_name, releaseId: metadata.id, commit: source.sha, protocol,
    assets: metadata.assets.map(({ id, name, size }: any) => ({ id, name, size })) };
}
export async function capturePublicIdentity(options: Options, fetcher: PublicFetch = fetch): Promise<SmokeIdentity> {
  commit(options.runnerSha);
  if (!/^\d+\/\d+$/.test(options.run) || !options.platforms.length || options.platforms.some(p => !['Linux', 'macOS', 'Windows'].includes(p)) || new Set(options.platforms).size !== options.platforms.length) throw Error('invalid smoke run identity');
  if (options.tag) tag(options.tag);
  const latest = await identity(await publicJson('releases/latest', fetcher), fetcher);
  let selected = latest;
  if (options.tag && options.tag !== latest.tag) {
    const metadata = await publicJson(`releases/tags/${options.tag}`, fetcher);
    if (metadata.tag_name !== options.tag) throw Error('selected public tag mismatch');
    selected = await identity(metadata, fetcher);
  }
  if ((options.releaseId || options.commit) && (options.releaseId !== String(selected.releaseId) || options.commit !== selected.commit)) throw Error('coordinator public identity mismatch');
  if ((options.expectedLatest && (options.expectedLatest.tag !== latest.tag || options.expectedLatest.commit !== latest.commit)) ||
      (options.expectedSelectedCommit && options.expectedSelectedCommit !== selected.commit)) throw Error('public release identity changed after capture');
  return { schema: 'tapid-public-smoke-identity-v1', run: options.run, runnerSha: options.runnerSha,
    platforms: options.platforms, selected, latest };
}

type InventoryAdapters = {
  fetch: PublicFetch;
  verifyRecord(record: string, signature: string): Promise<void>;
  verifyArchive(archive: string, name: string): Promise<void>;
  templates(commit: string): Promise<InstallerTemplates>;
  onAsset?(asset: VerifiedPublicAsset): Promise<void>;
  onPublicUrl?(url: string): Promise<void>;
};
export type VerifiedPublicAsset = { tag: string; name: string; url: string; size: number; sha256: string };

async function download(url: string, path: string, max: number, fetcher: PublicFetch): Promise<{size: number; sha256: string}> {
  const response = await publicResponse(url, fetcher, 180_000);
  const reader = response.body!.getReader();
  const file = await open(path, 'wx');
  let size = 0; const hash = createHash('sha256');
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > max) throw Error('public download exceeds size limit');
      hash.update(value);
      await file.writeFile(value);
    }
    if (!size) throw Error('empty public download');
  } finally { await reader.cancel(); await file.close(); }
  return { size, sha256: hash.digest('hex') };
}

/** Inspect bytes only; native installer jobs independently execute released binaries. */
export async function verifyPublicInventory(identity: SmokeIdentity, directory: string, adapters: InventoryAdapters): Promise<VerifiedPublicAsset[]> {
  if (identity.selected.tag === identity.latest.tag && (identity.selected.commit !== identity.latest.commit || identity.selected.releaseId !== identity.latest.releaseId)) throw Error('conflicting captured release identity');
  const assets: VerifiedPublicAsset[] = [];
  const releases = identity.selected.tag === identity.latest.tag ? [identity.selected] : [identity.selected, identity.latest];
  for (const release of releases) {
    // Historical protocols retain their existing installer verification paths.
    if (release.protocol === 'legacy') continue;
    const root = join(directory, release.tag);
    await mkdir(root, { recursive: true });
    const base = `https://github.com/LimeTip/tapid/releases/download/${release.tag}`;
    const get = async (name: string) => {
      const url = `${base}/${name}`;
      const result = await download(url, join(root, name), name.endsWith('.tar.gz') ? 512 * 1024 * 1024 : 256 * 1024, adapters.fetch);
      const asset = { tag: release.tag, name, url, ...result };
      assets.push(asset);
      await adapters.onAsset?.(asset);
      await adapters.onPublicUrl?.(url);
      return result;
    };
    await get('tapid-release-v1.tsv');
    await get('tapid-release-v1.tsv.sig');
    await adapters.verifyRecord(join(root, 'tapid-release-v1.tsv'), join(root, 'tapid-release-v1.tsv.sig'));
    // Only authenticated record bytes may participate in subsequent comparisons.
    const record = await readFile(join(root, 'tapid-release-v1.tsv'));
    const signature = await readFile(join(root, 'tapid-release-v1.tsv.sig'));
    for (const target of TARGETS) {
      const name = `tapid-${release.tag.slice(1)}-${target}.tar.gz`;
      await get(name);
      await adapters.verifyArchive(join(root, name), target.includes('windows') ? 'tapid.exe' : 'tapid');
    }
    await get('SHA256SUMS');
    if (!record.equals(Buffer.from(await releaseRecord(root, release.tag.slice(1), base)))) throw Error('signed record does not match public archive bytes or intended version');
    if (!(await readFile(join(root, 'SHA256SUMS'))).equals(Buffer.from(await checksumLines(root, release.tag.slice(1))))) throw Error('public checksum mismatch');
    for (const asset of assets.filter(asset => asset.tag === release.tag)) {
      if (release.assets.find(expected => expected.name === asset.name)?.size !== asset.size) throw Error(`public asset size mismatch: ${asset.name}`);
    }
    for (const route of [release.tag, ...(release.tag === identity.latest.tag ? ['latest'] : [])]) {
      for (const [suffix, expected] of [['', record], ['.sig', signature]] as const) {
        const path = join(root, `route-${route}${suffix}`);
        const url = `https://tapid.dev/releases/v1/${route}.tsv${suffix}`;
        await download(url, path, 256 * 1024, adapters.fetch);
        await adapters.onPublicUrl?.(url);
        if (!(await readFile(path)).equals(expected)) throw Error(`stale or mismatched owned discovery: ${route}${suffix}`);
      }
    }
    const generated = join(root, 'generated');
    await mkdir(generated);
    for (const target of TARGETS) {
      const name = `tapid-${release.tag.slice(1)}-${target}.tar.gz`;
      await copyFile(join(root, name), join(generated, name));
    }
    await renderInstallers(generated, release.tag.slice(1), base, await adapters.templates(release.commit));
    for (const name of ['install.sh', 'install.ps1']) {
      const result = await get(name);
      if (release.assets.find(a => a.name === name)?.size !== result.size) throw Error(`public installer size mismatch: ${name}`);
      const canonical = await readFile(join(generated, name));
      if (!(await readFile(join(root, name))).equals(canonical)) throw Error(`generated installer mismatch: ${name}`);
      if (release.tag === identity.latest.tag) {
        const path = join(root, `website-${name}`);
        const url = `https://tapid.dev/${name}`;
        await download(url, path, 256 * 1024, adapters.fetch);
        await adapters.onPublicUrl?.(url);
        if (!(await readFile(path)).equals(canonical)) throw Error(`website installer mismatch: ${name}`);
      }
    }
  }
  // Website source is maintained outside this checkout. Discover release docs
  // from public navigation rather than guessing a deployment path.
  const homepage = join(directory, 'website.html');
  await download('https://tapid.dev/', homepage, 2 * 1024 * 1024, adapters.fetch);
  await adapters.onPublicUrl?.('https://tapid.dev/');
  const html = await readFile(homepage, 'utf8');
  const routes = [...new Set([...html.matchAll(/href="(\/docs\/releases\/[A-Za-z0-9/_-]+\/)"/g)].map(match => match[1]))];
  if (!routes.length || routes.length > 10) throw Error('missing or excessive public release documentation routes');
  for (const [index, route] of routes.entries()) {
    const url = new URL(route, 'https://tapid.dev').href;
    if (new URL(url).origin !== 'https://tapid.dev') throw Error('unsafe public documentation route');
    const path = join(directory, `documentation-${index}.html`);
    await download(url, path, 2 * 1024 * 1024, adapters.fetch);
    if (!/<(?:html|h1)\b/i.test(await readFile(path, 'utf8'))) throw Error('public release documentation is not HTML');
    await adapters.onPublicUrl?.(url);
  }
  return assets;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [directory, binary] = process.argv.slice(2);
  if (!directory || !binary) throw Error('usage: public_smoke.ts OUTPUT_DIRECTORY NATIVE_VERIFIER');
  await mkdir(directory, { recursive: true });
  const report: { schema: string; status: string; identity?: SmokeIdentity; assets: VerifiedPublicAsset[]; failure?: string; skips: string[]; publicUrls: string[] } = {
    schema: 'tapid-public-smoke-inventory-v1', status: 'running', assets: [], skips: [], publicUrls: [],
  };
  const reportPath = join(directory, 'inventory.json');
  await writeFile(reportPath, JSON.stringify(report, null, 2));
  try {
    const identity = await capturePublicIdentity({ tag: process.env.SELECTED_TAG,
      expectedSelectedCommit: process.env.SELECTED_SHA,
      expectedLatest: { tag: process.env.LATEST_TAG!, commit: process.env.LATEST_SHA! },
      releaseId: process.env.EXPECTED_RELEASE_ID || undefined, commit: process.env.EXPECTED_SOURCE_SHA || undefined,
      runnerSha: process.env.RUNNER_SHA!, run: `${process.env.GITHUB_RUN_ID}/${process.env.GITHUB_RUN_ATTEMPT}`,
      platforms: process.env.EVENT_NAME === 'schedule' ? ['Linux'] : ['Linux', 'macOS', 'Windows'] });
    report.identity = identity;
    await writeFile(join(directory, 'identity.json'), JSON.stringify(identity, null, 2));
    const run = promisify(execFile);
    report.assets = await verifyPublicInventory(identity, directory, {
      fetch,
      verifyRecord: async (record, signature) => { await run(binary, ['__verify-release-record', record, signature], { timeout: 60_000, maxBuffer: 64 * 1024 }); },
      verifyArchive: async (archive, name) => { await run(binary, ['__verify-release-archive', archive, name], { timeout: 180_000, maxBuffer: 64 * 1024 }); },
      templates: templatesForCommit,
      onAsset: async asset => { report.assets.push(asset); await writeFile(reportPath, JSON.stringify(report, null, 2)); },
      onPublicUrl: async url => { report.publicUrls.push(url); await writeFile(reportPath, JSON.stringify(report, null, 2)); },
    });
    for (const release of [identity.selected, identity.latest]) {
      if (release.protocol === 'legacy') report.skips.push(`${release.tag}: inventory uses historical installer checks; current record contract is inapplicable`);
    }
    report.status = 'passed';
  } catch (error) {
    report.status = 'failed';
    // Do not record transport headers, environment variables, or signed redirect URLs.
    report.failure = error instanceof Error ? error.message.slice(0, 2048) : 'public validation failed';
    process.exitCode = 1;
  } finally { await writeFile(reportPath, JSON.stringify(report, null, 2)); }
}

export function aggregateSmoke(inventory: any, reports: any[], jobs: Record<string, string>, run: string, scheduled = false) {
  const failures: string[] = [];
  let identity = inventory?.identity as SmokeIdentity | undefined;
  try {
    if (!identity || identity.schema !== 'tapid-public-smoke-identity-v1' || identity.run !== run) throw Error('invalid identity');
    commit(identity.runnerSha);
    for (const release of [identity.selected, identity.latest]) {
      tag(release.tag); commit(release.commit);
      const protocol = releaseProtocol(release.tag);
      if (!Number.isSafeInteger(release.releaseId) || release.releaseId <= 0 || release.protocol !== protocol) throw Error('invalid release');
    }
    if (JSON.stringify(identity.platforms) !== JSON.stringify(scheduled ? ['Linux'] : ['Linux', 'macOS', 'Windows'])) throw Error('incorrect platform coverage');
  } catch { identity = undefined; failures.push('malformed smoke identity or incorrect coverage'); }
  if (inventory?.schema !== 'tapid-public-smoke-inventory-v1' || inventory.status !== 'passed' || identity?.schema !== 'tapid-public-smoke-identity-v1' || identity.run !== run) failures.push('missing or failed inventory identity');
  for (const name of ['resolve', 'public-readback', 'unix']) if (jobs[name] !== 'success') failures.push(`required job failed: ${name}`);
  if (!identity?.platforms?.includes('Windows') && jobs.windows !== 'skipped') failures.push('unexpected Windows coverage');
  if (identity?.platforms?.includes('Windows') && jobs.windows !== 'success') failures.push('required Windows job failed');
  const checks = ['install_published', 'selected_consumer', 'install_public', 'install_latest', 'latest_consumer', 'upgrade', 'previous_repeat'];
  if (inventory?.publicUrls !== undefined && (!Array.isArray(inventory.publicUrls) || inventory.publicUrls.some((url: unknown) => {
    try { const parsed = new URL(url as string); return typeof url !== 'string' || parsed.protocol !== 'https:' || !!parsed.username || !!parsed.password; } catch { return true; }
  }))) failures.push('malformed public URL evidence');
  if (inventory?.skips !== undefined && (!Array.isArray(inventory.skips) || inventory.skips.some((skip: unknown) => typeof skip !== 'string'))) failures.push('malformed skip evidence');
  if (identity) {
    if (!Array.isArray(inventory.assets)) failures.push('missing asset evidence');
    else for (const release of identity.selected.tag === identity.latest.tag ? [identity.selected] : [identity.selected, identity.latest]) {
      if (release.protocol === 'legacy') continue;
      const assets = inventory.assets.filter((asset: any) => asset?.tag === release.tag);
      if (JSON.stringify(assets.map((asset: any) => asset.name).sort()) !== JSON.stringify(expectedAssets(release.tag)) ||
          assets.some((asset: any) => !/^[a-f0-9]{64}$/.test(asset.sha256) || !Number.isSafeInteger(asset.size) || asset.size <= 0 || asset.url !== `https://github.com/LimeTip/tapid/releases/download/${release.tag}/${asset.name}`)) failures.push(`incomplete asset evidence: ${release.tag}`);
    }
    for (const platform of identity.platforms) {
      const matching = reports.filter(report => report?.platform === platform);
      if (matching.length !== 1) { failures.push(`missing or duplicate platform: ${platform}`); continue; }
      const report = matching[0];
      if (report.schema !== 'tapid-public-smoke-platform-v1' || JSON.stringify(report.identity) !== JSON.stringify(identity)) failures.push(`platform identity mismatch: ${platform}`);
      for (const check of checks) if (report.checks?.[check] !== 'success') failures.push(`${platform}: ${check} did not succeed`);
    }
    if (reports.some(report => !identity.platforms.includes(report?.platform))) failures.push('unexpected platform report');
  }
  return { schema: 'tapid-public-smoke-summary-v1', status: failures.length ? 'failed' : 'passed',
    coverage: identity?.platforms?.includes('Windows') ? 'cross-platform' : 'scheduled Linux only', identity,
    assets: Array.isArray(inventory?.assets) ? inventory.assets : [], publicUrls: Array.isArray(inventory?.publicUrls) ? inventory.publicUrls : [], skips: Array.isArray(inventory?.skips) ? inventory.skips : [], platforms: reports, failures };
}
