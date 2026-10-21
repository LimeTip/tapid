// Keep Claude's committed skill tree in sync with the cross-tool source.
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

function files(directory: string): Map<string, Buffer> {
  const result = new Map<string, Buffer>();
  const walk = (relative: string): void => {
    const path = join(directory, relative);
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) throw new Error(`Skill trees must use regular files: ${path}`);
    if (stat.isDirectory()) {
      for (const entry of readdirSync(path).sort()) walk(join(relative, entry));
    } else if (stat.isFile()) {
      result.set(relative, readFileSync(path));
    } else {
      throw new Error(`Unsupported skill entry: ${path}`);
    }
  };
  walk("");
  return result;
}

try {
  const mode = process.argv.slice(2);
  if (mode.length !== 1 || !["--check", "--write"].includes(mode[0])) {
    throw new Error("usage: node --experimental-strip-types scripts/sync-agent-skills.ts --check|--write (from repository root)");
  }
  for (const parent of [".agents", ".claude"]) {
    if (existsSync(parent) && lstatSync(parent).isSymbolicLink()) {
      throw new Error(`Skill trees must use regular directories: ${parent}`);
    }
  }
  const source = files(".agents/skills");
  if (![...source.keys()].some((path) => path.endsWith("SKILL.md"))) {
    throw new Error("No canonical skills found in .agents/skills");
  }
  const targetDirectory = ".claude/skills";
  const target = existsSync(targetDirectory) ? files(targetDirectory) : new Map<string, Buffer>();
  if (mode[0] === "--write") {
    for (const [relative, contents] of source) {
      const path = join(targetDirectory, relative);
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, contents);
    }
  }
  const differences: string[] = [];
  for (const [relative, contents] of source) {
    if (mode[0] === "--write") continue;
    const actual = target.get(relative);
    if (!actual) differences.push(`Missing: ${relative}`);
    else if (!contents.equals(actual)) differences.push(`Changed: ${relative}`);
  }
  for (const relative of target.keys()) {
    if (!source.has(relative)) differences.push(`Extra: ${relative} (remove manually after review)`);
  }
  if (differences.length) {
    throw new Error(`Claude skill mirror differs:\n${differences.join("\n")}`);
  }
  console.log(`Skill mirror ${mode[0] === "--write" ? "updated" : "verified"}: ${source.size} files`);
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
