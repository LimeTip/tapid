import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { test } from "node:test";
import { collectCommand, mandatoryControls, validateEvidence } from "../scripts/windows-project-script-acceptance.ts";
import { nodeArgs, temporary, writeJson, sha256 } from "./script-support.ts";

test("empty evidence cannot establish Windows acceptance", () => {
  const result = validateEvidence({}, ".");
  assert.equal(result.complete, false);
  assert.ok(result.errors.includes("unsupported evidence schema"));
});

async function evidence(check: (report: any, directory: string) => void) {
  await temporary((directory) => {
    const dimensions = {
      filesystem_read: true, filesystem_write: true, network: true,
      environment_sanitization: true, descriptor_hygiene: true,
      subprocess_restriction: false, descendant_authority_propagation: true,
      descendant_lifecycle: false, process_tree_membership: false,
      complete_cleanup: false, timeout: false, output: false,
      process_count: false, memory: false,
    };
    const receipt = {
      schema_version: 1, operation: "run", outcome: "completed", error: null,
      receipt: {
        schema_version: 1, assurance: "Restricted", backend: { name: "windows-appcontainer-job" },
        requested: dimensions, declared: dimensions, observed: dimensions, enforced: dimensions,
        effective_filesystem: [], established_evidence: [],
        termination: "Exited(0)", completion: { confirmed: dimensions, cleanup_confidence: "BestEffortObserved" },
      },
    };
    writeJson(join(directory, "receipt.json"), receipt);
    const report = {
      schemaVersion: 1, source: { commit: "a".repeat(40), clean: true },
      platform: { os: "win32", edition: "Windows 11 Pro", build: "26300", architecture: "x64" },
      identities: Object.fromEntries(["cli", "workloadNode", "compatibility", "guard"].map((name) => [name, { sha256: "b".repeat(64), description: name }])),
      developmentNode: "v22.23.3", rust: "rustc fixture", officialNodeProvenance: "official distribution fixture",
      attachments: [{ path: "receipt.json", sha256: sha256(JSON.stringify(receipt)) }],
      commands: ["install", "build", "test", "start"].map((name) => ({ name, argv: ["tapid", name], exit: 0, durationMs: 1, stdout: "receipt.json", stderr: "stderr.log", receipt: "receipt.json" })),
      controls: mandatoryControls.map((id) => ({ id, status: "pass", commit: "a".repeat(40), cliSha256: "b".repeat(64), attachments: ["receipt.json"] })),
      readiness: { body: "TAPID_NEWS_SITE_ACCEPTANCE_V1", durationMs: 10 },
      cleanup: { method: "console-ctrl-c", confirmed: true, durationMs: 5 },
    };
    writeJson(join(directory, "stderr.log"), "stderr");
    report.attachments.push({ path: "stderr.log", sha256: sha256(JSON.stringify("stderr")) });
    check(report, directory);
  });
}

test("complete supplied records pass without treating Restricted as ManagedTree", () => evidence((report, directory) => {
  assert.deepEqual(validateEvidence(report, directory), { complete: true, errors: [] });
}));

for (const [label, mutate] of [
  ["dirty source", (r: any) => { r.source.clean = false; }],
  ["missing provenance", (r: any) => { delete r.officialNodeProvenance; }],
  ["non-Windows source", (r: any) => { r.platform.os = "darwin"; }],
  ["missing component identity", (r: any) => { delete r.identities.compatibility; }],
  ["unavailable control", (r: any) => { r.controls[0].status = "unavailable"; }],
  ["missing control", (r: any) => { r.controls.pop(); }],
  ["duplicate control", (r: any) => { r.controls.push(r.controls[0]); }],
  ["different candidate", (r: any) => { r.controls[0].commit = "c".repeat(40); }],
  ["different executable", (r: any) => { r.controls[0].cliSha256 = "c".repeat(64); }],
  ["failed command", (r: any) => { r.commands[1].exit = 1; }],
  ["missing command", (r: any) => { r.commands.pop(); }],
  ["merged streams", (r: any) => { r.commands[0].stderr = r.commands[0].stdout; }],
  ["missing receipt", (r: any) => { delete r.commands[1].receipt; }],
  ["wrong readiness", (r: any) => { r.readiness.body = "404"; }],
  ["incomplete cleanup", (r: any) => { r.cleanup.confirmed = false; }],
  ["process kill as cancellation", (r: any) => { r.cleanup.method = "taskkill"; }],
  ["tampered attachment", (r: any) => { r.attachments[0].sha256 = "c".repeat(64); }],
] as const) test(`rejects ${label}`, () => evidence((report, directory) => {
  mutate(report);
  assert.equal(validateEvidence(report, directory).complete, false);
}));

for (const [label, mutate] of [
  ["absent authority", (r: any) => { delete r.receipt.enforced; }],
  ["empty authority", (r: any) => { r.receipt.enforced = {}; }],
  ["unenforced request", (r: any) => { r.receipt.enforced.filesystem_read = false; }],
  ["absent backend", (r: any) => { delete r.receipt.backend; }],
  ["unknown assurance", (r: any) => { r.receipt.assurance = "Unknown"; }],
  ["failed termination", (r: any) => { r.receipt.termination = "Exited(1)"; }],
  ["false ManagedTree cleanup", (r: any) => { r.receipt.assurance = "ManagedTree"; }],
] as const) test(`rejects receipt with ${label}`, () => evidence((report, directory) => {
  const record = JSON.parse(requireReceipt(directory));
  mutate(record);
  writeJson(join(directory, "receipt.json"), record);
  report.attachments[0].sha256 = sha256(JSON.stringify(record));
  assert.equal(validateEvidence(report, directory).complete, false);
}));
function requireReceipt(directory: string) { return readFileSync(join(directory, "receipt.json"), "utf8"); }

test("collector preserves argv boundaries, exit and separate streams", () => temporary(async (directory) => {
  const record = await collectCommand({
    executable: process.execPath,
    argv: ["-e", "process.stdout.write(process.argv[1]); process.stderr.write('failure'); process.exitCode=7", "two words"],
    cwd: directory, directory, name: "probe", timeoutMs: 5000,
    env: {},
  });
  assert.equal(record.exit, 7);
  assert.equal(readFileSync(join(directory, record.stdout), "utf8"), "two words");
  assert.equal(readFileSync(join(directory, record.stderr), "utf8"), "failure");
  assert.equal(record.error, null);
  assert.equal(record.timedOut, false);
  assert.ok(record.durationMs >= 0);
}));

test("collector records spawn failures and leaves readable logs", () => temporary(async (directory) => {
  const record = await collectCommand({ executable: join(directory, "missing"), argv: [], cwd: directory, directory, name: "missing", timeoutMs: 500, env: {} });
  assert.equal(record.exit, null);
  assert.match(record.error!, /ENOENT/);
  assert.equal(readFileSync(join(directory, record.stdout)).length, 0);
}));
test("collector timeout is unsuccessful and never claims console cancellation", () => temporary(async (directory) => {
  const record = await collectCommand({ executable: process.execPath, argv: ["-e", "setInterval(()=>{},1000)"], cwd: directory, directory, name: "timeout", timeoutMs: 100, env: {} });
  assert.equal(record.timedOut, true);
  assert.notEqual(record.exit, 0);
}));
test("collector preserves existing evidence", () => temporary(async (directory) => {
  writeJson(join(directory, "probe.stdout.log"), "previous evidence");
  await assert.rejects(collectCommand({ executable: process.execPath, argv: [], cwd: directory, directory, name: "probe", timeoutMs: 100, env: {} }), /EEXIST/);
  assert.equal(readFileSync(join(directory, "probe.stdout.log"), "utf8"), '"previous evidence"');
}));

for (const [label, value] of [["timeout", { timedOut: true }], ["spawn error", { error: "failed" }], ["signal", { signal: "SIGKILL" }]] as const)
  test(`rejects command ${label} despite a supplied zero exit`, () => evidence((report, directory) => {
    Object.assign(report.commands[0], value);
    assert.equal(validateEvidence(report, directory).complete, false);
  }));

test("attachment traversal cannot import evidence outside the dossier", () => temporary(async (directory) => {
  writeJson(join(directory, "outside.json"), "outside");
  await evidence((report, dossier) => {
    report.attachments.push({ path: relative(dossier, join(directory, "outside.json")), sha256: sha256(JSON.stringify("outside")) });
    assert.equal(validateEvidence(report, dossier).complete, false);
  });
}));
test("malformed receipt JSON cannot produce acceptance", () => evidence((report, directory) => {
  writeJson(join(directory, "receipt.json"), null);
  report.attachments[0].sha256 = sha256("null");
  assert.equal(validateEvidence(report, directory).complete, false);
}));

test("actual CLI cancellation exit 130 is accepted only for start with a Cancelled receipt", () => evidence((report, directory) => {
  const record = JSON.parse(requireReceipt(directory));
  record.receipt.termination = "Cancelled";
  writeJson(join(directory, "start.json"), record);
  report.attachments.push({ path: "start.json", sha256: sha256(JSON.stringify(record)) });
  report.commands[3].receipt = "start.json";
  report.commands[3].exit = 130;
  assert.equal(validateEvidence(report, directory).complete, true);
}));

test("checker CLI returns diagnostics and preserves the supplied dossier", () => evidence((report, directory) => {
  report.controls[0].status = "unavailable";
  const path = join(directory, "report.json");
  writeJson(path, report);
  const before = readFileSync(path);
  const result = spawnSync(process.execPath, nodeArgs("scripts/windows-project-script-acceptance.ts", [path]), { encoding: "utf8", timeout: 5000 });
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  assert.match(JSON.parse(result.stdout).errors.join("\n"), /workload.build/);
  assert.deepEqual(readFileSync(path), before);
}));
