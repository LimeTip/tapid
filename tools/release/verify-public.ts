import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { releaseVersion, TARGETS } from './release.ts';
import { buildPublicVerifier, type PublicVerifier } from './public_native.ts';
import { verifyPublicIntegrity } from './public_integrity.ts';
import { withPublicSource } from './public_source.ts';
import { verifyPublicPackages, type PublicPackage } from './public_packages.ts';
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
  packages?: PublicPackage[];
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
export async function verifyPublicRelease(tag: string, options: { transport?: PublicTransport; verifier?: (remainingMs: () => number) => Promise<PublicVerifier> } = {}): Promise<PublicReport> {
  const report: PublicReport = { schema: 'tapid-public-verification-v1', tag, ok: false, checks: [] };
  try {
    releaseVersion(tag, tag.slice(1));
    report.contract = compareStableVersions(tag.slice(1), '0.0.10')! <= 0 ? 'historical-checksums' : 'signed-record-v1';
    report.checks.push({ name: 'tag', status: 'pass' });
  } catch {
    report.checks.push({ name: 'tag', status: 'fail', detail: 'expected canonical stable vX.Y.Z with unsigned 64-bit components' });
    for (const name of ['identity', 'assets', 'archives', 'integrity', 'smoke', 'packages']) report.checks.push({ name, status: 'not-performed' });
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
  let archives: Check = { name: 'archives', status: 'not-performed' };
  let integrity: Check = { name: 'integrity', status: 'not-performed' };
  if (report.identity) {
    try {
      report.assets = await verifyPublicAssets(transport, tag, releaseAssets, async ({ directory }) => {
        let verifier: PublicVerifier | undefined;
        try {
          verifier = await (options.verifier ?? buildPublicVerifier)(() => transport.remainingMs());
          for (const target of TARGETS) {
            await verifier.inspectArchive(join(directory, `tapid-${tag.slice(1)}-${target}.tar.gz`), target.includes('windows') ? 'tapid.exe' : 'tapid');
          }
          archives = { name: 'archives', status: 'pass' };
        } catch (error) {
          archives = { name: 'archives', status: 'fail', detail: failureDetail(error) };
        }
        if (report.contract === 'historical-checksums') {
          integrity = { name: 'integrity', status: 'pass', detail: 'historical same-provider checksums; no independent signature' };
        } else if (verifier) {
          try {
            await verifyPublicIntegrity(transport, verifier, { directory, tag, commit: report.identity!.commit });
            integrity = { name: 'integrity', status: 'pass' };
          } catch (error) {
            integrity = { name: 'integrity', status: 'fail', detail: failureDetail(error) };
          }
        }
      });
      if (integrity.status === 'pass' && report.contract === 'signed-record-v1') {
        for (const asset of report.assets) if (asset.name.endsWith('.tar.gz')) asset.digestEvidence = 'authenticated-record';
      }
      report.checks.push({ name: 'assets', status: 'pass' });
    } catch (error) {
      report.checks.push({ name: 'assets', status: 'fail', detail: failureDetail(error) });
    }
  } else report.checks.push({ name: 'assets', status: 'not-performed' });
  report.checks.push(archives);
  report.checks.push(integrity);
  if (report.identity) {
    try {
      if (typeof publishedAt !== 'string') throw Error('missing public release publication time');
      report.smoke = await verifyPublicSmoke(transport, { tag, ...report.identity, publishedAt });
      report.checks.push({ name: 'smoke', status: 'pass' });
    } catch (error) {
      report.checks.push({ name: 'smoke', status: 'fail', detail: failureDetail(error) });
    }
  } else report.checks.push({ name: 'smoke', status: 'not-performed' });
  if (report.identity) {
    try {
      report.packages = await withPublicSource(transport, report.identity.commit, async metadata => {
        const cli = metadata.packages.find(pkg => pkg.name === 'tapid');
        if (!cli) throw Error('tagged Cargo metadata lacks tapid');
        releaseVersion(tag, cli.version);
        return verifyPublicPackages(transport, metadata);
      });
      report.checks.push({ name: 'packages', status: 'pass' });
    } catch (error) {
      report.checks.push({ name: 'packages', status: 'fail', detail: failureDetail(error) });
    }
  } else report.checks.push({ name: 'packages', status: 'not-performed' });
  report.ok = report.checks.every(check => check.status === 'pass');
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
