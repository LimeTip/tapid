import { ok as assert, strictEqual as assertEquals } from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
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
  await write(root, "Cargo.toml", '[workspace]\nmembers = ["fixture"]\nresolver = "2"\n');
  await write(root, "fixture/Cargo.toml", '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n');
  await write(root, "fixture/src/lib.rs", "");
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
  const lock = await run("cargo", ["generate-lockfile", "--offline"], root);
  assertEquals(lock.code, 0, lock.stderr);
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

test("architecture checker rejects renamed normal test-support dependencies", () => withFixture(async (root) => {
  await write(root, "support/Cargo.toml", '[package]\nname = "tapid-test-support"\nversion = "0.1.0"\nedition = "2021"\n');
  await write(root, "support/src/lib.rs", "");
  await write(root, "fixture/Cargo.toml", '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n[dependencies]\nfixtures = { package = "tapid-test-support", path = "../support" }\n');
  const result = await check(root);
  assertEquals(result.code, 1, result.stdout + result.stderr);
  assert(result.stdout.includes("fixture: dependency tapid-test-support"));
  assert(result.stdout.includes("test-support isolation"));
  assert(result.stdout.includes("alias fixtures"));
}));

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

async function dependencyFixture(root: string, owner: string, dependency: string, section: string, attributes = "", inherited = false) {
  await write(root, "dependency/Cargo.toml", `[package]\nname = "${dependency}"\nversion = "0.1.0"\nedition = "2021"\n`);
  await write(root, "dependency/src/lib.rs", "");
  await write(root, "Cargo.toml", `[workspace]\nmembers = ["fixture", "dependency"]\ndefault-members = ["dependency"]\nresolver = "2"\n${inherited ? `[workspace.dependencies]\nrenamed = { package = "${dependency}", path = "dependency" }\n` : ""}`);
  await write(root, "fixture/Cargo.toml", `[package]\nname = "${owner}"\nversion = "0.1.0"\nedition = "2021"\n[${section}]\nrenamed = { ${inherited ? 'workspace = true' : `package = "${dependency}", path = "../dependency"`}${attributes} }\n`);
}

for (const [owner, dependency, rule] of [
  ["capability", "tapid-test-support", "test-support isolation"],
  ["tapid", "tapid-test-support", "test-support isolation"],
  ["capability", "tapid", "CLI dependency direction"],
  ["tapid-core", "network-client", "core dependency allowlist"],
]) {
  for (const kind of ["dependencies", "build-dependencies", "dev-dependencies"]) {
    test(`architecture ${owner} -> ${dependency} ${kind}`, () => withFixture(async (root) => {
      await dependencyFixture(root, owner, dependency, kind);
      const result = await check(root);
      assertEquals(result.code, kind === "dev-dependencies" ? 0 : 1, result.stdout + result.stderr);
      if (kind !== "dev-dependencies") {
        assert(result.stdout.includes(`${owner}: dependency ${dependency}`));
        assert(result.stdout.includes(rule));
        assert(result.stdout.includes(kind === "build-dependencies" ? "(build," : "(normal,"));
      }
    }));
  }
}

for (const [owner, dependency] of [["capability", "another-capability"], ["tapid", "capability"], ["tapid-test-support", "tapid"], ...["base64", "semver", "url"].map(name => ["tapid-core", name])]) {
  for (const kind of ["dependencies", "build-dependencies"]) {
    test(`architecture permits ${owner} -> ${dependency} ${kind}`, () => withFixture(async (root) => {
      await dependencyFixture(root, owner, dependency, kind);
      const result = await check(root);
      assertEquals(result.code, 0, result.stdout + result.stderr);
    }));
  }
}

for (const [form, section, attributes, inherited] of [
  ["inactive target", 'target.\'cfg(target_os = "none")\'.dependencies', "", false],
  ["inactive target build", 'target.\'cfg(target_os = "none")\'.build-dependencies', "", false],
  ["optional", "dependencies", ", optional = true", false],
  ["workspace inherited", "dependencies", "", true],
] as const) {
  for (const dependency of ["tapid-test-support", "another-capability"]) {
    test(`architecture checks ${form} ${dependency} in non-default member`, () => withFixture(async (root) => {
      await dependencyFixture(root, "capability", dependency, section, attributes, inherited);
      const result = await check(root);
      assertEquals(result.code, dependency === "tapid-test-support" ? 1 : 0, result.stdout + result.stderr);
      if (dependency === "tapid-test-support") {
        assert(result.stdout.includes("alias renamed"));
        if (form.startsWith("inactive")) assert(result.stdout.includes('target cfg(target_os = "none")'));
      }
    }));
  }
}

test("architecture excludes the separate integration workspace", () => withFixture(async (root) => {
  await write(root, "tests/integration/Cargo.toml", '[workspace]\n[package]\nname = "integration-harness"\nversion = "0.1.0"\n[dependencies]\ntapid-test-support = { path = "support" }\n');
  await write(root, "tests/integration/src/lib.rs", "");
  await write(root, "tests/integration/support/Cargo.toml", '[package]\nname = "tapid-test-support"\nversion = "0.1.0"\n');
  await write(root, "tests/integration/support/src/lib.rs", "");
  const result = await check(root);
  assertEquals(result.code, 0, result.stdout + result.stderr);
}));

test("architecture sorts dependency diagnostics and size exceptions cannot waive them", () => withFixture(async (root) => {
  await dependencyFixture(root, "tapid-core", "tapid-test-support", "dependencies");
  await write(root, "crates/tapid-cli/src/main.rs", "line\n".repeat(101));
  await write(root, "docs/architecture-exceptions.txt", "crates/tapid-cli/src/main.rs | Temporary entrypoint migration debt under review.\n");
  await track(root, ["crates/tapid-cli/src/main.rs", "docs/architecture-exceptions.txt"]);
  const result = await check(root);
  assertEquals(result.code, 1, result.stdout + result.stderr);
  const diagnostics = result.stdout.split("\n").filter(line => line.startsWith("- "));
  assertEquals(diagnostics.length, 2);
  assertEquals(diagnostics.join("\n"), [...diagnostics].sort().join("\n"));
  assert(result.stdout.includes("test-support isolation"));
  assert(result.stdout.includes("core dependency allowlist"));
  assert(!result.stdout.includes("entrypoint threshold"));
}));

test("architecture requires a root manifest", () => withFixture(async (root) => {
  await unlink(join(root, "Cargo.toml"));
  const result = await run(process.execPath, ["--experimental-strip-types", checker, "--root", root]);
  assertEquals(result.code, 2);
  assert(result.stderr.includes("Architecture check could not run"));
}));

test("architecture fails closed on Cargo failure", () => withFixture(async (root) => {
  await write(root, "Cargo.toml", "not valid TOML !!!");
  const result = await run(process.execPath, ["--experimental-strip-types", checker, "--root", root]);
  assertEquals(result.code, 2);
  assert(result.stderr.includes("Architecture check could not run"));
}));

// Control only the external Cargo process for outputs real Cargo cannot emit.
const metadataPackage = { id: "fixture", name: "fixture", dependencies: [] };
const metadataDependency = { name: "example", kind: null, rename: null, target: null, optional: false };
for (const [name, output] of [
  ["valid metadata control", JSON.stringify({ packages: [metadataPackage], workspace_members: ["fixture"] })],
  ["malformed JSON", "{"],
  ["null metadata", "null"],
  ["missing packages", JSON.stringify({ workspace_members: [] })],
  ["missing members", JSON.stringify({ packages: [] })],
  ["missing member record", JSON.stringify({ packages: [], workspace_members: ["missing"] })],
  ["invalid member identity", JSON.stringify({ packages: [{ ...metadataPackage, name: null }], workspace_members: ["fixture"] })],
  ["invalid dependency list", JSON.stringify({ packages: [{ ...metadataPackage, dependencies: null }], workspace_members: ["fixture"] })],
  ["unsupported dependency kind", JSON.stringify({ packages: [{ ...metadataPackage, dependencies: [{ ...metadataDependency, kind: "normal" }] }], workspace_members: ["fixture"] })],
  ...["kind", "name", "target", "rename", "optional"].map(field => [
    `invalid dependency ${field}`,
    JSON.stringify({ packages: [{ ...metadataPackage, dependencies: [{ ...metadataDependency, [field]: 42 }] }], workspace_members: ["fixture"] }),
  ]),
]) {
  test(`architecture metadata boundary: ${name}`, () => withFixture(async (root) => {
    const preload = join(root, "cargo-boundary.mjs");
    await writeFile(preload, `import cp from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import { promisify } from 'node:util';
const original = cp.execFile;
const originalAsync = promisify(original);
cp.execFile = function(...args) { return original(...args); };
cp.execFile[promisify.custom] = async function(command, ...args) {
  if (command !== 'cargo') return originalAsync(command, ...args);
  return { stdout: ${JSON.stringify(output)}, stderr: '' };
};
syncBuiltinESMExports();
`);
    const result = await run(process.execPath, ["--import", pathToFileURL(preload).href, "--experimental-strip-types", checker, "--root", root]);
    assertEquals(result.code, name === "valid metadata control" ? 0 : 2, result.stdout + result.stderr);
    if (name !== "valid metadata control") {
      assert(result.stderr.includes("Architecture check could not run"));
      assert(!result.stdout.includes("Architecture check passed"));
    }
  }));
}
