import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { releaseRecord } from './release.ts';

function replaceOnce(source: string, marker: string, value: string): string {
  if (source.split(marker).length !== 2) throw new Error(`installer must contain exactly one bootstrap marker: ${marker}`);
  return source.replace(marker, value);
}

export type InstallerTemplates = { 'install.sh': string; 'install.ps1': string };

export async function renderInstallers(directory: string, version: string, baseUrl: string, templates?: InstallerTemplates): Promise<void> {
  // Reuse the record generator's exact target set, byte hashes, sizes and safe URL validation.
  const record = await releaseRecord(directory, version, baseUrl);
  const rows = record.trimEnd().split('\n').slice(1).map(line => line.split('\t'));
  const base = baseUrl.replace(/\/+$/, '');
  // Quotes and substitution metacharacters must never become executable template text.
  if (!/^https:\/\/[A-Za-z0-9][A-Za-z0-9.:-]*(?:\/[A-Za-z0-9._/-]*)?$/.test(base)) {
    throw new Error('bootstrap release URL must contain only safe literal URL characters');
  }
  const shellPins = rows.map(([target, , , hash]) => `    ${target}) printf '%s\\n' '${hash}' ;;`).join('\n');
  const windowsPins = rows.map(([target, , , hash]) => `    '${target}' = '${hash}'`).join('\n');
  for (const name of ['install.sh', 'install.ps1']) {
    let source = templates?.[name as keyof InstallerTemplates] ?? await readFile(new URL(`../../scripts/${name}`, import.meta.url), 'utf8');
    source = replaceOnce(source, '@TAPID_BOOTSTRAP_VERSION@', version);
    source = replaceOnce(source, '@TAPID_BOOTSTRAP_BASE_URL@', base);
    source = replaceOnce(source, '# @TAPID_BOOTSTRAP_PINS@', name.endsWith('.sh') ? shellPins : windowsPins);
    await writeFile(join(directory, name), source);
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const args = process.argv.slice(2);
  if (args.length !== 3) throw new Error('usage: bootstrap.ts DIRECTORY VERSION RELEASE_BASE_URL');
  await renderInstallers(args[0], args[1], args[2]);
}
