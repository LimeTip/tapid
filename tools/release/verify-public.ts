import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { releaseVersion } from './release.ts';
import { compareStableVersions } from './versions.ts';
import { PublicTransport } from './public_transport.ts';
import { verifyPublicSmoke, type SmokeEvidence } from './public_smoke.ts';
import { verifyPublicAssets, type AssetObservation } from './public_assets.ts';

type Check = { name: string; status: 'pass' | 'fail' | 'not-performed'; detail?: string };
export type PublicReport = {
  schema: 'tapid-public-verification-v1';
  tag: string;
  ok: boolean;
  contract?: 'historical-checksums' | 'signed-record-v1';
  identity?: { tagObject: string; commit: string; releaseId: number };
  smoke?: SmokeEvidence;
  assets?: AssetObservation[];
  checks: Check[];
};
const API = 'https://api.github.com/repos/LimeTip/tapid';
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('malformed public identity');
  return value as Record<string, unknown>;
}
function sha(value: unknown): string {
  if (typeof value !== 'string' || !/^[a-f0-9]{40}$/.test(value)) throw Error('invalid public Git object identity');
  return value;
}
function id(value: unknown): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) throw Error('invalid public release identity');
  return value;
}
function failureDetail(error: unknown): string {
  // OS failures may include transient paths. Provider bodies and headers never
  // become diagnostics in the public transport.
  if (!(error instanceof Error) || 'code' in error) return 'local verification operation failed';
  return error.message;
}

/** Independent public read-back; no release mutation adapter is reachable here. */
export async function verifyPublicRelease(tag: string, options: { transport?: PublicTransport } = {}): Promise<PublicReport> {
  const report: PublicReport = { schema: 'tapid-public-verification-v1', tag, ok: false, checks: [] };
  try {
    releaseVersion(tag, tag.slice(1));
    report.contract = compareStableVersions(tag.slice(1), '0.0.10')! <= 0 ? 'historical-checksums' : 'signed-record-v1';
    report.checks.push({ name: 'tag', status: 'pass' });
  } catch {
    report.checks.push({ name: 'tag', status: 'fail', detail: 'expected canonical stable vX.Y.Z with unsigned 64-bit components' });
    for (const name of ['identity', 'assets', 'integrity', 'smoke', 'packages']) report.checks.push({ name, status: 'not-performed' });
    return report;
  }
  const transport = options.transport ?? new PublicTransport();
  let releaseAssets: unknown;
  let publishedAt: unknown;
  try {
    const ref = record(await transport.json(`${API}/git/ref/tags/${tag}`));
    const refObject = record(ref.object);
    if (ref.ref !== `refs/tags/${tag}` || refObject.type !== 'tag') throw Error('release requires an exact annotated tag');
    const tagObject = sha(refObject.sha);
    const annotated = record(await transport.json(`${API}/git/tags/${tagObject}`));
    const target = record(annotated.object);
    if (annotated.sha !== tagObject || annotated.tag !== tag || target.type !== 'commit') throw Error('inconsistent annotated tag or unsupported tag indirection');
    const commit = sha(target.sha);
    const releases = (await transport.list(`${API}/releases`)).map(record);
    const ids = new Set<number>();
    for (const release of releases) {
      const releaseId = id(release.id);
      if (ids.has(releaseId)) throw Error('duplicate public release identity');
      ids.add(releaseId);
      if (typeof release.tag_name !== 'string') throw Error('malformed public release tag');
    }
    const matching = releases.filter(release => release.tag_name === tag);
    if (matching.length !== 1) throw Error('expected exactly one matching public release');
    const release = matching[0];
    if (release.draft !== false || release.prerelease !== false || release.immutable !== true) throw Error('release must be public, stable, and immutable');
    report.identity = { tagObject, commit, releaseId: id(release.id) };
    releaseAssets = release.assets;
    publishedAt = release.published_at;
    report.checks.push({ name: 'identity', status: 'pass' });
  } catch (error) {
    report.checks.push({ name: 'identity', status: 'fail', detail: failureDetail(error) });
  }
  if (report.identity) {
    try {
      report.assets = await verifyPublicAssets(transport, tag, releaseAssets, async () => {});
      report.checks.push({ name: 'assets', status: 'pass' });
    } catch (error) {
      report.checks.push({ name: 'assets', status: 'fail', detail: failureDetail(error) });
    }
  } else report.checks.push({ name: 'assets', status: 'not-performed' });
  report.checks.push({ name: 'integrity', status: 'not-performed' });
  if (report.identity) {
    try {
      if (typeof publishedAt !== 'string') throw Error('missing public release publication time');
      report.smoke = await verifyPublicSmoke(transport, { tag, ...report.identity, publishedAt });
      report.checks.push({ name: 'smoke', status: 'pass' });
    } catch (error) {
      report.checks.push({ name: 'smoke', status: 'fail', detail: failureDetail(error) });
    }
  } else report.checks.push({ name: 'smoke', status: 'not-performed' });
  report.checks.push({ name: 'packages', status: 'not-performed' });
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  if (args.length < 1 || args.length > 2 || args.length === 2 && args[1] !== '--json' || args[0].startsWith('-')) {
    console.error('usage: node --experimental-strip-types tools/release/verify-public.ts TAG [--json]');
    process.exitCode = 2;
  } else {
    const report = await verifyPublicRelease(args[0]);
    const summary = `Public verification ${report.tag}: ${report.ok ? 'passed' : 'incomplete or failed'}; ${report.checks.map(check => `${check.name}=${check.status}${check.detail ? ` (${check.detail})` : ''}`).join(', ')}`;
    if (args[1] === '--json') { console.log(JSON.stringify(report, null, 2)); console.error(summary); }
    else console.log(summary);
    process.exitCode = report.ok ? 0 : 1;
  }
}
