import { ok as assert, strictEqual as assertEquals } from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const checker = fileURLToPath(new URL("./check_architecture.ts", import.meta.url));

function run(command: string, args: string[], cwd?: string): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => stdout += chunk);
    child.stderr.on("data", (chunk) => stderr += chunk);
    child.on("error", reject);
    child.on("close", (code) => resolve({ code: code ?? -1, stdout, stderr }));
  });
}

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "tapid-architecture-"));
  const init = await run("git", ["init", "--quiet", root]);
  assertEquals(init.code, 0, init.stderr);
  return root;
}

async function write(root: string, relative: string, content: string) {
  const path = join(root, relative);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, content);
}

async function track(root: string, paths: string[]) {
  const result = await run("git", ["add", "--", ...paths], root);
  assertEquals(result.code, 0, result.stderr);
}

async function check(root: string) {
  return await run(process.execPath, ["--experimental-strip-types", checker, "--root", root], root);
}

async function withFixture(body: (root: string) => Promise<void>) {
  const root = await fixture();
  try {
    await body(root);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

test("architecture checker rejects unrelated CLI arguments", async () => {
  const result = await run(process.execPath, [
    "--experimental-strip-types",
    checker,
    "unexpected",
    "--root",
    process.cwd(),
  ]);
  assertEquals(result.code, 2);
  assert(result.stderr.includes("usage: check_architecture.ts [--root PATH]"));
});

test("architecture checker reports advisory files in path order", () => withFixture(async (root) => {
  await write(root, "crates/zeta/src/lib.rs", "line\n".repeat(801));
  await write(root, "crates/alpha/src/lib.rs", "line\n".repeat(802));
  await track(root, ["crates/zeta/src/lib.rs", "crates/alpha/src/lib.rs"]);
  const result = await check(root);
  assertEquals(result.code, 0, result.stdout + result.stderr);
  assert(result.stdout.includes("crates/alpha/src/lib.rs: 802 physical lines exceeds the 800-line review recommendation"));
  assert(result.stdout.indexOf("crates/alpha") < result.stdout.indexOf("crates/zeta"));
}));

test("architecture checker enforces the CLI entrypoint threshold", () => withFixture(async (root) => {
  await write(root, "crates/tapid-cli/src/main.rs", "line\n".repeat(101));
  await track(root, ["crates/tapid-cli/src/main.rs"]);
  const result = await check(root);
  assertEquals(result.code, 1);
  assert(result.stdout.includes("101 physical lines exceeds entrypoint threshold 100"));
}));

test("architecture checker validates exception staleness", () => withFixture(async (root) => {
  await write(root, "crates/tapid-cli/src/main.rs", "fn main() {}\n");
  await write(root, "docs/architecture-exceptions.txt", "crates/tapid-cli/src/main.rs | This documented exception is no longer needed.\n");
  await track(root, ["crates/tapid-cli/src/main.rs", "docs/architecture-exceptions.txt"]);
  const result = await check(root);
  assertEquals(result.code, 1);
  assert(result.stdout.includes("exception is stale at 1 physical lines"));
}));

test("architecture checker ignores tests, generated trees, and untracked files", () => withFixture(async (root) => {
  const excluded = [
    "crates/example/tests/large.rs",
    "crates/example/src/tests.rs",
    "tests/integration/large.rs",
    "target/debug/build/large.rs",
    "generated/large.rs",
  ];
  for (const path of excluded) await write(root, path, "line\n".repeat(801));
  await write(root, "crates/example/src/untracked.rs", "line\n".repeat(801));
  await write(root, "crates/example/src/lib.rs", "pub fn small() {}\n");
  await track(root, [...excluded, "crates/example/src/lib.rs"]);
  const result = await check(root);
  assertEquals(result.code, 0, result.stdout + result.stderr);
  assert(result.stdout.includes("Architecture check passed: 1 production Rust file scanned"));
}));

test("architecture checker ignores deleted tracked files", () => withFixture(async (root) => {
  const path = "crates/example/src/lib.rs";
  await write(root, path, "line\n".repeat(801));
  await track(root, [path]);
  await unlink(join(root, path));
  const result = await check(root);
  assertEquals(result.code, 0, result.stdout + result.stderr);
  assert(result.stdout.includes("Architecture check passed: 0 production Rust files scanned"));
}));

test("architecture checker rejects a tracked Rust symbolic link", { skip: process.platform === "win32" }, () => withFixture(async (root) => {
  const path = "crates/example/src/lib.rs";
  await write(root, "outside.rs", "pub fn outside() {}\n");
  await mkdir(dirname(join(root, path)), { recursive: true });
  await symlink(join(root, "outside.rs"), join(root, path));
  await track(root, [path]);
  const result = await check(root);
  assertEquals(result.code, 2);
  assert(result.stderr.includes("architecture input must be a regular file"));
}));

test("architecture checker accepts the advisory threshold", () => withFixture(async (root) => {
  await write(root, "crates/example/src/lib.rs", "line\n".repeat(800));
  await track(root, ["crates/example/src/lib.rs"]);
  const result = await check(root);
  assertEquals(result.code, 0, result.stdout + result.stderr);
  assert(!result.stdout.includes("review recommendation"));
}));

test("architecture checker scans a production module named build", () => withFixture(async (root) => {
  await write(root, "crates/example/src/build/mod.rs", "line\n".repeat(801));
  await track(root, ["crates/example/src/build/mod.rs"]);
  const result = await check(root);
  assertEquals(result.code, 0, result.stdout + result.stderr);
  assert(result.stdout.includes("crates/example/src/build/mod.rs: 801 physical lines"));
}));

test("architecture checker rejects an exception for an advisory file", () => withFixture(async (root) => {
  await write(root, "crates/example/src/lib.rs", "line\n".repeat(801));
  await write(root, "docs/architecture-exceptions.txt", "crates/example/src/lib.rs | Cohesive parser retained after architecture review.\n");
  await track(root, ["crates/example/src/lib.rs", "docs/architecture-exceptions.txt"]);
  const result = await check(root);
  assertEquals(result.code, 1);
  assert(result.stdout.includes("exception path has no hard architecture threshold"));
}));

test("architecture checker requires an exception rationale", () => withFixture(async (root) => {
  await write(root, "crates/tapid-cli/src/main.rs", "line\n".repeat(101));
  await write(root, "docs/architecture-exceptions.txt", "crates/tapid-cli/src/main.rs |\n");
  await track(root, ["crates/tapid-cli/src/main.rs", "docs/architecture-exceptions.txt"]);
  const result = await check(root);
  assertEquals(result.code, 1);
  assert(result.stdout.includes("line 1 must contain a path and rationale"));
}));

test("architecture checker requires a concrete exception rationale", () => withFixture(async (root) => {
  await write(root, "crates/tapid-cli/src/main.rs", "line\n".repeat(101));
  await write(root, "docs/architecture-exceptions.txt", "crates/tapid-cli/src/main.rs | x\n");
  await track(root, ["crates/tapid-cli/src/main.rs", "docs/architecture-exceptions.txt"]);
  const result = await check(root);
  assertEquals(result.code, 1);
  assert(result.stdout.includes("line 1 rationale must be at least"));
}));
