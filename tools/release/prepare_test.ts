import { deepStrictEqual, match, rejects, strictEqual, throws } from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { nextProductVersion, preparationFiles, prepareRelease, refreshLockfiles } from "./prepare.ts";

test("product defaults to next patch and accepts only a newer stable explicit version", () => {
  strictEqual(nextProductVersion("0.0.11", ""), "0.0.12");
  strictEqual(nextProductVersion("0.0.11", "0.1.0"), "0.1.0");
  for (const version of ["0.0.11", "0.0.10", "v0.0.12", "0.0.12-rc.1", "01.0.0", "../notes"]) {
    throws(() => nextProductVersion("0.0.11", version));
  }
});

test("baseline drift blocks preparation", () => {
  throws(() => preparationFiles("0.0.12", "v0.0.12", "a".repeat(40), [{ name: "tapid", version: "0.0.12" }], []), /baseline/);
});

test("complete notes and intent record the exact planned packages and escaped history", () => {
  const packages = [{ name: "tapid-core", version: "0.0.7" }, { name: "tapid", version: "0.0.12" }];
  const files = preparationFiles("0.0.12", "v0.0.11", "a".repeat(40), packages, [
    { sha: "a".repeat(40), title: "Fix [parser]\nhtml <script>", pr: 123 },
    { sha: "b".repeat(40), title: "Installer maintenance" },
  ]);
  strictEqual(files.notesPath, "docs/releases/0.0.12.md");
  deepStrictEqual(JSON.parse(files.intent).packages, packages);
  strictEqual(JSON.parse(files.intent).prepared_from, "a".repeat(40));
  match(files.notes, /tapid-core.*0\.0\.7/);
  match(files.notes, /pull\/123/);
  match(files.notes, /commit\/bbbbbbbb/);
  strictEqual(files.notes.includes("<script>"), false);
  throws(() => preparationFiles("0.0.12", "v0.0.11", "a".repeat(40), [{ name: "tapid", version: "0.0.13" }], []), /product/);
  throws(() => preparationFiles("0.0.12", "v0.0.11", "main", packages, []), /source/);
});

test("nested workspace lock refresh is followed by locked verification", async () => {
  const commands: string[][] = [];
  await refreshLockfiles(["Cargo.lock", "tests/integration/Cargo.lock"], async (_command, args) => { commands.push(args); return ""; });
  deepStrictEqual(commands, [
    ["update", "--workspace", "--manifest-path", "Cargo.toml"],
    ["metadata", "--no-deps", "--format-version", "1", "--locked", "--manifest-path", "Cargo.toml"],
    ["update", "--workspace", "--manifest-path", "tests/integration/Cargo.toml"],
    ["metadata", "--no-deps", "--format-version", "1", "--locked", "--manifest-path", "tests/integration/Cargo.toml"],
  ]);
  await rejects(refreshLockfiles(["../Cargo.lock"], async () => ""), /lockfile/);
  await rejects(refreshLockfiles(["Cargo.lock"], async () => { throw new Error("registry unavailable"); }), /registry unavailable/);
});

test("preparation workflow is main-only, uses App only after checks, and never publishes", async () => {
  const workflow = await readFile(new URL("../../.github/workflows/release-prepare.yml", import.meta.url), "utf8");
  match(workflow, /github\.ref == 'refs\/heads\/main'/);
  match(workflow, /RELEASE_APP_PRIVATE_KEY/);
  match(workflow, /persist-credentials: false/);
  strictEqual(workflow.includes("pull_request_target:"), false);
  strictEqual(workflow.includes("release-plz release"), false);
  strictEqual(workflow.includes("cargo publish"), false);
  strictEqual(workflow.indexOf("Prepare complete release tree") < workflow.indexOf("write-pr:"), true);
  const preparation = workflow.split("  write-pr:")[0];
  strictEqual(preparation.includes("RELEASE_APP_PRIVATE_KEY"), false);
  const writer = workflow.split("  write-pr:")[1];
  strictEqual(writer.includes("cargo "), false);
  match(writer, /artifact-ids: \$\{\{ needs\.prepare\.outputs\.artifact_id \}\}/);
  match(writer, /git apply --index/);
  match(workflow, /pulls\?state=open&base=main&head=.*:release\/prepare/);
  match(workflow, /name: Open complete release PR\n\s+if: steps\.existing\.outputs\.url == ''/);
  match(workflow, /name: Prepare complete release tree\n\s+if: steps\.existing\.outputs\.url == ''/);
});

test("tooling-only preparation uses maintained version tool and creates both final files", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid-prepare-"));
  const commands: string[] = [];
  let version = "0.0.11";
  try {
    await writeFile(join(directory, "Cargo.toml"), "fixture");
    await writeFile(join(directory, "Cargo.lock"), "fixture");
    const run = async (command: string, args: string[]) => {
      commands.push(`${command} ${args.join(" ")}`);
      if (command === "cargo" && args[0] === "metadata") return JSON.stringify({ packages: [{ name: "tapid", version, dependencies: [] }] });
      if (command === "git" && args[0] === "cat-file") return "tag";
      if (command === "git" && args[0] === "rev-parse") return "d".repeat(40);
      if (command === "git" && args[0] === "log") return `${"c".repeat(40)}\tFix workflow (#204)`;
      if (command === "release-plz" && args[0] === "set-version") {
        strictEqual(args[2], "--config", "set-version needs an isolated changelog config even when changelog updates are disabled");
        match(await readFile(args[3], "utf8"), /changelog_path/);
        version = args[1].split("@")[1];
      }
      return "";
    };
    await prepareRelease("", "v0.0.11", { directory, run, lookup: async () => ({ published: false, latestVersion: "0.0.11" }) });
    const intent = JSON.parse(await readFile(join(directory, "docs/releases/intent.json"), "utf8"));
    strictEqual(intent.version, "0.0.12");
    strictEqual(intent.prepared_from, "d".repeat(40));
    strictEqual(commands.indexOf("git rev-parse HEAD") < commands.indexOf("release-plz update"), true);
    strictEqual(commands.includes("release-plz update"), true);
    strictEqual(commands.some((command) => command.startsWith("release-plz set-version tapid@0.0.12 --config ")), true);
    match(await readFile(join(directory, intent.notes), "utf8"), /Fix workflow/);
    await rm(join(directory, intent.notes));
    version = "0.0.11";
    await rejects(prepareRelease("", "v0.0.11", { directory, run, lookup: async () => { throw new Error("registry outage"); } }), /registry outage/);
    await rejects(readFile(join(directory, intent.notes)), /ENOENT/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
