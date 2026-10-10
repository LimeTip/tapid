import { strict as assert } from "node:assert";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { cargoEnvironment } from "../scripts/dev.ts";

const temporary = (body: (root: string) => void) => {
  const root = mkdtempSync(join(tmpdir(), "tapid-dev-test-"));
  try {
    body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
};
const git = (args: string[], cwd?: string) =>
  execFileSync("git", args, { cwd, stdio: "pipe" });

test("worktrees share cache but unrelated repositories do not", () =>
  temporary((root) => {
    const repo = join(root, "repo"),
      other = join(root, "other"),
      worktree = join(root, "worktree");
    for (const path of [repo, other]) git(["init", "--quiet", path]);
    git(
      ["worktree", "add", "--quiet", "--orphan", "-b", "fixture", worktree],
      repo,
    );
    assert.deepEqual(
      cargoEnvironment(repo, {}),
      cargoEnvironment(worktree, {}),
    );
    assert.notDeepEqual(
      cargoEnvironment(repo, {}),
      cargoEnvironment(other, {}),
    );
    assert.equal(
      cargoEnvironment(repo, {}).CARGO_TARGET_DIR,
      join(realpathSync.native(join(repo, ".git")), "target", "dev"),
    );
  }));
test("explicit target and other environment are preserved", () => {
  const env = { CARGO_TARGET_DIR: "custom-target", RUSTFLAGS: "-Dwarnings" };
  assert.deepEqual(cargoEnvironment("missing-repository", env), env);
});
test("sibling bare repositories keep separate worktree caches", () =>
  temporary((root) => {
    const caches = ["tapid", "other"].map((name) => {
      const repo = join(root, `${name}.git`);
      git(["init", "--quiet", "--bare", repo]);
      const worktrees = ["first", "second"].map((label) =>
        join(root, `${name}-${label}`),
      );
      worktrees.forEach((path, index) =>
        git([
          "--git-dir",
          repo,
          "worktree",
          "add",
          "--quiet",
          "--orphan",
          "-b",
          `fixture-${index}`,
          path,
        ]),
      );
      const first = cargoEnvironment(worktrees[0], {}).CARGO_TARGET_DIR;
      assert.equal(first, cargoEnvironment(worktrees[1], {}).CARGO_TARGET_DIR);
      return first;
    });
    assert.notEqual(caches[0], caches[1]);
  }));
test("nested Git directories keep distinct repository caches", () =>
  temporary((root) => {
    const shared = join(root, "shared.git"),
      worktree = join(root, "bare-worktree");
    git(["init", "--quiet", "--bare", shared]);
    git(["init", "--quiet", shared]);
    git([
      "--git-dir",
      shared,
      "worktree",
      "add",
      "--quiet",
      "--orphan",
      "-b",
      "fixture",
      worktree,
    ]);
    assert.notEqual(
      cargoEnvironment(worktree, {}).CARGO_TARGET_DIR,
      cargoEnvironment(shared, {}).CARGO_TARGET_DIR,
    );
  }));
