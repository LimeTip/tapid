import { lstatSync, readdirSync } from "node:fs";
import { join } from "node:path";

// No symlink traversal. Inode deduplication is scoped to this inventory, not the
// whole filesystem; allocated blocks cannot reveal compression or CoW sharing.
function inventory(roots: string[]) {
  const seen = new Set<string>();
  let logicalBytes = 0,
    allocatedBytes = 0,
    regularFiles = 0,
    symlinks = 0;
  let blocksAvailable = true;
  function visit(path: string) {
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) {
      symlinks++;
      return;
    }
    if (stat.isDirectory()) {
      for (const name of readdirSync(path)) visit(join(path, name));
    } else if (stat.isFile()) {
      regularFiles++;
      const identity = `${stat.dev}:${stat.ino}`;
      if (seen.has(identity)) return;
      seen.add(identity);
      logicalBytes += stat.size;
      if (typeof stat.blocks === "number") allocatedBytes += stat.blocks * 512;
      else blocksAvailable = false;
    }
  }
  for (const root of roots) visit(root);
  return {
    logicalBytes,
    allocatedBytes: blocksAvailable ? allocatedBytes : null,
    regularFiles,
    uniqueFiles: seen.size,
    symlinks,
  };
}
export function diskSnapshot(project: string, cache: string) {
  return {
    project: inventory([project]),
    cache: inventory([cache]),
    combined: inventory([project, cache]),
  };
}
