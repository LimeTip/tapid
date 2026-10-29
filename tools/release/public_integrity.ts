import { link, mkdtemp, readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { renderInstallers, type InstallerTemplates } from './bootstrap.ts';
import { releaseRecord, releaseVersion, TARGETS } from './release.ts';
import { PUBLIC_LIMITS, PublicTransport } from './public_transport.ts';
import type { PublicVerifier } from './public_native.ts';

/** Independent read-back of the immutable signed-record contract, without execution. */
export async function verifyPublicIntegrity(
  transport: PublicTransport,
  verifier: PublicVerifier,
  evidence: { directory: string; tag: string; commit: string },
): Promise<void> {
  const { directory, tag, commit } = evidence;
  const version = releaseVersion(tag, tag.slice(1));
  if (!/^[a-f0-9]{40}$/.test(commit)) throw Error('invalid tagged template commit');
  const recordName = 'tapid-release-v1.tsv';
  await verifier.verifySignature(join(directory, recordName), join(directory, `${recordName}.sig`));
  const base = `https://github.com/LimeTip/tapid/releases/download/${tag}`;
  // Exact comparison with the publisher's pure generator enforces every row,
  // order, target, version, immutable URL, size and digest without a second parser.
  const record = await readFile(join(directory, recordName));
  if (!record.equals(Buffer.from(await releaseRecord(directory, version, base)))) throw Error('release record differs from exact downloaded archive contract');
  for (const suffix of ['', '.sig']) {
    const owned = await transport.bytes(`https://tapid.dev/releases/v1/${tag}.tsv${suffix}`, PUBLIC_LIMITS.metadataBytes);
    if (!Buffer.from(owned).equals(await readFile(join(directory, `${recordName}${suffix}`)))) throw Error('owned release metadata differs from immutable asset');
  }
  const templates = {} as InstallerTemplates;
  for (const name of ['install.sh', 'install.ps1'] as const) {
    const bytes = await transport.bytes(`https://raw.githubusercontent.com/LimeTip/tapid/${commit}/scripts/${name}`, PUBLIC_LIMITS.metadataBytes);
    try { templates[name] = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
    catch { throw Error('invalid tagged installer template encoding'); }
  }
  const regenerated = await mkdtemp(join(directory, 'regenerated-'));
  try {
    for (const target of TARGETS) {
      const name = `tapid-${version}-${target}.tar.gz`;
      await link(join(directory, name), join(regenerated, name));
    }
    await renderInstallers(regenerated, version, base, templates);
    for (const name of ['install.sh', 'install.ps1']) {
      if (!(await readFile(join(directory, name))).equals(await readFile(join(regenerated, name)))) throw Error('generated installer differs from tagged template and release pins');
    }
  } finally { await rm(regenerated, { recursive: true, force: true }); }
}
