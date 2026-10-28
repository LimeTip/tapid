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
Preparation PR final head: 1231d6f3040ca7fa0a3de9f00808f18f737397c4
Merge SHA: 35073b495b322a4619806793a204325f2867253a
Post-merge checks on merge SHA: all required checks passed
Product-vs-supporting-crate version distinction confirmed: product v0.0.8; supporting crates retain truthful versions
Changed supporting crates: none during preparation; publication membership listed below
Generated publication matrix: retained planner output summary from operations record
| Dependency order | Package | Exact version | Publication status | Final registry read-back |
| ---: | --- | --- | --- | --- |
| 1 | tapid-archive | 0.0.3 | published | available, not yanked |
| 2 | tapid-core | 0.0.4 | published | available, not yanked |
| 3 | tapid-manifest | 0.0.6 | published | available, not yanked |
| 4 | tapid-linker | 0.0.4 | published | available, not yanked |
| 5 | tapid-lockfile | 0.0.8 | published | available, not yanked |
| 6 | tapid-registry-client | 0.0.4 | published | available, not yanked |
| 7 | tapid-resolver | 0.0.4 | published | available, not yanked |
| 8 | tapid-store | 0.0.4 | published | available, not yanked |
| 9 | tapid | 0.0.8 | published | available, not yanked |
Annotated tag: v0.0.8
Annotated tag object: 8ba56bbb4757edf43ab6d758499ad4cbdc778d0b
Peeled commit: 35073b495b322a4619806793a204325f2867253a
Binary workflow run ID: 33970253804
Recovery workflow run ID: 33970253804
Draft/public release ID: 383262465
Asset: SHA256SUMS | 545895105 | 649 | Checksum manifest
Asset: tapid-0.0.8-aarch64-apple-darwin.tar.gz | 545894708 | 2642424 | 1ffdb9cf25d5bbd38d669566c85994486525def42cb1a0c24aa6bac30cb08762
Asset: tapid-0.0.8-aarch64-pc-windows-msvc.tar.gz | 545895043 | 2355370 | cca767a12888642e7472f0521215b5e5d0c675688abd4e4a1a1e56ce015acdf2
Asset: tapid-0.0.8-aarch64-unknown-linux-gnu.tar.gz | 545895056 | 2876698 | 67123cdcb6f5ec4243a0d7b84df87c5bc331d11f94c7b0208bbd0abb0124fdaa
Asset: tapid-0.0.8-x86_64-apple-darwin.tar.gz | 545895062 | 2786377 | b97754260e92fb5272e9d5404abaa8395430a2624b94b00fa15668ad498e80ec
Asset: tapid-0.0.8-x86_64-pc-windows-msvc.tar.gz | 545895080 | 2517609 | b82405f88a3ef9bd23362d2af4f89dd97c3c4a3fb7cc2fb7b18416fa9ba41653
Asset: tapid-0.0.8-x86_64-unknown-linux-gnu.tar.gz | 545895091 | 2928383 | 2ce12c015b2bda71066318cbed0a2f3c839f1f2141eb1383d9667fc34b66c4a9
Archive verification result: all six archive hashes matched downloaded SHA256SUMS; exact names and provider sizes matched (recorded summary).
Release notes read-back: exact reviewed body read back
Independent GitHub publication approval: historical separate lane; reviewer identity and timestamp not retained in the operations record
Public release read-back: release 383262465 is public with exact asset set
Public-smoke workflow run ID: 33976229757
Tagged commit used by public smoke: 35073b495b322a4619806793a204325f2867253a
Job 1 conclusion: Unix installer (ubuntu-latest): success
Job 2 conclusion: Unix installer (macos-latest): success
Job 3 conclusion: Windows installer: success
crates.io dry-run plan: dependency-ordered package/version set had no missing packages
Trusted Publisher verification: workflow and protected environment verified
Independent crates.io approval: independent reviewer approved protected environment
crates.io workflow run: 33976531200 at e9495179f826b7931560ae284df2f321dde28d7e
Published package/version set: all nine matrix entries independently read back as available and not yanked; second dry-run had no missing packages
Clean locked cargo-install command/output: recorded summary, not transcript: cargo install tapid --version 0.0.8 --locked with isolated Cargo home/target succeeded; tapid --version returned tapid 0.0.8 and tapid --help succeeded
Final endpoint/repository-state read-back: public release, tag, registry, workflow, and repository all read back
Limitations: checksum and archive share the GitHub trust boundary
Follow-ups: https://github.com/LimeTip/tapid/issues/123
`;

function validateWorkedRecord(record: string): void {
  assertMatch(record, /Product version:\s+v\d+\.\d+\.\d+/);
  assertMatch(record, /Product-vs-supporting-crate version distinction confirmed:[^\n]*product[^\n]*supporting/i);
  validateMatrix(record);
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
  for (const field of [...requiredEvidence, ...currentFields, "Generated publication matrix:", matrixColumns, recoveryColumns]) {
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

test("v0.0.8 preserves the evidenced preparation head separately from its merge", async () => {
  assertMatch(workedV008Record, /Preparation PR final head: 1231d6f3040ca7fa0a3de9f00808f18f737397c4/);
  const record = await readFile(operationsRecordPath, "utf8");
  assert(record.includes("Preparation PR final head: `1231d6f3040ca7fa0a3de9f00808f18f737397c4`"));
});

const currentFields = [
  "Release lane:", "Activation state:", "Coordinator workflow:", "Coordinator run URL/ID/attempt:",
  "Workflow implementation commit:", "Immutable release source:", "Retained candidate summary:",
  "Candidate identity:", "Independent reviewer:", "Approval timestamp:", "Approval result/read-back:",
  "Approved candidate identity:", "Recovery approvals:",
];

const recoveryColumns = "| Recovery run URL/ID/attempt | Workflow implementation commit | Immutable source | Candidate identity | Renewed stable-release approval | Independent reviewer | Timestamp | Approval read-back |";

function validateCurrentRecord(record: string): void {
  for (const field of currentFields) {
    assert(record.split("\n").some((line) => line.startsWith(field) && line.slice(field.length).trim()), `missing ${field}`);
  }
  for (const binding of ["source", "reviewed notes digest", "unsigned artifact identities/digests", "exact crate plan"]) {
    assert(record.split("\n").some((line) => line.startsWith("Retained candidate summary:") && line.includes(binding)), `missing candidate binding: ${binding}`);
  }
  assertMatch(record, /Coordinator workflow: crates-publication\.yml/);
  assertMatch(record, /Approval result\/read-back: stable-release approved/);
  const identity = record.match(/^Candidate identity: (.+)$/m)?.[1];
  assert(record.includes(`Approved candidate identity: ${identity}\n`), "approval must bind candidate identity");
  if (!record.includes("Recovery approvals: not applicable")) {
    assert(record.includes(recoveryColumns), "missing recovery table columns");
    const table = record.slice(record.indexOf(recoveryColumns)).split("\n");
    const rows = table.slice(2).filter((line) => line.startsWith("|"));
    assert(rows.length > 0, "missing recovery run");
    for (const row of rows) {
      const cells = row.split("|").slice(1, -1).map((cell) => cell.trim());
      assert(cells.length === 8 && cells.every((cell) => cell && !cell.includes("<!--")), "incomplete recovery row");
      assertMatch(cells[1], /^[0-9a-f]{40}$/);
      assertMatch(cells[2], /^[0-9a-f]{40}$/);
      assert(cells[3] === identity, "recovery must approve same candidate");
      assert(cells[4] === "stable-release approved", "missing renewed approval");
    }
  }
}

// Synthetic contract example, not evidence of an actual approval or release.
const currentRecord = `Release lane: ADR 0007 automatic
Activation state: additional crates.io reviewer retained; approval recorded separately
Coordinator workflow: crates-publication.yml
Coordinator run URL/ID/attempt: https://example.invalid/runs/42 / 42 / 1
Workflow implementation commit: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
Immutable release source: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
Retained candidate summary: source, reviewed notes digest, unsigned artifact identities/digests, exact crate plan at https://example.invalid/candidate
Candidate identity: synthetic-candidate-sha256
Independent reviewer: synthetic-reviewer, independent of initiating actor
Approval timestamp: 2026-10-20T12:00:00Z
Approval result/read-back: stable-release approved at https://example.invalid/approval
Approved candidate identity: synthetic-candidate-sha256
Recovery approvals: required for each new run
${recoveryColumns}
| --- | --- | --- | --- | --- | --- | --- | --- |
| https://example.invalid/runs/43 / 43 / 1 | aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb | synthetic-candidate-sha256 | stable-release approved | synthetic-reviewer | 2026-10-20T13:00:00Z | https://example.invalid/approval-43 |
`;

test("current lane records coordinator-bound approval and renewed recovery approval", async () => {
  validateCurrentRecord(currentRecord);
  validateCurrentRecord(currentRecord.replace(/Recovery approvals:[\s\S]*/, "Recovery approvals: not applicable\n"));
  for (const field of currentFields) {
    assertThrows(() => validateCurrentRecord(currentRecord.split("\n").filter((line) => !line.startsWith(field)).join("\n")));
  }
  assertThrows(() => validateCurrentRecord(currentRecord.replace("| stable-release approved", "| pending")));
  assertThrows(() => validateCurrentRecord(currentRecord.replace("Approved candidate identity: synthetic-candidate-sha256", "Approved candidate identity: other")), /bind candidate/);
  const template = await readFile(templatePath, "utf8");
  for (const field of currentFields) {
    assertThrows(() => validateTemplate(template.replace(field, "omitted:")), /missing required/);
  }
  for (const binding of ["reviewed notes digest", "unsigned artifact identities/digests", "exact crate plan"]) {
    assertThrows(() => validateCurrentRecord(currentRecord.replace(binding, "omitted")), /candidate binding/);
  }
  assert(template.includes("does not authorize changing protection settings"));
  assert(!template.includes("GitHub publication and crates.io publication were approved independently."));
});

test("recovery approvals use the template table and reject incomplete rows", async () => {
  const template = await readFile(templatePath, "utf8");
  const table = template.slice(template.indexOf("| Recovery run URL/ID/attempt |"), template.indexOf("## GitHub source")).trim();
  const values = ["https://example.invalid/runs/43 / 43 / 1", "a".repeat(40), "b".repeat(40), "synthetic-candidate-sha256", "stable-release approved", "synthetic-reviewer", "2026-10-20T13:00:00Z", "https://example.invalid/approval-43"];
  const filled = table.replace(/<!--[^>]*-->/g, () => values.shift()!);
  const record = currentRecord.slice(0, currentRecord.indexOf(recoveryColumns)) + filled + "\n";
  validateCurrentRecord(record);
  const row = filled.split("\n")[2];
  validateCurrentRecord(record + row.replace("runs/43 / 43", "runs/44 / 44") + "\n");
  for (let index = 1; index <= 8; index++) {
    const cells = row.split("|");
    cells[index] = " ";
    assertThrows(() => validateCurrentRecord(record.replace(row, cells.join("|"))), /incomplete recovery/);
  }
  assertThrows(() => validateCurrentRecord(record.replace(row, "")), /missing recovery run/);
  assertThrows(() => validateCurrentRecord(record + row.replace("synthetic-candidate-sha256", "other") + "\n"), /same candidate/);
  for (const column of recoveryColumns.split("|").slice(1, -1)) {
    assertThrows(() => validateTemplate(template.replace(recoveryColumns, recoveryColumns.replace(column.trim(), "omitted"))), /missing required/);
  }
});

const matrixColumns = "| Dependency order | Package | Exact version | Publication status | Final registry read-back |";

function validateMatrix(record: string): void {
  assert(record.includes(matrixColumns), "missing generated matrix columns");
  assertMatch(record, /Generated publication matrix: retained planner output/);
  assertMatch(record, /\| 1 \| tapid-archive \| 0\.0\.3 \| published \| available, not yanked \|/);
  assertMatch(record, /\| 9 \| tapid \| 0\.0\.8 \| published \| available, not yanked \|/);
}

test("generated matrix keeps publication membership separate from changed crates", async () => {
  const template = await readFile(templatePath, "utf8");
  assert(template.includes(matrixColumns), "missing generated matrix columns");
  assert(template.includes("Generated publication matrix:"));
  for (const column of ["Dependency order", "Exact version", "Publication status", "Final registry read-back"]) {
    assertThrows(() => validateTemplate(template.replace(column, "omitted")), /missing required/);
  }
  validateMatrix(workedV008Record);
  assertThrows(() => validateMatrix(workedV008Record.replace("Exact version", "Product version")), /matrix columns/);
  assertThrows(() => validateMatrix(workedV008Record.replaceAll("0.0.3", "0.0.8")), /tapid-archive/);
  assertThrows(() => validateMatrix(workedV008Record.replace("Generated publication matrix: retained planner output", "Generated publication matrix:")), /planner output/);
});

function validateHistoricalRecord(record: string, operations: string): void {
  assertMatch(record, /Preparation PR final head: 1231d6f3040ca7fa0a3de9f00808f18f737397c4/);
  assertMatch(record, /Merge SHA: 35073b495b322a4619806793a204325f2867253a/);
  const packages = ["tapid-archive 0.0.3", "tapid-core 0.0.4", "tapid-manifest 0.0.6", "tapid-linker 0.0.4", "tapid-lockfile 0.0.8", "tapid-registry-client 0.0.4", "tapid-resolver 0.0.4", "tapid-store 0.0.4", "tapid 0.0.8"];
  packages.forEach((entry, index) => {
    assert(operations.includes(`\`${entry}\``), `historical package ${entry}`);
    const [name, version] = entry.split(" ");
    assert(record.includes(`| ${index + 1} | ${name} | ${version} | published | available, not yanked |`), `missing historical package ${entry}`);
  });
  // Expectations come from the independently retained operations table, not this fixture.
  const assets = operations.split("\n").filter((line) => /^\| `(?:SHA256SUMS|tapid-0\.0\.8-)/.test(line));
  assert(assets.length === 7);
  assert(record.split("\n").filter((line) => line.startsWith("Asset:")).length === 7);
  for (const asset of assets) {
    const columns = asset.split("|").slice(1, -1).map((value) => value.trim().replaceAll("`", ""));
    assert(record.includes(`Asset: ${columns.join(" | ")}`), `missing historical asset ${columns[0]}`);
  }
  for (const [index, name] of ["Unix installer (ubuntu-latest)", "Unix installer (macos-latest)", "Windows installer"].entries()) {
    assert(record.includes(`Job ${index + 1} conclusion: ${name}: success`), `missing smoke conclusion ${name}`);
  }
  assert(record.includes("crates.io workflow run: 33976531200 at e9495179f826b7931560ae284df2f321dde28d7e"));
  assert(record.includes("Changed supporting crates: none during preparation"));
  assert(record.includes("Archive verification result: all six archive hashes matched downloaded SHA256SUMS"));
}

test("complete historical example preserves package, asset and smoke evidence", async () => {
  const operations = await readFile(operationsRecordPath, "utf8");
  validateHistoricalRecord(workedV008Record, operations);
  for (const removed of ["tapid-core | 0.0.4", "545895105", "Unix installer (ubuntu-latest): success", "1231d6f3040ca7fa0a3de9f00808f18f737397c4"]) {
    assertThrows(() => validateHistoricalRecord(workedV008Record.replace(removed, "incorrect"), operations));
  }
});
