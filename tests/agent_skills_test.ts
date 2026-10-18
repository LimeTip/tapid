import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const script = join(repository, "scripts/sync-agent-skills.ts");
const run = (root: string, mode: string) =>
  spawnSync(process.execPath, ["--experimental-strip-types", script, mode], {
    cwd: root,
    encoding: "utf8",
  });

function temporary(body: (root: string) => void): void {
  const root = mkdtempSync(join(tmpdir(), "tapid-agent-skills-"));
  try {
    body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("write mirrors complete skill trees including references and licenses", () =>
  temporary((root) => {
    const source = join(root, ".agents/skills");
    mkdirSync(join(source, "example/references"), { recursive: true });
    writeFileSync(join(source, "example/SKILL.md"), "---\nname: example\n---\n");
    writeFileSync(join(source, "example/references/details.md"), "Read this reference.\n");
    writeFileSync(join(source, "LICENSE.example"), "Example license\n");
    const result = run(root, "--write");
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      readFileSync(join(root, ".claude/skills/example/references/details.md"), "utf8"),
      "Read this reference.\n",
    );
    assert.equal(readFileSync(join(root, ".claude/skills/LICENSE.example"), "utf8"), "Example license\n");
    const check = run(root, "--check");
    assert.equal(check.status, 0, check.stderr);
  }));

test("check reports missing, changed, and extra files without modifying them", () =>
  temporary((root) => {
    const source = join(root, ".agents/skills/example");
    const target = join(root, ".claude/skills/example");
    mkdirSync(source, { recursive: true });
    mkdirSync(target, { recursive: true });
    writeFileSync(join(source, "SKILL.md"), "canonical\n");
    writeFileSync(join(source, "reference.md"), "missing\n");
    writeFileSync(join(target, "SKILL.md"), "outdated\n");
    writeFileSync(join(target, "extra.md"), "keep until reviewed\n");
    const result = run(root, "--check");
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Changed: .*SKILL\.md/);
    assert.match(result.stderr, /Missing: .*reference\.md/);
    assert.match(result.stderr, /Extra: .*extra\.md/);
    assert.equal(readFileSync(join(target, "SKILL.md"), "utf8"), "outdated\n");
    assert.equal(existsSync(join(target, "reference.md")), false);
    const write = run(root, "--write");
    assert.equal(write.status, 1, "extras still require manual review");
    assert.equal(readFileSync(join(target, "extra.md"), "utf8"), "keep until reviewed\n");
    assert.equal(readFileSync(join(target, "SKILL.md"), "utf8"), "canonical\n");
  }));

test("write fails before creating a mirror when the canonical tree is missing", () =>
  temporary((root) => {
    const result = run(root, "--write");
    assert.equal(result.status, 1);
    assert.equal(existsSync(join(root, ".claude")), false);
  }));

test("checkout contains every recommended skill, dependency, reference, and license", () => {
  const source = join(repository, ".agents/skills");
  const expected = [
    "code-review", "codebase-design", "domain-modeling", "grill-with-docs", "grilling",
    "implement", "implement-spec", "pr", "retro", "setup-matt-pocock-skills",
    "show-me", "tdd", "to-spec", "to-tickets", "writing-for-agents",
  ];
  const actual = readdirSync(source, { withFileTypes: true })
    .filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
  assert.deepEqual(actual, expected);
  for (const name of expected) {
    const contents = readFileSync(join(source, name, "SKILL.md"), "utf8");
    assert.match(contents, new RegExp(`^---\\r?\\nname: ${name}\\r?\\n`));
    assert.match(contents, /^description: .+/m);
  }
  for (const entry of ["LICENSE.mattpocock", "LICENSE.humanlayer"]) {
    assert.match(readFileSync(join(source, entry), "utf8"), /MIT License/);
  }
  const checkLinks = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) checkLinks(path);
      else if (entry.name.endsWith(".md")) {
        // Templates contain illustrative links to files a future user creates.
        const contents = readFileSync(path, "utf8").replace(/^```[^\n]*\n[\s\S]*?^```[^\n]*$/gm, "");
        for (const match of contents.matchAll(/\[[^\]\n]+\]\(([^)\s]+)\)/g)) {
          const target = match[1].split("#")[0];
          if (!target || /^[a-z]+:/i.test(target)) continue;
          assert.ok(existsSync(resolve(dirname(path), target)), `${path}: missing ${target}`);
        }
      }
    }
  };
  checkLinks(source);
  const result = run(repository, "--check");
  assert.equal(result.status, 0, result.stderr);
});
