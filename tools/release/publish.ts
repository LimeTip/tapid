import { execFile } from "node:child_process";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);
type Dependency = string | {
  name: string;
  source?: string | null;
  kind?: string | null;
  path?: string | null;
};
type MetadataPackage = {
  name: string;
  version: string;
  dependencies?: Dependency[];
  publish?: string[] | null;
};
type CargoMetadata = { packages: MetadataPackage[] };
type Package = { name: string; version: string };

/** Returns local non-dev dependencies that must be published before this package. */
function internalDependencies(pkg: MetadataPackage): string[] {
  return (pkg.dependencies ?? []).flatMap((dependency) => {
    if (typeof dependency === "string") return [dependency];
    return dependency.source === null && dependency.kind !== "dev" ? [dependency.name] : [];
  });
}

/** Reports whether Cargo permits this package to be published to crates.io. */
function publishableToCratesIo(pkg: MetadataPackage): boolean {
  return pkg.publish === undefined || pkg.publish === null || pkg.publish.includes("crates-io");
}

/**
 * Builds a deterministic, dependency-first plan for missing crates.io versions.
 * Packages restricted to other registries are excluded, and `tapid` is ordered last.
 * Throws when metadata is incomplete, cyclic, or requires an unpublishable local dependency.
 */
export function publicationPlan(metadata: CargoMetadata, published: Set<string>): Package[] {
  const packages = new Map(metadata.packages.map((pkg) => [pkg.name, pkg]));
  if (!packages.has("tapid")) throw new Error("cargo metadata is missing publishable package tapid");
  const visiting = new Set<string>();
  const visited = new Set<string>();
  const ordered: Package[] = [];

  function visit(name: string): void {
    if (visiting.has(name)) throw new Error(`workspace dependency cycle includes ${name}`);
    if (visited.has(name)) return;
    const pkg = packages.get(name);
    if (!pkg) throw new Error(`cargo metadata is missing publishable package ${name}`);
    if (!publishableToCratesIo(pkg)) {
      throw new Error(`workspace dependency ${name} is not publishable to crates.io`);
    }
    visiting.add(name);
    for (const dependency of internalDependencies(pkg)) visit(dependency);
    visiting.delete(name);
    visited.add(name);
    if (!published.has(`${pkg.name}@${pkg.version}`)) {
      ordered.push({ name: pkg.name, version: pkg.version });
    }
  }

  const roots = [...packages.values()]
    .filter(publishableToCratesIo)
    .map((pkg) => pkg.name)
    .filter((name) => name !== "tapid")
    .sort();
  for (const name of roots) visit(name);
  visit("tapid");
  return ordered;
}

async function cargoMetadata(): Promise<CargoMetadata> {
  const { stdout } = await execFileAsync(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1", "--locked"],
    { encoding: "utf8", maxBuffer: 10 * 1024 * 1024 },
  );
  return JSON.parse(stdout);
}

/** Queries crates.io for one exact package version with bounded transient retries. */
export async function isPublished(
  pkg: Package,
  fetchFn: typeof fetch = fetch,
  sleepFn: (milliseconds: number) => Promise<void> = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
): Promise<boolean> {
  const url = `https://crates.io/api/v1/crates/${encodeURIComponent(pkg.name)}/${encodeURIComponent(pkg.version)}`;
  for (let attempt = 1; attempt <= 3; attempt++) {
    let response: Response;
    try {
      response = await fetchFn(url, {
        headers: { "User-Agent": "tapid-release-workflow (https://github.com/LimeTip/tapid)" },
        signal: AbortSignal.timeout(10_000),
      });
    } catch (error) {
      if (attempt === 3) throw error;
      await sleepFn(attempt * 1_000);
      continue;
    }
    if (response.status === 200) return true;
    if (response.status === 404) return false;
    const transient = response.status === 429 || response.status >= 500;
    if (!transient || attempt === 3) {
      throw new Error(`crates.io returned HTTP ${response.status} for ${pkg.name} ${pkg.version}`);
    }
    await sleepFn(attempt * 1_000);
  }
  throw new Error(`crates.io lookup retries exhausted for ${pkg.name} ${pkg.version}`);
}

async function waitUntilPublished(pkg: Package): Promise<void> {
  for (let attempt = 1; attempt <= 20; attempt++) {
    if (await isPublished(pkg)) return;
    await new Promise((resolve) => setTimeout(resolve, 3_000));
  }
  throw new Error(`${pkg.name} ${pkg.version} was not visible on crates.io after 60 seconds`);
}

async function main(): Promise<void> {
  const metadata = await cargoMetadata();
  const published = new Set<string>();
  const candidates = publicationPlan(metadata, published);
  for (const pkg of candidates) {
    if (await isPublished(pkg)) published.add(`${pkg.name}@${pkg.version}`);
  }

  const plan = publicationPlan(metadata, published);
  if (process.argv.includes("--dry-run")) {
    for (const pkg of plan) console.log(`${pkg.name} ${pkg.version}`);
    return;
  }

  for (const pkg of plan) {
    console.log(`Publishing ${pkg.name} ${pkg.version}`);
    await execFileAsync("cargo", ["publish", "-p", pkg.name, "--locked"], {
      encoding: "utf8",
      maxBuffer: 10 * 1024 * 1024,
    });
    await waitUntilPublished(pkg);
  }
}

const isMain = process.argv[1] !== undefined && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
if (isMain) await main();
