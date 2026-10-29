import { createHash } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expectedAssets } from './candidate.ts';
import { TARGETS, checksumLines, releaseVersion } from './release.ts';
import { compareStableVersions } from './versions.ts';
import { PUBLIC_LIMITS, PublicTransport } from './public_transport.ts';

export type AssetObservation = { id: number; name: string; size: number; sha256: string; digestEvidence: 'provider-checksum' | 'authenticated-record' | 'observed' };
type Download = { id: number; name: string; size: number; url: string };

/** Owns temporary bytes until all offline inspection consumers have completed. */
export async function verifyPublicAssets(
  transport: PublicTransport,
  tag: string,
  input: unknown,
  inspect: (evidence: { directory: string; assets: AssetObservation[] }) => Promise<void>,
): Promise<AssetObservation[]> {
  const version = releaseVersion(tag, tag.slice(1));
  const archives = TARGETS.map(target => `tapid-${version}-${target}.tar.gz`);
  const modern = compareStableVersions(version, '0.0.10')! > 0;
  const names = modern ? expectedAssets(tag) : [...archives, 'SHA256SUMS'].sort();
  if (!Array.isArray(input) || input.length !== names.length) throw Error('release asset set differs from selected contract');
  const ids = new Set<number>(), seen = new Set<string>();
  const downloads: Download[] = input.map(value => {
    if (!value || typeof value !== 'object') throw Error('malformed public asset');
    const { id, name, size, browser_download_url: url } = value;
    if (!Number.isSafeInteger(id) || id <= 0 || ids.has(id) || typeof name !== 'string' || !names.includes(name) || seen.has(name)) throw Error('unexpected or duplicate public asset identity');
    ids.add(id); seen.add(name);
    const limit = archives.includes(name) ? PUBLIC_LIMITS.archiveBytes : PUBLIC_LIMITS.metadataBytes;
    if (!Number.isSafeInteger(size) || size <= 0 || size > limit) throw Error('invalid provider asset size');
    if (url !== `https://github.com/LimeTip/tapid/releases/download/${tag}/${name}`) throw Error('asset URL does not match immutable release route');
    return { id, name, size, url };
  }).sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
  const directory = await mkdtemp(join(tmpdir(), 'tapid-public-'));
  try {
    const assets: AssetObservation[] = [];
    let checksums: Uint8Array | undefined;
    for (const asset of downloads) {
      const bytes = await transport.bytes(asset.url, asset.size);
      if (bytes.length !== asset.size) throw Error('download differs from provider asset size');
      await writeFile(join(directory, asset.name), bytes, { flag: 'wx' });
      if (asset.name === 'SHA256SUMS') checksums = bytes;
      assets.push({ id: asset.id, name: asset.name, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'), digestEvidence: archives.includes(asset.name) ? 'provider-checksum' : 'observed' });
    }
    // Reuse the canonical generator: this checks names, duplicate/extra rows,
    // whitespace and every archive digest, rather than accepting a loose subset.
    if (!checksums || !Buffer.from(checksums).equals(Buffer.from(await checksumLines(directory, version)))) throw Error('release SHA256SUMS mismatch');
    await inspect({ directory, assets });
    return assets;
  } finally { await rm(directory, { recursive: true, force: true }); }
}
