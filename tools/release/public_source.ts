import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { PUBLIC_LIMITS, PublicTransport } from './public_transport.ts';
import { taggedCargoMetadata } from './public_native.ts';
import type { CargoMetadata } from './publish.ts';

/** Materialize only manifests, locks and Rust targets; never fetch Cargo configuration or execute tagged code. */
export async function withPublicSource<T>(
  transport: PublicTransport, commit: string,
  inspect: (metadata: CargoMetadata, directory: string) => Promise<T>,
): Promise<T> {
  if (!/^[a-f0-9]{40}$/.test(commit)) throw Error('invalid tagged source commit');
  const response = await transport.json(`https://api.github.com/repos/LimeTip/tapid/git/trees/${commit}?recursive=1`);
  if (!response || typeof response !== 'object' || Array.isArray(response)) throw Error('malformed tagged source tree');
  const { tree, truncated } = response as Record<string, unknown>;
  if (truncated !== false || !Array.isArray(tree) || tree.length > PUBLIC_LIMITS.sourceEntries) throw Error('incomplete or oversized tagged source tree');
  const files: { path: string; size: number }[] = [];
  const seen = new Set<string>();
  let materialized = 0;
  for (const entry of tree) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) throw Error('malformed tagged source entry');
    const { path, mode, type, size } = entry as Record<string, unknown>;
    if (typeof path !== 'string' || !/^(?:[A-Za-z0-9_.-]+\/)*[A-Za-z0-9_.-]+$/.test(path)
      || path.split('/').some(part => part === '.' || part === '..') || seen.has(path)) throw Error('unsafe or duplicate tagged source path');
    seen.add(path);
    if (!/(?:^|\/)(?:Cargo\.toml|Cargo\.lock)$|\.rs$/.test(path)) continue;
    if (type !== 'blob' || !['100644', '100755'].includes(String(mode)) || typeof size !== 'number' || !Number.isSafeInteger(size) || size < 0 || size > PUBLIC_LIMITS.apiBytes) throw Error('invalid tagged source file');
    materialized += size;
    if (materialized > PUBLIC_LIMITS.sourceMaterializedBytes) throw Error('tagged source materialization limit exceeded');
    files.push({ path, size });
  }
  if (!files.some(file => file.path === 'Cargo.toml') || !files.some(file => file.path === 'Cargo.lock')) throw Error('tagged source lacks root Cargo manifest or lockfile');
  files.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  const directory = await mkdtemp(join(tmpdir(), 'tapid-tagged-source-'));
  try {
    const source = join(directory, 'source');
    await mkdir(source);
    for (const file of files) {
      const bytes = await transport.bytes(`https://raw.githubusercontent.com/LimeTip/tapid/${commit}/${file.path}`, Math.max(1, file.size));
      if (bytes.length !== file.size) throw Error('tagged source file size mismatch');
      const destination = join(source, file.path);
      await mkdir(dirname(destination), { recursive: true });
      await writeFile(destination, bytes, { flag: 'wx' });
    }
    const metadata = await taggedCargoMetadata(source, join(directory, 'cargo-home'), () => transport.remainingMs());
    return await inspect(metadata, source);
  } finally { await rm(directory, { recursive: true, force: true }); }
}
