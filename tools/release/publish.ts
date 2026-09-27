import { execFile } from "node:child_process";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);
export type Dependency = string | {
  name: string;
  source?: string | null;
  kind?: string | null;
  path?: string | null;
};
export type MetadataPackage = {
  name: string;
  version: string;
  dependencies?: Dependency[];
  publish?: string[] | null;
  packageVerificationError?: string;
};
export type CargoMetadata = { packages: MetadataPackage[]; lockfiles?: string[] };
type Package = { name: string; version: string };

/** Returns local non-dev dependencies that must be published before this package. */
function internalDependencies(pkg: MetadataPackage): string[] {
  return (pkg.dependencies ?? []).flatMap((dependency) => {
    if (typeof dependency === "string") return [dependency];
    return dependency.source === null && dependency.kind !== "dev" ? [dependency.name] : [];
  }).sort();
}

/** Reports whether Cargo permits this package to be published to crates.io. */
function publishableToCratesIo(pkg: MetadataPackage): boolean {
  return pkg.publish === undefined || pkg.publish === null || pkg.publish.includes("crates-io");
}

/**
 * Builds a deterministic, dependency-first plan for missing crates.io versions.
 * Packages restricted to other registries are excluded, and `tapid` is ordered last.
 * Throws when metadata is incomplete, cyclic, or requires an unpublished local dependency that cannot be published.
 */
export function publicationPlan(metadata: CargoMetadata, published: Set<string>): Package[] {
  const packages = new Map(metadata.packages.map((pkg) => [pkg.name, pkg]));
  if (!packages.has("tapid")) throw new Error("cargo metadata is missing publishable package tapid");
  const visiting = new Set<string>();
  const visited = new Set<string>();
  const ordered: Package[] = [];

  /** Visits one package, adding local dependencies first and rejecting cycles. */
  function visit(name: string): void {
    if (visited.has(name)) return;
    const pkg = packages.get(name);
    if (!pkg) throw new Error(`cargo metadata is missing publishable package ${name}`);
    if (published.has(`${pkg.name}@${pkg.version}`)) {
      visited.add(name);
      return;
    }
    if (visiting.has(name)) throw new Error(`workspace dependency cycle includes ${name}`);
    if (!publishableToCratesIo(pkg)) {
      throw new Error(`workspace dependency ${name} is not publishable to crates.io`);
    }
    visiting.add(name);
    for (const dependency of internalDependencies(pkg)) visit(dependency);
    visiting.delete(name);
    visited.add(name);
    ordered.push({ name: pkg.name, version: pkg.version });
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

export type PublicationPlan = {
  packages: Package[];
  missing: Package[];
  drift: { name: string; localVersion: string; publicVersion: string }[];
  dependentBumps: { dependent: string; dependency: string; requiredVersion: string }[];
  lockfiles: string[];
  blockers: string[];
  verification: string[];
  recovery: string;
};
export type RegistryLookup = (pkg: Package) => Promise<boolean | { published: boolean; latestVersion?: string }>;

/** Computes a read-only plan from exact registry lookups. */
export async function planPublication(metadata: CargoMetadata, isInRegistry: RegistryLookup): Promise<PublicationPlan> {
  // Query every metadata package before planning so a locally unpublishable
  // stand-in can be recognized as an already-published registry dependency.
  const candidates = [...metadata.packages]
    .sort((left, right) => left.name.localeCompare(right.name))
    .map(({ name, version }) => ({ name, version }));
  const published = new Set<string>();
  const missing: Package[] = [];
  const drift: PublicationPlan["drift"] = [];
  const blockers: string[] = [];
  for (const pkg of candidates) {
    try {
      const response = await isInRegistry(pkg);
      const state = typeof response === "boolean" ? { published: response } : response;
      if (state.published) published.add(`${pkg.name}@${pkg.version}`);
      else {
        missing.push(pkg);
        if (state.latestVersion && state.latestVersion !== pkg.version) {
          drift.push({ name: pkg.name, localVersion: pkg.version, publicVersion: state.latestVersion });
        }
      }
    } catch (error) {
      blockers.push(`${pkg.name}@${pkg.version}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  for (const pkg of metadata.packages) {
    if (pkg.packageVerificationError) blockers.push(`${pkg.name}: ${pkg.packageVerificationError}`);
    for (const dependency of pkg.dependencies ?? []) {
      if (typeof dependency !== "string" && dependency.path && dependency.source) {
        blockers.push(`${pkg.name}: local path dependency ${dependency.name} has registry source ${dependency.source}`);
      }
    }
  }
  let packages: Package[] = [];
  if (blockers.length === 0) {
    try { packages = publicationPlan(metadata, published); }
    catch (error) { blockers.push(error instanceof Error ? error.message : String(error)); }
  }
  const packageNames = new Set(packages.map((pkg) => pkg.name));
  const dependentBumps: PublicationPlan["dependentBumps"] = [];
  for (const pkg of metadata.packages) {
    for (const dependency of internalDependencies(pkg)) {
      const local = metadata.packages.find((candidate) => candidate.name === dependency);
      if (local && packageNames.has(dependency) && packageNames.has(pkg.name)) {
        dependentBumps.push({ dependent: pkg.name, dependency, requiredVersion: local.version });
      }
    }
  }
  dependentBumps.sort((a, b) => `${a.dependent}\0${a.dependency}`.localeCompare(`${b.dependent}\0${b.dependency}`));
  const lockfiles = [...(metadata.lockfiles ?? [])].sort();
  const verification = lockfiles.length === 0
    ? ["cargo package --workspace --locked", "cargo metadata --manifest-path tests/integration/Cargo.toml --locked --format-version 1", ...packages.map((pkg) => `cargo package -p ${pkg.name} --locked`)]
    : [
      ...packages.map((pkg) => `cargo package -p ${pkg.name} --locked`),
      "cargo metadata --locked --format-version 1",
      ...lockfiles.filter((lockfile) => lockfile !== "Cargo.lock").map((lockfile) => `cargo metadata --manifest-path ${lockfile.replace(/Cargo\.lock$/, "Cargo.toml")} --locked --format-version 1`),
    ];
  return {
    packages,
    missing: missing.sort((a, b) => `${a.name}\0${a.version}`.localeCompare(`${b.name}\0${b.version}`)),
    drift: drift.sort((a, b) => a.name.localeCompare(b.name)),
    dependentBumps,
    lockfiles,
    blockers,
    verification,
    recovery: "Record confirmed package versions, query crates.io again, and rerun this dry-run; resume only with the remaining dependency-ordered suffix.",
  };
}

export function renderMachinePlan(plan: PublicationPlan): string {
  return `${JSON.stringify(plan, null, 2)}\n`;
}

export function renderHumanPlan(plan: PublicationPlan): string {
  if (plan.blockers.length > 0) return `Publication blocked:\n${plan.blockers.map((blocker) => `- ${blocker}`).join("\n")}\n`;
  if (plan.packages.length === 0) return "No crates.io packages require publication.\n";
  return [
    "Crates.io publication plan:",
    ...plan.packages.map((pkg, index) => `${index + 1}. ${pkg.name} ${pkg.version}`),
    ...(plan.drift.length > 0 ? ["Version drift:", ...plan.drift.map((item) => `- ${item.name}: local ${item.localVersion}, public ${item.publicVersion}`)] : []),
    ...(plan.dependentBumps.length > 0 ? ["Dependent bumps:", ...plan.dependentBumps.map((item) => `- ${item.dependent} requires ${item.dependency} ${item.requiredVersion}`)] : []),
    "Verification:",
    ...plan.verification.map((command) => `- ${command}`),
    ...(plan.lockfiles.length > 0 ? ["Lockfiles requiring regeneration:", ...plan.lockfiles.map((lockfile) => `- ${lockfile}`)] : []),
    `Recovery: ${plan.recovery}`,
    "",
  ].join("\n");
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

/** Reads the public version state without attempting publication or mutation. */
export async function registryState(pkg: Package, fetchFn: typeof fetch = fetch): Promise<{ published: boolean; latestVersion?: string }> {
  const response = await fetchFn(`https://crates.io/api/v1/crates/${encodeURIComponent(pkg.name)}`, {
    headers: { "User-Agent": "tapid-release-workflow (https://github.com/LimeTip/tapid)" },
    signal: AbortSignal.timeout(10_000),
  });
  if (response.status === 404) return { published: false };
  if (!response.ok) throw new Error(`crates.io returned HTTP ${response.status} for ${pkg.name}`);
  const body = await response.json() as { crate?: { max_version?: string }; versions?: { num?: string }[] };
  const versions = new Set((body.versions ?? []).flatMap((version) => version.num ? [version.num] : []));
  return { published: versions.has(pkg.version), latestVersion: body.crate?.max_version };
}

async function main(): Promise<void> {
  const metadata = await cargoMetadata();
  const plan = await planPublication(metadata, (pkg) => registryState(pkg));
  console.log(process.argv.includes("--json") ? renderMachinePlan(plan) : renderHumanPlan(plan));
  if (plan.blockers.length > 0) process.exitCode = 2;
}

const isMain = process.argv[1] !== undefined && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
if (isMain) await main();
