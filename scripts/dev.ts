// Run Cargo with a build cache shared by this repository's worktrees.
import { execFileSync, spawnSync } from "node:child_process";
import { realpathSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export function cargoEnvironment(
  root: string,
  environment: NodeJS.ProcessEnv,
): NodeJS.ProcessEnv {
  const env = { ...environment };
  if (!Object.hasOwn(env, "CARGO_TARGET_DIR")) {
    const common = execFileSync(
      "git",
      ["rev-parse", "--path-format=absolute", "--git-common-dir"],
      { cwd: root, encoding: "utf8" },
    ).trim();
    env.CARGO_TARGET_DIR = join(realpathSync(common), "target", "dev");
  }
  return env;
}

export function main(args = process.argv.slice(2)): number {
  if (!args.length) {
    console.error(
      "usage: node --experimental-strip-types scripts/dev.ts <cargo arguments>",
    );
    return 2;
  }
  try {
    const root = dirname(dirname(fileURLToPath(import.meta.url)));
    const result = spawnSync("cargo", args, {
      cwd: root,
      env: cargoEnvironment(root, process.env),
      stdio: "inherit",
    });
    if (result.error) throw result.error;
    return result.status ?? 1;
  } catch (error) {
    console.error(`cannot run development command: ${error}`);
    return 1;
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  process.exitCode = main();
