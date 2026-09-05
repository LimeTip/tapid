import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);

const TARGETS = [
  "aarch64-apple-darwin",
  "aarch64-pc-windows-msvc",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
] as const;

export function releaseVersion(tag: string, cargoVersion: string): string {
  const match = /^v([0-9]+\.[0-9]+\.[0-9]+)$/.exec(tag);
  if (!match) throw new Error(`release tag must match vX.Y.Z: ${tag}`);
  const version = match[1];
  if (cargoVersion !== version) {
    throw new Error(`release tag ${tag} does not match tapid ${cargoVersion}`);
  }
  return version;
}

async function sha256(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

export async function checksumLines(directory: string, version: string): Promise<string> {
  const expected = TARGETS.map((target) => `tapid-${version}-${target}.tar.gz`).sort();
  const actual = (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.endsWith(".tar.gz"))
    .map((entry) => entry.name)
    .sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`expected exactly these archives:\n${expected.join("\n")}\nfound:\n${actual.join("\n")}`);
  }
  const lines: string[] = [];
  for (const name of expected) lines.push(`${await sha256(join(directory, name))}  ${name}`);
  return `${lines.join("\n")}\n`;
}

async function output(command: string, args: string[]): Promise<string> {
  const { stdout } = await execFileAsync(command, args, { encoding: "utf8" });
  return stdout.trim();
}

async function tapidVersion(): Promise<string> {
  const metadata = JSON.parse(await output("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"]));
  const tapid = metadata.packages.find((pkg: { name: string }) => pkg.name === "tapid");
  if (!tapid) throw new Error("cargo metadata does not contain the tapid package");
  return tapid.version;
}

async function checkTag(tag: string): Promise<void> {
  const type = await output("git", ["cat-file", "-t", `refs/tags/${tag}`]);
  if (type !== "tag") throw new Error(`release tag must be annotated: ${tag}`);
  console.log(releaseVersion(tag, await tapidVersion()));
}

const isMain = process.argv[1] !== undefined &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url;

if (isMain) {
  const [command, ...args] = process.argv.slice(2);
  if (command === "check-tag" && args.length === 1) {
    await checkTag(args[0]);
  } else if (command === "current-version" && args.length === 0) {
    console.log(await tapidVersion());
  } else if (command === "checksums" && args.length === 2) {
    const content = await checksumLines(args[0], args[1]);
    await writeFile(join(args[0], "SHA256SUMS"), content);
  } else {
    console.error("usage: release.ts check-tag TAG | current-version | checksums DIRECTORY VERSION");
    process.exitCode = 2;
  }
}
