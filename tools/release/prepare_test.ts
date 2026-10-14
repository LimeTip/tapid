import { deepStrictEqual, match, rejects, strictEqual, throws } from "node:assert/strict";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
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

test("an unpublished main version is reused without a second bump", () => {
  strictEqual(nextProductVersion("0.0.12", "", "0.0.11"), "0.0.12");
  strictEqual(nextProductVersion("0.0.12", "0.0.12", "0.0.11"), "0.0.12");
  strictEqual(nextProductVersion("0.0.12", "0.1.0", "0.0.11"), "0.1.0");
  throws(() => nextProductVersion("0.0.13", "0.0.12", "0.0.11"));
  throws(() => nextProductVersion("0.0.12", "0.0.11", "0.0.11"));
  throws(() => nextProductVersion("0.0.10", "", "0.0.11"));
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

test("preparation packages uncommitted version bumps and still verifies compilation", async () => {
  const exec = promisify(execFile);
  const workflow = await readFile(new URL("../../.github/workflows/release-prepare.yml", import.meta.url), "utf8");
  const args = workflow.match(/^\s+cargo (package[^\n]+)$/m)![1].trim().split(/\s+/);
  const cargo = (await exec("rustup", ["which", "cargo"])).stdout.trim();
  const rustc = (await exec("rustup", ["which", "rustc"])).stdout.trim();
  const directory = await realpath(await mkdtemp(join(tmpdir(), "tapid-prepare-package-")));
  const env = { PATH: process.env.PATH, HOME: join(directory, "home"), CARGO_HOME: join(directory, "cargo-home"), RUSTC: rustc, GIT_CONFIG_NOSYSTEM: "1" };
  const run = (command: string, arguments_: string[]) => exec(command, arguments_, { cwd: directory, env });
  const manifest = (version: string) => `[package]\nname = "tapid-preparation-fixture"\nversion = "${version}"\nedition = "2021"\n[workspace]\n`;
  try {
    await mkdir(join(directory, "src"));
    await mkdir(env.HOME);
    await writeFile(join(directory, "Cargo.toml"), manifest("0.0.1"));
    await writeFile(join(directory, "src/lib.rs"), "pub fn version() -> u8 { 1 }\n");
    await run(cargo, ["generate-lockfile", "--offline"]);
    await run("git", ["init"]);
    await run("git", ["add", "."]);
    await run("git", ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "initial fixture"]);
    await writeFile(join(directory, "Cargo.toml"), manifest("0.0.2"));
    await run(cargo, ["generate-lockfile", "--offline"]);
    await run(cargo, [...args, "--offline"]);
    strictEqual((await readFile(join(directory, "target/package/tapid-preparation-fixture-0.0.2.crate"))).length > 0, true);
    await writeFile(join(directory, "src/lib.rs"), "invalid Rust syntax\n");
    await rejects(run(cargo, [...args, "--offline"]), /could not compile|failed to verify/);
  } finally { await rm(directory, { recursive: true, force: true }); }
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

test("already-bumped preparation preserves reviewed notes and refreshes the release intent", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid-prepare-bumped-"));
  const commands: string[] = [];
  let version = "0.0.12";
  let proposed = "0.0.12";
  let ancestryValid = true;
  const run = async (command: string, args: string[]) => {
    commands.push(`${command} ${args.join(" ")}`);
    if (command === "cargo" && args[0] === "metadata") return JSON.stringify({ packages: [{ name: "tapid", version, dependencies: [] }] });
    if (command === "git" && args[0] === "cat-file") return "tag";
    if (command === "git" && args[0] === "rev-parse") return "d".repeat(40);
    if (command === "git" && args[0] === "merge-base" && !ancestryValid) throw new Error("baseline is not an ancestor");
    if (command === "git" && args[0] === "log") return `${"c".repeat(40)}\tReviewed version bump`;
    if (command === "release-plz" && args[0] === "update") version = proposed;
    if (command === "release-plz" && args[0] === "set-version") version = args[1].split("@")[1];
    return "";
  };
  const options = { directory, run, lookup: async () => ({ published: false, latestVersion: "0.0.11" }) };
  try {
    await writeFile(join(directory, "Cargo.toml"), "fixture");
    await writeFile(join(directory, "Cargo.lock"), "fixture");
    await mkdir(join(directory, "docs/releases"), { recursive: true });
    const reviewedNotes = "# Tapid 0.0.12 release notes\n\nHandwritten release highlights.\n";
    await writeFile(join(directory, "docs/releases/0.0.12.md"), reviewedNotes);
    await writeFile(join(directory, "docs/releases/intent.json"), "stale intent");
    await writeFile(join(directory, "release-preparation.json"), "stale preparation");
    await prepareRelease("", "v0.0.11", options);
    const intent = JSON.parse(await readFile(join(directory, "docs/releases/intent.json"), "utf8"));
    strictEqual(intent.version, "0.0.12");
    strictEqual(intent.baseline, "v0.0.11");
    strictEqual(intent.prepared_from, "d".repeat(40));
    deepStrictEqual(intent.packages, [{ name: "tapid", version: "0.0.12" }]);
    strictEqual(await readFile(join(directory, intent.notes), "utf8"), reviewedNotes);
    const preparation = JSON.parse(await readFile(join(directory, "release-preparation.json"), "utf8"));
    strictEqual(preparation.notes, intent.notes);
    deepStrictEqual(preparation.packages, intent.packages);
    strictEqual(commands.includes("release-plz update"), true);
    strictEqual(commands.includes("git merge-base --is-ancestor refs/tags/v0.0.11^{commit} HEAD"), true);
    await rm(join(directory, intent.notes));
    await rm(join(directory, "docs/releases/intent.json"));
    await writeFile(join(directory, intent.notes), " \n");
    commands.length = 0;
    await rejects(prepareRelease("", "v0.0.11", options), /nonempty regular file/);
    strictEqual(commands.some((command) => command.startsWith("release-plz ") || command.startsWith("cargo update ")), false);
    strictEqual(await readFile(join(directory, intent.notes), "utf8"), " \n");
    await rejects(readFile(join(directory, "docs/releases/intent.json")), /ENOENT/);
    await rm(join(directory, intent.notes));
    await mkdir(join(directory, intent.notes));
    commands.length = 0;
    await rejects(prepareRelease("", "v0.0.11", options), /nonempty regular file/);
    strictEqual(commands.some((command) => command.startsWith("release-plz ") || command.startsWith("cargo update ")), false);
    await rm(join(directory, intent.notes), { recursive: true });
    commands.length = 0;
    ancestryValid = false;
    await rejects(prepareRelease("", "v0.0.11", options), /not an ancestor/);
    strictEqual(commands.includes("release-plz update"), false);
    ancestryValid = true;
    proposed = "0.1.0";
    await rejects(prepareRelease("", "v0.0.11", options), /version analysis requires 0.1.0/);
    await rejects(readFile(join(directory, "docs/releases/intent.json")), /ENOENT/);
    version = "0.0.10";
    commands.length = 0;
    await rejects(prepareRelease("", "v0.0.11", options), /main.*baseline/);
    strictEqual(commands.includes("release-plz update"), false);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
