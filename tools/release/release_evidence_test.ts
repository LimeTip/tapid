import { match as assertMatch, ok as assert, throws as assertThrows } from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const templatePath = new URL("../../.github/ISSUE_TEMPLATE/release-evidence.md", import.meta.url);
const operationsRecordPath = new URL("../../docs/releases/0.0.8-operations.md", import.meta.url);

const requiredEvidence = [
  "Product version:",
  "Preparation PR URL:",
  "Preparation PR final head:",
  "Merge SHA:",
  "Post-merge checks on merge SHA:",
  "Product-vs-supporting-crate version distinction confirmed:",
  "Changed supporting crates (one row per crate; package, version, and reason are independent fields):",
  "Annotated tag object:",
  "Peeled commit:",
  "Binary workflow run ID:",
  "Recovery workflow run ID",
  "Draft/public release ID:",
  "Exact assets (name, numeric asset ID, provider size, and SHA-256 verification for each):",
  "Release notes read-back:",
  "Independent GitHub publication approval:",
  "Public release read-back:",
  "Public-smoke workflow run ID:",
  "Tagged commit used by public smoke:",
  "Job 1 conclusion:",
  "Job 2 conclusion:",
  "Job 3 conclusion:",
  "crates.io dry-run plan:",
  "Trusted Publisher verification:",
  "Independent crates.io approval:",
  "crates.io workflow run:",
  "Published package/version set:",
  "Clean locked cargo-install command/output:",
  "Final endpoint/repository-state read-back:",
  "Limitations:",
  "Follow-ups:",
];

const workedV008Record = `
Product version: v0.0.8
Preparation PR URL: https://github.com/LimeTip/tapid/pull/120
Preparation PR final head: 35073b495b322a4619806793a204325f2867253a
Merge SHA: 35073b495b322a4619806793a204325f2867253a
Post-merge checks on merge SHA: all required checks passed
Product-vs-supporting-crate version distinction confirmed: product v0.0.8; supporting crates retain truthful versions
tapid-lockfile | 0.0.8 | changed dependency requirements
Annotated tag: v0.0.8
Annotated tag object: 8ba56bbb4757edf43ab6d758499ad4cbdc778d0b
Peeled commit: 35073b495b322a4619806793a204325f2867253a
Binary workflow run ID: 33970253804
Recovery workflow run ID: 33970253804
Draft/public release ID: 383262465
Asset: SHA256SUMS | 545895105 | 649 | verified
Asset: tapid-0.0.8-x86_64-unknown-linux-gnu.tar.gz | 545895091 | 2928383 | 2ce12c015b2bda71066318cbed0a2f3c839f1f2141eb1383d9667fc34b66c4a9
Release notes read-back: exact reviewed body read back
Independent GitHub publication approval: independent reviewer approved release 383262465
Public release read-back: release 383262465 is public with exact asset set
Public-smoke workflow run ID: 33976229757
Tagged commit used by public smoke: 35073b495b322a4619806793a204325f2867253a
Job 1 conclusion: Unix installer passed
Job 2 conclusion: macOS installer passed
Job 3 conclusion: Windows installer passed
crates.io dry-run plan: dependency-ordered package/version set had no missing packages
Trusted Publisher verification: workflow and protected environment verified
Independent crates.io approval: independent reviewer approved protected environment
crates.io workflow run: 33976531200 at e9495179f826b7931560ae284df2f321dde28d7e
Published package/version set: tapid-lockfile@0.0.8 and tapid@0.0.8 read back from registry
Clean locked cargo-install command/output: isolated install returned tapid 0.0.8 and help succeeded
Final endpoint/repository-state read-back: public release, tag, registry, workflow, and repository all read back
Limitations: checksum and archive share the GitHub trust boundary
Follow-ups: https://github.com/LimeTip/tapid/issues/123
`;

function validateWorkedRecord(record: string): void {
  assertMatch(record, /Product version:\s+v\d+\.\d+\.\d+/);
  assertMatch(record, /Product-vs-supporting-crate version distinction confirmed:[^\n]*product[^\n]*supporting/i);
  assertMatch(record, /(?:^|\n)[^\n|]+\|\s*\d+\.\d+\.\d+\s*\|\s*[^\n|]+/);
  for (const field of ["Preparation PR final head", "Merge SHA", "Annotated tag object", "Peeled commit", "Tagged commit used by public smoke"]) {
    assertMatch(record, new RegExp(`${field}:\\s+[0-9a-f]{40}(?:\\s|$)`, "i"));
  }
  for (const field of ["Binary workflow run ID", "Public-smoke workflow run ID", "crates.io workflow run", "Draft/public release ID"]) {
    assertMatch(record, new RegExp(`${field}:[^\\n]*\\b\\d+\\b`));
  }
  assertMatch(record, /Asset:[^\n|]+\|\s*\d+\s*\|\s*\d+\s*\|/);
  for (const field of ["Independent GitHub publication approval", "Public release read-back", "Independent crates.io approval", "Final endpoint/repository-state read-back"]) {
    assertMatch(record, new RegExp(`${field}:[^\\n]+\\S`));
  }
  assert(!/\b(password|token|cookie|secret|credential|signing material)\b/i.test(record), "release evidence must not contain authentication or signing material");
  assert(!/(?:git\s+push|cargo\s+publish|gh\s+(?:release|workflow)\s+(?:create|publish|run)|workflow_dispatch)/i.test(record), "release evidence must not contain publication-triggering content");
}

function validateTemplate(template: string): void {
  for (const field of requiredEvidence) {
    assert(template.includes(field), `missing required release evidence field: ${field}`);
  }
  assert(
    template.includes("product version is independent from each supporting-crate version"),
    "product and supporting-crate versions must remain distinct",
  );
  assert(
    !/\b(password|token|cookie|secret|credential)\b/i.test(template),
    "release evidence must not request authentication material",
  );
  assert(
    !/(?:git\s+push|cargo\s+publish|gh\s+(?:release|workflow)\s+(?:create|publish|run)|workflow_dispatch)/i.test(template),
    "release evidence must not contain publication-triggering content",
  );
}

test("release evidence template covers the complete operator ledger", async () => {
  const template = await readFile(templatePath, "utf8");
  validateTemplate(template);
  assert(template.includes("../../docs/release-distribution.md"));
});

test("release evidence template is grounded in the v0.0.8 operations record", async () => {
  const record = await readFile(operationsRecordPath, "utf8");
  for (const immutableEvidence of [
    "GitHub release ID: `383262465`",
    "Annotated tag object: `8ba56bbb4757edf43ab6d758499ad4cbdc778d0b`",
    "Tagged commit: `35073b495b322a4619806793a204325f2867253a`",
    "Public installer smoke run: [`33976229757`]",
    "crates.io publication run: [`33976531200`]",
  ]) {
    assert(record.includes(immutableEvidence), `operations record is missing ${immutableEvidence}`);
  }
  assert(record.includes("## GitHub release evidence"));
  assert(record.includes("## Public installer evidence"));
  assert(record.includes("## crates.io evidence"));
});

test("release evidence contract rejects a missing required field", async () => {
  const template = await readFile(templatePath, "utf8");
  assertThrows(() => validateTemplate(template.replace("Limitations:", "Known limitations:")), /missing required/);
});

test("release evidence contract rejects credentials and version conflation", async () => {
  const template = await readFile(templatePath, "utf8");
  assertThrows(() => validateTemplate(`${template}\n- GitHub token: <!-- do not add -->`), /authentication material/);
  assertThrows(
    () => validateTemplate(template.replace("product version is independent from each supporting-crate version", "all packages use the product version")),
    /versions must remain distinct/,
  );
});

test("worked v0.0.8 evidence fixture satisfies immutable read-back contract", () => {
  validateWorkedRecord(workedV008Record);
});

test("release evidence contract rejects missing immutable IDs, approvals, and triggers", () => {
  assertThrows(() => validateWorkedRecord(workedV008Record.replace("Annotated tag object: 8ba56bbb4757edf43ab6d758499ad4cbdc778d0b", "Annotated tag object:")), /Annotated tag object/);
  assertThrows(() => validateWorkedRecord(workedV008Record.replace("Independent crates.io approval: independent reviewer approved protected environment", "Independent crates.io approval:")), /Independent crates\.io approval/);
  assertThrows(() => validateWorkedRecord(workedV008Record.replace("Public release read-back: release 383262465 is public with exact asset set", "Public release read-back:")), /Public release read-back/);
  assertThrows(() => validateWorkedRecord(`${workedV008Record}\ncargo publish --dry-run`), /publication-triggering/);
  assertThrows(() => validateWorkedRecord(`${workedV008Record}\nGitHub token: omitted`), /authentication or signing/);
});
