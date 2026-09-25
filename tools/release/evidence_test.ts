import { ok as assert, rejects as assertRejects } from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { validateReleaseEvidence } from "./evidence.ts";

const root = fileURLToPath(new URL("../../", import.meta.url));
const templatePath = `${root}.github/ISSUE_TEMPLATE/release-operation.md`;

async function template(): Promise<string> {
  return await readFile(templatePath, "utf8");
}

test("release evidence template satisfies the complete operator contract", async () => {
  const markdown = await template();
  validateReleaseEvidence(markdown);
  assert(markdown.includes("docs/release-distribution.md"));
  assert(markdown.includes("docs/releases/0.0.8-operations.md"));
  assert(markdown.includes("Evidence only"));
  assert(markdown.includes("No secrets"));
});

test("release evidence template points to the complete v0.0.8 worked example", async () => {
  const recordPath = `${root}docs/releases/0.0.8-operations.md`;
  const record = await readFile(recordPath, "utf8");
  assert(record.includes("# Tapid v0.0.8 release operations record"));
  assert(record.includes("Annotated tag"));
  assert(record.includes("Public smoke"));
  assert(record.includes("crates.io"));
});

test("release evidence contract rejects missing immutable references", async () => {
  const markdown = await template();
  await assertRejects(async () => validateReleaseEvidence(markdown.replace("- Peeled commit: commit SHA", "")), /Peeled commit/);
});

test("release evidence contract rejects missing asset verification", async () => {
  const markdown = await template();
  await assertRejects(async () => validateReleaseEvidence(markdown.replace("- SHA-256 read-back: each downloaded archive matched the downloaded `SHA256SUMS`", "")), /SHA-256 read-back/);
});

test("release evidence contract rejects missing public smoke evidence", async () => {
  const markdown = await template();
  await assertRejects(async () => validateReleaseEvidence(markdown.replace("- Public smoke run ID and tagged commit:", "")), /Public smoke run ID/);
});

test("release evidence contract rejects missing registry read-back", async () => {
  const markdown = await template();
  await assertRejects(async () => validateReleaseEvidence(markdown.replace("- Registry read-back: every published package/version confirmed independently and not yanked", "")), /Registry read-back/);
});

test("release evidence contract rejects combined approval boundaries", async () => {
  const markdown = await template();
  await assertRejects(async () => validateReleaseEvidence(markdown.replace("- [ ] GitHub publication approval was separate from crates.io approval.", "")), /GitHub publication approval/);
});

test("release evidence contract rejects publication triggers and secret values", async () => {
  const markdown = await template();
  await assertRejects(async () => validateReleaseEvidence(`${markdown}\ncargo publish tapid`), /publication trigger/);
  await assertRejects(async () => validateReleaseEvidence(`${markdown}\ngh workflow dispatch release.yml`), /publication trigger/);
  await assertRejects(async () => validateReleaseEvidence(`${markdown}\npassword: hunter2`), /publication trigger/);
});
