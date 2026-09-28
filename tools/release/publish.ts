import { execFile } from "node:child_process";
import { readdir, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, relative, resolve, sep } from "node:path";
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

const ignoredLockfileDirectories = new Set([".git", "node_modules", "target", ".worktrees", "worktrees"]);

/** Returns Cargo.lock files paired with a manifest, relative to the workspace tree. */
export async function findCargoLockfiles(workspaceDir: string): Promise<string[]> {
  const root = resolve(workspaceDir);
  const lockfiles: string[] = [];
  async function visit(directory: string): Promise<void> {
    const entries = await readdir(directory, { withFileTypes: true });
    const files = new Set(entries.filter((entry) => entry.isFile()).map((entry) => entry.name));
    if (files.has("Cargo.toml") && files.has("Cargo.lock")) {
      lockfiles.push(relative(root, join(directory, "Cargo.lock")).split(sep).join("/"));
    }
    for (const entry of entries) {
      if (entry.isDirectory() && !ignoredLockfileDirectories.has(entry.name)) {
        await visit(join(directory, entry.name));
      }
    }
  }
  await visit(root);
  return lockfiles.sort();
}

export type Package = { name: string; version: string };

export type PublicationAdapter = {
  isPublished(pkg: Package): Promise<boolean>;
  publish(pkg: Package): Promise<void>;
  waitForPublished(pkg: Package): Promise<void>;
};

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

function compareStableVersions(left: string, right: string): number | undefined {
  const parse = (version: string): bigint[] | undefined => {
    const match = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.exec(version);
    return match ? match.slice(1).map(BigInt) : undefined;
  };
  const leftParts = parse(left);
  const rightParts = parse(right);
  if (!leftParts || !rightParts) return undefined;
  for (let index = 0; index < 3; index++) {
    if (leftParts[index] !== rightParts[index]) return leftParts[index] > rightParts[index] ? 1 : -1;
  }
  return 0;
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

/** Publishes each missing package in dependency order and waits for registry read-back before continuing. */
export async function publishPackages(packages: Package[], adapter: PublicationAdapter): Promise<void> {
  for (const pkg of packages) {
    if (await adapter.isPublished(pkg)) continue;
    await adapter.publish(pkg);
    await adapter.waitForPublished(pkg);
  }
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
          const comparison = compareStableVersions(pkg.version, state.latestVersion);
          if (comparison === undefined) {
            blockers.push(`${pkg.name}@${pkg.version}: cannot safely compare crates.io version ${state.latestVersion}`);
          } else if (comparison < 0) {
            blockers.push(`${pkg.name}@${pkg.version} is older than published crates.io version ${state.latestVersion}`);
          }
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
  const lockfileVerification = lockfiles
    .filter((lockfile) => lockfile !== "Cargo.lock")
    .map((lockfile) => `cargo metadata --manifest-path ${lockfile.replace(/Cargo\.lock$/, "Cargo.toml")} --locked --format-version 1`);
  const tapidVersion = metadata.packages.find((pkg) => pkg.name === "tapid")?.version;
  const cleanInstallVerification = tapidVersion === undefined ? [] : [
    `CARGO_HOME="$RUNNER_TEMP/clean-cargo-home" cargo install tapid --version ${tapidVersion} --locked --root "$RUNNER_TEMP/tapid-clean-install"`,
    '"$RUNNER_TEMP/tapid-clean-install/bin/tapid" --version',
  ];
  const verification = [
    "cargo package --workspace --locked",
    ...lockfileVerification,
    ...cleanInstallVerification,
  ];
  return {
    packages,
    missing: missing.sort((a, b) => `${a.name}\0${a.version}`.localeCompare(`${b.name}\0${b.version}`)),
    drift: drift.sort((a, b) => a.name.localeCompare(b.name)),
    dependentBumps,
    lockfiles,
    blockers,
    verification,
    recovery: "Record confirmed package versions, query crates.io again, and rerun this read-only plan; resume only with the remaining dependency-ordered suffix.",
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

export type CliOptions = {
  publish: boolean;
  json: boolean;
  workspaceDir: string;
  expectedPlanPath?: string;
};

/** Parses safe defaults: publication is impossible unless --publish is explicit. */
export function parseCliArgs(args: string[], baseDir = process.cwd()): CliOptions {
  let publish = false;
  let json = false;
  let workspace: string | undefined;
  let expectedPlan: string | undefined;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "--publish") {
      if (publish) throw new Error("duplicate --publish option");
      publish = true;
    } else if (arg === "--json") {
      if (json) throw new Error("duplicate --json option");
      json = true;
    } else if (arg === "--workspace" || arg === "--expected-plan") {
      const value = args[++index];
      if (!value || value.startsWith("--")) throw new Error(`${arg} requires a path`);
      if (arg === "--workspace") {
        if (workspace !== undefined) throw new Error("duplicate --workspace option");
        workspace = value;
      } else {
        if (expectedPlan !== undefined) throw new Error("duplicate --expected-plan option");
        expectedPlan = value;
      }
    } else {
      throw new Error(`unknown option: ${arg}`);
    }
  }
  if (expectedPlan !== undefined && !publish) throw new Error("--expected-plan requires --publish");
  return {
    publish,
    json,
    workspaceDir: resolve(baseDir, workspace ?? "."),
    expectedPlanPath: expectedPlan === undefined ? undefined : resolve(baseDir, expectedPlan),
  };
}

export async function waitForRegistryPublication(
  pkg: Package,
  lookup: (pkg: Package) => Promise<boolean> = isPublished,
  sleepFn: (milliseconds: number) => Promise<void> = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
  maxAttempts = 12,
): Promise<void> {
  if (!Number.isInteger(maxAttempts) || maxAttempts < 1) throw new Error("maxAttempts must be a positive integer");
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    if (await lookup(pkg)) return;
    if (attempt < maxAttempts) await sleepFn(Math.min(attempt * 5_000, 30_000));
  }
  throw new Error(`crates.io did not expose ${pkg.name}@${pkg.version} after ${maxAttempts} checks`);
}

async function cargoMetadata(workspaceDir: string): Promise<CargoMetadata> {
  const manifestPath = resolve(workspaceDir, "Cargo.toml");
  const cargoHome = resolve(process.env.RUNNER_TEMP ?? tmpdir(), "tapid-cargo-metadata-home");
  const { stdout } = await execFileAsync(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1", "--locked", "--manifest-path", manifestPath],
    { cwd: workspaceDir, encoding: "utf8", env: cargoMetadataEnv(process.env, cargoHome), maxBuffer: 10 * 1024 * 1024 },
  );
  const metadata = JSON.parse(stdout) as CargoMetadata;
  metadata.lockfiles = await findCargoLockfiles(workspaceDir);
  return metadata;
}

function retryAfterMilliseconds(response: Response): number | undefined {
  const value = response.headers.get("Retry-After");
  if (value === null) return undefined;
  if (/^\d+$/.test(value.trim())) return Number(value.trim()) * 1_000;
  const date = Date.parse(value);
  return Number.isNaN(date) ? undefined : Math.max(0, date - Date.now());
}

/** Queries crates.io with bounded retry for transient HTTP and transport failures. */
async function fetchCratesIo(
  url: string,
  pkg: Package,
  versioned: boolean,
  fetchFn: typeof fetch,
  sleepFn: (milliseconds: number) => Promise<void>,
): Promise<Response> {
  const label = versioned ? `${pkg.name} ${pkg.version}` : pkg.name;
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
    if (response.status === 404 || response.status === 200) return response;
    const transient = response.status === 429 || response.status >= 500;
    if (!transient || attempt === 3) {
      throw new Error(`crates.io returned HTTP ${response.status} for ${label}`);
    }
    const retryAfter = retryAfterMilliseconds(response);
    if (retryAfter !== undefined && retryAfter > 300_000) {
      throw new Error(`crates.io requested a retry delay over 5 minutes for ${label}`);
    }
    await sleepFn(retryAfter ?? attempt * 1_000);
  }
  throw new Error(`crates.io lookup retries exhausted for ${label}`);
}

/** Queries crates.io for one exact package version with bounded transient retries. */
export async function isPublished(
  pkg: Package,
  fetchFn: typeof fetch = fetch,
  sleepFn: (milliseconds: number) => Promise<void> = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
): Promise<boolean> {
  const url = `https://crates.io/api/v1/crates/${encodeURIComponent(pkg.name)}/${encodeURIComponent(pkg.version)}`;
  const response = await fetchCratesIo(url, pkg, true, fetchFn, sleepFn);
  if (response.status === 404) return false;
  return true;
}

/** Reads the public version state without attempting publication or mutation. */
export async function registryState(
  pkg: Package,
  fetchFn: typeof fetch = fetch,
  sleepFn: (milliseconds: number) => Promise<void> = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
): Promise<{ published: boolean; latestVersion?: string }> {
  const url = `https://crates.io/api/v1/crates/${encodeURIComponent(pkg.name)}`;
  const response = await fetchCratesIo(url, pkg, false, fetchFn, sleepFn);
  if (response.status === 404) return { published: false };
  const body = await response.json() as {
    crate?: { max_version?: unknown };
    versions?: unknown;
  };
  if (
    typeof body.crate?.max_version !== "string" ||
    !Array.isArray(body.versions) ||
    body.versions.some((version) => !version || typeof version !== "object" || typeof (version as { num?: unknown }).num !== "string")
  ) {
    throw new Error(`malformed crates.io response for ${pkg.name}`);
  }
  const versions = new Set((body.versions as { num: string }[]).map((version) => version.num));
  if (!versions.has(body.crate.max_version)) throw new Error(`malformed crates.io response for ${pkg.name}`);
  return { published: versions.has(pkg.version), latestVersion: body.crate.max_version };
}

const CARGO_CHILD_ENV_KEYS = [
  "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN",
  "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
  "http_proxy", "https_proxy", "all_proxy", "no_proxy",
  "SSL_CERT_FILE", "SSL_CERT_DIR", "TMPDIR", "TMP", "TEMP",
];

function cargoChildEnv(environment: Record<string, string | undefined>, isolatedCargoHome: string): Record<string, string | undefined> {
  const safeEnvironment: Record<string, string | undefined> = {};
  for (const key of CARGO_CHILD_ENV_KEYS) {
    if (environment[key] !== undefined) safeEnvironment[key] = environment[key];
  }
  safeEnvironment.CARGO_HOME = isolatedCargoHome;
  return safeEnvironment;
}

export function cargoMetadataEnv(
  environment: Record<string, string | undefined>,
  isolatedCargoHome: string,
): Record<string, string | undefined> {
  return cargoChildEnv(environment, isolatedCargoHome);
}

export function cargoPublishEnv(
  environment: Record<string, string | undefined>,
  registryToken: string,
  isolatedCargoHome: string,
): Record<string, string | undefined> {
  return { ...cargoChildEnv(environment, isolatedCargoHome), CARGO_REGISTRY_TOKEN: registryToken };
}

async function cargoPublish(pkg: Package, workspaceDir: string): Promise<void> {
  const token = process.env.CARGO_REGISTRY_TOKEN;
  if (!token) throw new Error("CARGO_REGISTRY_TOKEN is required for explicit publication");
  const manifestPath = resolve(workspaceDir, "Cargo.toml");
  const cargoHome = resolve(process.env.RUNNER_TEMP ?? tmpdir(), "tapid-cargo-publish-home");
  try {
    const { stdout, stderr } = await execFileAsync("cargo", [
      "publish", "--no-verify", "--locked", "--package", pkg.name, "--manifest-path", manifestPath,
    ], {
      cwd: workspaceDir,
      encoding: "utf8",
      maxBuffer: 10 * 1024 * 1024,
      env: cargoPublishEnv(process.env, token, cargoHome),
      timeout: 10 * 60 * 1_000,
    });
    if (stdout) process.stdout.write(stdout.replaceAll(token, "[REDACTED]"));
    if (stderr) process.stderr.write(stderr.replaceAll(token, "[REDACTED]"));
  } catch (error) {
    const failure = error as Error & { stdout?: string; stderr?: string; code?: number | string };
    for (const [label, output] of [["stdout", failure.stdout], ["stderr", failure.stderr]] as const) {
      if (output) process.stderr.write(`${label}: ${output.replaceAll(token, "[REDACTED]")}\n`);
    }
    throw new Error(`cargo publish failed for ${pkg.name}@${pkg.version} (exit ${failure.code ?? "unknown"})`);
  }
}

async function main(): Promise<void> {
  let options: CliOptions;
  try {
    options = parseCliArgs(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 2;
    return;
  }
  const metadata = await cargoMetadata(options.workspaceDir);
  const plan = await planPublication(metadata, (pkg) => registryState(pkg));
  const rendered = options.json ? renderMachinePlan(plan) : renderHumanPlan(plan);
  process.stdout.write(rendered);
  if (plan.blockers.length > 0) {
    process.exitCode = 2;
    return;
  }
  if (options.expectedPlanPath !== undefined) {
    const expected = await readFile(options.expectedPlanPath, "utf8");
    if (expected !== renderMachinePlan(plan)) {
      throw new Error("current publication plan differs from the reviewed preflight plan; rerun preflight before publishing");
    }
  }
  if (!options.publish || plan.packages.length === 0) return;
  if (!process.env.CARGO_REGISTRY_TOKEN) throw new Error("CARGO_REGISTRY_TOKEN is required for explicit publication");
  await publishPackages(plan.packages, {
    isPublished,
    publish: (pkg) => cargoPublish(pkg, options.workspaceDir),
    waitForPublished: (pkg) => waitForRegistryPublication(pkg),
  });
}

const isMain = process.argv[1] !== undefined && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
if (isMain) await main();
