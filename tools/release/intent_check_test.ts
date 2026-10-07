import { match, strictEqual, rejects } from "node:assert/strict";
import { execFile } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { test } from "node:test";

const workflow = () => readFile(new URL("../../.github/workflows/release-intent-check.yml", import.meta.url), "utf8");
const exec = promisify(execFile);

test("release intent freshness runs on every PR so its required status cannot stay absent", async () => {
  const source = await workflow();
  match(source, /pull_request:/);
  match(source, /name: Release intent freshness/);
  strictEqual(/paths(?:-ignore)?:/.test(source), false);
  strictEqual(source.includes("pull_request_target"), false);
});

test("freshness checks have read-only permissions and no persisted checkout credentials", async () => {
  const source = await workflow();
  match(source, /permissions:\n  contents: read/);
  strictEqual(/(?:contents|pull-requests|id-token): write/.test(source), false);
  match(source, /ref: \$\{\{ github\.event\.pull_request\.head\.sha \}\}/);
  match(source, /persist-credentials: false/);
  match(source, /fetch-depth: 0/);
  strictEqual(source.includes("secrets."), false);
});

test("the check binds intent changes to exact event base and head before schema validation", async () => {
  const source = await workflow();
  match(source, /BASE_SHA: \$\{\{ github\.event\.pull_request\.base\.sha \}\}/);
  match(source, /HEAD_SHA: \$\{\{ github\.event\.pull_request\.head\.sha \}\}/);
  match(source, /git diff --quiet "\$BASE_SHA" "\$HEAD_SHA" -- docs\/releases\/intent\.json/);
  match(source, /tools\/release\/candidate\.ts intent/);
  match(source, /\.prepared_from/);
  match(source, /test "\$PREPARED_FROM" = "\$BASE_SHA"/);
  match(source, /git merge-base "\$BASE_SHA" "\$HEAD_SHA"/);
});

test("actual PR check accepts current preparation and note edits, rejects stale source and intent deletion", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid-intent-check-"));
  const git = async (...args: string[]) => (await exec("git", args, { cwd: directory })).stdout.trim();
  try {
    await git("init", "-b", "main");
    await git("config", "user.name", "Fixture");
    await git("config", "user.email", "fixture@example.test");
    await writeFile(join(directory, "baseline"), "fixture");
    await git("add", "baseline");
    await git("commit", "-m", "baseline");
    const base = await git("rev-parse", "HEAD");
    await mkdir(join(directory, "docs/releases"), { recursive: true });
    await mkdir(join(directory, "tools"));
    await cp(fileURLToPath(new URL(".", import.meta.url)), join(directory, "tools/release"), { recursive: true });
    const intentPath = join(directory, "docs/releases/intent.json");
    const intent = { schema: "tapid-release-intent-v1", prepared_from: base, version: "0.0.12", baseline: "v0.0.11", notes: "docs/releases/0.0.12.md", packages: [{ name: "tapid", version: "0.0.12" }] };
    await writeFile(intentPath, JSON.stringify(intent));
    await git("add", "docs");
    await git("commit", "-m", "prepare release");
    const source = await workflow();
    const script = source.split("        run: |\n")[1].split("\n").map((line) => line.replace(/^ {10}/, "")).join("\n");
    const check = async () => exec("bash", ["-c", script], { cwd: directory, env: { PATH: process.env.PATH, BASE_SHA: base, HEAD_SHA: await git("rev-parse", "HEAD"), RUNNER_TEMP: directory, GITHUB_REPOSITORY: "LimeTip/tapid" } });
    await check();
    await writeFile(join(directory, "docs/releases/0.0.12.md"), "Maintainer-edited notes");
    await git("add", "docs");
    await git("commit", "-m", "review notes");
    await check();
    await writeFile(intentPath, JSON.stringify({ ...intent, prepared_from: "f".repeat(40) }));
    await git("add", "docs");
    await git("commit", "-m", "stale source");
    await rejects(check(), /preparation is stale/);
    await git("rm", "docs/releases/intent.json");
    await git("commit", "-m", "remove intent");
    // Deletion compared with a base that contained the release intent is rejected.
    const releaseBase = await git("rev-parse", "HEAD^1");
    await rejects(exec("bash", ["-c", script], { cwd: directory, env: { PATH: process.env.PATH, BASE_SHA: releaseBase, HEAD_SHA: await git("rev-parse", "HEAD"), RUNNER_TEMP: directory, GITHUB_REPOSITORY: "LimeTip/tapid" } }), /ENOENT/);
    await git("checkout", "--detach", base);
    await writeFile(join(directory, "ordinary-change"), "not a release");
    await git("add", "ordinary-change");
    await git("commit", "-m", "normal PR");
    match((await check()).stdout, /intent unchanged/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
