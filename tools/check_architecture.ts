#!/usr/bin/env -S node --experimental-strip-types

import { execFile } from "node:child_process";
import { lstat, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);
const REVIEW_RECOMMENDATION = 800;
const CLI_MAIN_PATH = "crates/tapid-cli/src/main.rs";
const CLI_MAIN_THRESHOLD = 100;
const EXCEPTIONS_PATH = "docs/architecture-exceptions.txt";
const MIN_RATIONALE_CHARS = 20;
const EXCLUDED_TEST_PARTS = new Set(["test", "tests"]);
const EXCLUDED_TOP_LEVEL_TREES = new Set(["target", "generated", "build"]);
const EXCLUDED_TEST_FILES = new Set(["test.rs", "tests.rs"]);

async function trackedFiles(root: string): Promise<string[]> {
  const { stdout } = await execFileAsync("git", ["ls-files", "-z"], { cwd: root, encoding: "utf8" });
  const existing: string[] = [];
  for (const path of stdout.split("\0").filter(Boolean)) {
    try {
      await lstat(join(root, path));
      existing.push(path);
    } catch (error) {
      if (!(error instanceof Error) || !("code" in error) || error.code !== "ENOENT") throw error;
    }
  }
  return existing.sort();
}

function isProductionRust(path: string): boolean {
  const parts = path.split("/");
  const name = parts.at(-1) ?? "";
  return name.endsWith(".rs") &&
    !EXCLUDED_TEST_FILES.has(name) &&
    !EXCLUDED_TOP_LEVEL_TREES.has(parts[0]) &&
    !parts.some((part) => EXCLUDED_TEST_PARTS.has(part));
}

async function physicalLineCount(path: string): Promise<number> {
  const bytes = await readFile(path);
  if (bytes.length === 0) return 0;
  let lines = bytes[bytes.length - 1] === 10 ? 0 : 1;
  for (const byte of bytes) if (byte === 10) lines++;
  return lines;
}

async function readExceptions(root: string, tracked: Set<string>) {
  const exceptions = new Map<string, string>();
  const errors: string[] = [];
  if (!tracked.has(EXCEPTIONS_PATH)) return { exceptions, errors };

  const text = await readFile(join(root, EXCEPTIONS_PATH), "utf8");
  for (const [index, rawLine] of text.split(/\r?\n/).entries()) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const separator = line.indexOf("|");
    const path = separator < 0 ? "" : line.slice(0, separator).trim();
    const rationale = separator < 0 ? "" : line.slice(separator + 1).trim();
    const lineNumber = index + 1;
    if (!path || !rationale) {
      errors.push(`${EXCEPTIONS_PATH}: line ${lineNumber} must contain a path and rationale`);
    } else if (rationale.length < MIN_RATIONALE_CHARS) {
      errors.push(`${EXCEPTIONS_PATH}: line ${lineNumber} rationale must be at least ${MIN_RATIONALE_CHARS} characters`);
    } else if (exceptions.has(path)) {
      errors.push(`${EXCEPTIONS_PATH}: duplicate exception for ${path}`);
    } else {
      exceptions.set(path, rationale);
    }
  }
  return { exceptions, errors };
}

export async function checkArchitecture(root: string) {
  const tracked = await trackedFiles(root);
  const productionFiles = tracked.filter(isProductionRust);
  const lineCounts = new Map<string, number>();
  for (const path of productionFiles) lineCounts.set(path, await physicalLineCount(join(root, path)));

  const { exceptions, errors } = await readExceptions(root, new Set(tracked));
  const advisories: string[] = [];
  for (const path of [...exceptions.keys()].sort()) {
    if (!productionFiles.includes(path)) {
      errors.push(`${EXCEPTIONS_PATH}: exception path is not a tracked production Rust file: ${path}`);
    } else if (path !== CLI_MAIN_PATH) {
      errors.push(`${EXCEPTIONS_PATH}: exception path has no hard architecture threshold: ${path}`);
    } else if ((lineCounts.get(path) ?? 0) <= CLI_MAIN_THRESHOLD) {
      errors.push(`${EXCEPTIONS_PATH}: exception is stale at ${lineCounts.get(path)} physical lines: ${path}`);
    }
  }

  for (const path of productionFiles) {
    const lineCount = lineCounts.get(path) ?? 0;
    if (path === CLI_MAIN_PATH) {
      if (lineCount > CLI_MAIN_THRESHOLD && !exceptions.has(path)) {
        errors.push(`${path}: ${lineCount} physical lines exceeds entrypoint threshold ${CLI_MAIN_THRESHOLD}; keep main.rs to argument dispatch and exit conversion or document an exception in ${EXCEPTIONS_PATH}`);
      }
    } else if (lineCount > REVIEW_RECOMMENDATION) {
      advisories.push(`${path}: ${lineCount} physical lines exceeds the ${REVIEW_RECOMMENDATION}-line review recommendation; review cohesion, module depth, and navigability before deciding whether to split it`);
    }
  }
  return { productionFiles, advisories: advisories.sort(), errors: errors.sort() };
}

const isMain = process.argv[1] !== undefined && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
if (isMain) {
  const args = process.argv.slice(2);
  const validArgs = args.length === 0 ||
    (args.length === 2 && args[0] === "--root" && args[1].length > 0);
  if (!validArgs) {
    console.error("usage: check_architecture.ts [--root PATH]");
    process.exitCode = 2;
  } else {
    const root = resolve(args.length === 2 ? args[1] : process.cwd());
    try {
      const result = await checkArchitecture(root);
      if (result.errors.length) {
        console.log("Architecture check failed:");
        for (const error of result.errors) console.log(`- ${error}`);
        process.exitCode = 1;
      } else {
        if (result.advisories.length) {
          console.log("Architecture review recommended:");
          for (const advisory of result.advisories) console.log(`- ${advisory}`);
        }
        const noun = result.productionFiles.length === 1 ? "file" : "files";
        console.log(`Architecture check passed: ${result.productionFiles.length} production Rust ${noun} scanned`);
      }
    } catch (error) {
      console.error(`Architecture check could not run: ${error instanceof Error ? error.message : error}`);
      process.exitCode = 2;
    }
  }
}