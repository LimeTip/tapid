import assert from "node:assert/strict";
import { test } from "node:test";
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { temporary } from "./script-support.ts";
import { runManagedNewsSite } from "../scripts/check-managed-news-site.ts";

async function fixture(path: string, mode: string) {
  const source = join(path, "fixture");
  mkdirSync(source);
  writeFileSync(join(source, "package.json"), JSON.stringify({ mode }));
  writeFileSync(join(source, "tapid.toml"), "restricted original");
  writeFileSync(join(source, "tapid.linux-managed.toml"), "managed acceptance policy");
  const binary = join(path, "fake-tapid");
  writeFileSync(binary, `#!${process.execPath}
const fs = require('node:fs'), http = require('node:http');
const args = process.argv.slice(2), project = args[args.indexOf('--project-dir') + 1];
const mode = JSON.parse(fs.readFileSync(project + '/package.json')).mode;
fs.appendFileSync(project + '/calls', JSON.stringify(args) + '\\n');
if (args[0] === 'install') process.exit(0);
if (mode === 'build-failure' && args[1] === 'build') process.exit(37);
const result = args[args.indexOf('--result-file') + 1];
function receipt(termination) {
 fs.writeFileSync(result, JSON.stringify({outcome:'completed', receipt:{assurance:'ManagedTree', termination, requested:{filesystem_read:true}, enforced:{filesystem_read:true}, completion:{cleanup_confidence:mode === 'bad-cleanup' ? 'NoCleanupGuarantee' : 'KernelOwnedComplete'}}}));
}
if (args[1] !== 'start') { receipt('Exited(0)'); process.exit(0); }
const server = http.createServer((req,res) => res.end(mode === 'not-ready' ? 'wrong marker' : 'TAPID_NEWS_SITE_ACCEPTANCE_V1'));
server.listen(Number(args[args.indexOf('--port')+1]), '127.0.0.1');
process.on('SIGINT', () => { if (mode === 'ignore-cancel') return; server.close(() => { receipt('Cancelled'); process.exit(130); }); });
`);
  chmodSync(binary, 0o755);
  return { source, binary, node: process.execPath, evidence: join(path, "evidence"), readinessMs: 1500, shutdownMs: 2000 };
}

test("ManagedTree fixture runs build/test/start with explicit runtime, policy, arguments and cancellation receipts", () => temporary(async path => {
  const options = await fixture(path, "success");
  const evidence = await runManagedNewsSite(options);
  const calls = readFileSync(join(evidence.project, "calls"), "utf8").trim().split('\n').map(line => JSON.parse(line));
  assert.deepEqual(calls.map(args => args.slice(0,2)), [['install','--project-dir'], ['run','build'], ['run','test'], ['run','start']]);
  for (const args of calls.slice(1)) {
    assert.equal(args[args.indexOf('--node-runtime')+1], process.execPath);
    assert.ok(args.includes('--result-file'));
  }
  assert.ok(calls[1].includes('--allow-process-memory-stats'));
  assert.ok(!calls[2].includes('--allow-process-memory-stats'));
  assert.deepEqual(calls[3].slice(calls[3].indexOf('--')+1, -2), ['--hostname','127.0.0.1']);
  assert.equal(readFileSync(join(evidence.project, 'tapid.toml'), 'utf8'), 'managed acceptance policy');
  assert.equal(readFileSync(join(options.source, 'tapid.toml'), 'utf8'), 'restricted original');
  assert.equal(evidence.start.receipt.termination, 'Cancelled');
}));

test("fixture build failure propagates without running later scripts", () => temporary(async path => {
  const options = await fixture(path, "build-failure");
  await assert.rejects(runManagedNewsSite(options), /build.*37/);
  const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
  const calls = readFileSync(join(report.project, 'calls'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
  assert.equal(calls.length, 2);
  assert.equal(report.status, 'failed');
}));

test("readiness timeout still cancels and checks cleanup", () => temporary(async path => {
  const options = await fixture(path, "not-ready");
  await assert.rejects(runManagedNewsSite(options), /readiness/);
  const result = JSON.parse(readFileSync(join(options.evidence, 'start.json'), 'utf8'));
  assert.equal(result.receipt.termination, 'Cancelled');
}));


test("a successful script with insufficient cleanup evidence fails acceptance", () => temporary(async path => {
  const options = await fixture(path, "bad-cleanup");
  await assert.rejects(runManagedNewsSite(options), /KernelOwnedComplete/);
  const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
  assert.equal(report.status, 'failed');
  assert.equal(report.commands.length, 2);
}));

test("forced caller termination cannot count as successful cancellation", () => temporary(async path => {
  const options = await fixture(path, "ignore-cancel");
  options.shutdownMs = 500;
  await assert.rejects(runManagedNewsSite(options), /ManagedTree cancellation timed out/);
  const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
  assert.equal(report.status, 'failed');
  assert.equal(report.start, undefined);
  const args = report.commands.at(-1).args;
  const port = args[args.indexOf('--port') + 1];
  await assert.rejects(fetch('http://127.0.0.1:' + port + '/acceptance', {signal: AbortSignal.timeout(1000)}));
}));
