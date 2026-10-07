import { execFile } from "node:child_process";
import { lstat, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";
import { findCargoLockfiles, planPublication, registryState } from "./publish.ts";
import type { CargoMetadata, Package, RegistryLookup } from "./publish.ts";
import { releaseVersion } from "./release.ts";

const execFileAsync = promisify(execFile);
type Run = (command: string, args: string[]) => Promise<string>;
type History = { sha: string; title: string; pr?: number };

function stable(version: string): bigint[] {
  releaseVersion(`v${version}`, version);
  return version.split(".").map(BigInt);
}

function newer(version: string, baseline: string): boolean {
  const left = stable(version), right = stable(baseline);
  const index = left.findIndex((part, i) => part !== right[i]);
  return index >= 0 && left[index] > right[index];
}

export function nextProductVersion(current: string, requested: string, baseline: string = current): string {
  const [major, minor, patch] = stable(baseline);
  if (newer(baseline, current)) throw new Error("main product version must not be older than the public release baseline");
  const version = requested || (newer(current, baseline) ? current : `${major}.${minor}.${patch + 1n}`);
  if (!newer(version, baseline) || newer(current, version)) throw new Error("product version must be newer than the public baseline and not older than main");
  return version;
}

export async function refreshLockfiles(lockfiles: string[], run: Run): Promise<void> {
  for (const lock of [...lockfiles].sort()) {
    if (!/^(?:[A-Za-z0-9_-]+\/)*Cargo\.lock$/.test(lock)) throw new Error(`unsafe lockfile path: ${lock}`);
    const manifest = lock.replace(/Cargo\.lock$/, "Cargo.toml");
    await run("cargo", ["update", "--workspace", "--manifest-path", manifest]);
    await run("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked", "--manifest-path", manifest]);
  }
}

function markdown(text: string): string {
  return text.replace(/[\r\n\t]/g, " ").replace(/[\\`*_{}\[\]()<>|]/g, (character) => `\\${character}`);
}

export function preparationFiles(version: string, baseline: string, preparedFrom: string, packages: Package[], history: History[]) {
  const previous = baseline.replace(/^v/, "");
  releaseVersion(baseline, previous);
  if (!newer(version, previous)) throw new Error("release baseline must be older than the product version");
  if (!/^[a-f0-9]{40}$/.test(preparedFrom)) throw new Error("preparation source must be a commit SHA");
  if (packages.find((pkg) => pkg.name === "tapid")?.version !== version) throw new Error("publication plan must contain the product version");
  const names = new Set<string>();
  for (const pkg of packages) {
    stable(pkg.version);
    if (!/^[a-z][a-z0-9_-]*$/.test(pkg.name) || names.has(pkg.name)) throw new Error("invalid or duplicate planned package");
    names.add(pkg.name);
  }
  const notesPath = `docs/releases/${version}.md`;
  const commits = history.map(({ sha, title, pr }) => {
    if (!/^[a-f0-9]{40}$/.test(sha) || (pr !== undefined && (!Number.isSafeInteger(pr) || pr < 1))) throw new Error("invalid release history");
    const link = pr ? `https://github.com/LimeTip/tapid/pull/${pr}` : `https://github.com/LimeTip/tapid/commit/${sha}`;
    return `- ${markdown(title)} ([${pr ? `#${pr}` : sha.slice(0, 8)}](${link}))`;
  });
  const notes = [`# Tapid ${version}`, "", `Changes since ${baseline}. Reviewed source notes; publication and platform verification are reported by the release workflow.`, "", "## Changes", "", ...commits, "", "## Package versions", "", "| Package | Version |", "| --- | --- |", ...packages.map((pkg) => `| \`${pkg.name}\` | \`${pkg.version}\` |`), "", "Supporting crates retain independent versions. The table includes only the planned missing crates.io versions.", ""].join("\n");
  return { notesPath, notes, intent: `${JSON.stringify({ schema: "tapid-release-intent-v1", version, baseline, prepared_from: preparedFrom, notes: notesPath, packages }, null, 2)}\n` };
}

async function validateExistingNotes(notesPath: string): Promise<void> {
  if (!(await lstat(notesPath)).isFile() || !(await readFile(notesPath, "utf8")).trim()) {
    throw new Error(`existing release notes must be a nonempty regular file: ${notesPath}`);
  }
}

export async function prepareRelease(requested: string, baseline: string, options: { directory?: string; run?: Run; lookup?: RegistryLookup } = {}): Promise<void> {
  const directory = options.directory ?? process.cwd();
  const run: Run = options.run ?? (async (command, args) => (await execFileAsync(command, args, { cwd: directory, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 })).stdout.trim());
  const metadata = async () => JSON.parse(await run("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"])) as CargoMetadata;
  const before = await metadata();
  const current = before.packages.find((pkg) => pkg.name === "tapid")?.version;
  if (!current) throw new Error("workspace metadata must contain the tapid product version");
  const previous = baseline.replace(/^v/, "");
  releaseVersion(baseline, previous);
  const version = nextProductVersion(current, requested, previous);
  if (await run("git", ["status", "--porcelain"])) throw new Error("preparation requires a clean checkout");
  const preparedFrom = await run("git", ["rev-parse", "HEAD"]);
  if (await run("git", ["cat-file", "-t", `refs/tags/${baseline}`]) !== "tag") throw new Error("baseline must be an annotated tag");
  await run("git", ["merge-base", "--is-ancestor", `refs/tags/${baseline}^{commit}`, "HEAD"]);
  const notesPath = join(directory, `docs/releases/${version}.md`);
  try {
    await validateExistingNotes(notesPath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  await run("release-plz", ["update"]);
  const proposed = (await metadata()).packages.find((pkg) => pkg.name === "tapid")?.version;
  if (!proposed || newer(proposed, version)) throw new Error(`version analysis requires ${proposed}; prepare again with that explicit product version`);
  // set-version updates a changelog even when changelog_update=false. Supply a
  // disposable changelog so the tool still owns every Cargo version edit.
  const scratch = await mkdtemp(join(tmpdir(), "tapid-release-version-"));
  try {
    const changelog = join(scratch, "CHANGELOG.md");
    const config = join(scratch, "release-plz.toml");
    await writeFile(changelog, `# Changelog\n\n## [${proposed}]\n\nPrepared release.\n`);
    await writeFile(config, `[[package]]\nname = "tapid"\nchangelog_path = ${JSON.stringify(changelog)}\n`);
    await run("release-plz", ["set-version", `tapid@${version}`, "--config", config]);
  } finally { await rm(scratch, { recursive: true, force: true }); }
  const lockfiles = await findCargoLockfiles(directory);
  await refreshLockfiles(lockfiles, run);
  const plan = await planPublication({ ...(await metadata()), lockfiles }, options.lookup ?? registryState);
  if (plan.blockers.length) throw new Error(`release preparation blocked:\n${plan.blockers.join("\n")}`);
  const log = await run("git", ["log", "--reverse", "--format=%H%x09%s", `${baseline}..HEAD`]);
  const history = log ? log.split("\n").map((line) => {
    const [sha, ...titleParts] = line.split("\t");
    const title = titleParts.join(" ");
    const number = /\(#([1-9][0-9]*)\)$/.exec(title);
    return { sha, title, ...(number ? { pr: Number(number[1]) } : {}) };
  }) : [];
  if (!history.length) throw new Error("no commits since the baseline release");
  const files = preparationFiles(version, baseline, preparedFrom, plan.packages, history);
  await mkdir(join(directory, dirname(files.notesPath)), { recursive: true });
  try {
    await writeFile(notesPath, files.notes, { flag: "wx" });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    await validateExistingNotes(notesPath);
  }
  await writeFile(join(directory, "docs/releases/intent.json"), files.intent);
  await writeFile(join(directory, "release-preparation.json"), `${JSON.stringify({ version, baseline, packages: plan.packages, notes: files.notesPath }, null, 2)}\n`);
  console.log(`Prepared Tapid ${version}: ${plan.packages.length} package versions`);
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [command, ...args] = process.argv.slice(2);
  if (command !== "prepare" || args.length !== 2) throw new Error("usage: prepare.ts prepare VERSION_OR_EMPTY BASELINE_TAG");
  await prepareRelease(args[0], args[1]);
}
