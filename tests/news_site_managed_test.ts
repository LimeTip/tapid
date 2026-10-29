import assert from "node:assert/strict";
import { test } from "node:test";
import { chmodSync, existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
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
if (args[0] === 'install') {
 fs.mkdirSync(project + '/node_modules/.bin', {recursive:true});
 fs.mkdirSync(project + '/node_modules/next', {recursive:true});
 fs.writeFileSync(project + '/node_modules/next/cli.js', '#!/usr/bin/env node\\n', {mode:0o755});
 if (mode.startsWith('outside-bin')) {
  fs.writeFileSync(project + '/../outside-tool', 'synthetic outside data');
  fs.symlinkSync('../../../outside-tool', project + '/node_modules/.bin/next');
 } else fs.symlinkSync('../next/cli.js', project + '/node_modules/.bin/next');
 process.exit(0);
}
const diagnostic = fs.existsSync(project + '/.tmp/tool-diagnostic.cjs');
if (diagnostic) fs.writeFileSync(project + '/.tmp/tool-diagnostic.json', JSON.stringify({runtime:process.execPath, paths:[{error:'ENOENT'}], attempts:[{error:'EACCES'}]}));
if (diagnostic && mode === 'diagnostic-large') fs.writeFileSync(project + '/.tmp/tool-diagnostic.json', 'x'.repeat(20000));
if (diagnostic && mode === 'diagnostic-unavailable') fs.unlinkSync(project + '/.tmp/tool-diagnostic.json');
if (diagnostic && mode === 'diagnostic-real') {
 const result = require('node:child_process').spawnSync(process.execPath, [project + '/.tmp/tool-diagnostic.cjs'], {cwd:project, env:{PATH:require('node:path').dirname(process.execPath)}});
 if (result.status !== 0) process.exit(45);
}
if (!diagnostic && ['build-failure', 'outside-bin', 'diagnostic-large', 'diagnostic-unavailable', 'diagnostic-real'].includes(mode) && args[1] === 'build') process.exit(37);
const result = args[args.indexOf('--result-file') + 1];
function receipt(termination) {
 fs.writeFileSync(result, JSON.stringify({outcome:'completed', receipt:{assurance:'ManagedTree', termination, requested:{filesystem_read:true}, enforced:{filesystem_read:true}, completion:{cleanup_confidence:mode === 'bad-cleanup' ? 'NoCleanupGuarantee' : 'KernelOwnedComplete'}}}));
}
if (args[1] !== 'start') { receipt('Exited(0)'); process.exit(0); }
if (mode === 'early-exit') process.exit(42);
const server = http.createServer((req,res) => res.end(mode.startsWith('not-ready') ? 'wrong marker' : 'TAPID_NEWS_SITE_ACCEPTANCE_V1'));
server.listen(Number(args[args.indexOf('--port')+1]), '127.0.0.1');
process.on('SIGINT', () => { fs.writeFileSync(project + '/shutdown-attempted', 'yes'); if (mode === 'ignore-cancel') return; server.close(() => { receipt('Cancelled'); process.exit(mode === 'not-ready-bad-cancel' ? 43 : 130); }); });
`);
  chmodSync(binary, 0o755);
  return { source, binary, node: process.execPath, nodeMajor: Number(process.versions.node.split('.')[0]), evidence: join(path, "evidence"), readinessMs: 1500, shutdownMs: 2000 };
}

test("runtime mismatch rejects before invoking Tapid", () => temporary(async path => {
  const options = await fixture(path, "success");
  options.nodeMajor += 1;
  await assert.rejects(runManagedNewsSite(options), /acceptance requires Node/);
  assert.equal(existsSync(join(options.evidence, 'project/calls')), false);
}));

test("native runtime default remains Node 22", () => temporary(async path => {
  const { nodeMajor, ...options } = await fixture(path, "build-failure");
  await assert.rejects(runManagedNewsSite(options), nodeMajor === 22 ? /build.*37/ : /acceptance requires Node 22/);
  assert.equal(existsSync(join(options.evidence, 'project/calls')), nodeMajor === 22);
}));

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
  assert.equal(calls.length, 3);
  assert.equal(report.status, 'failed');
  assert.equal(report.local_tool.link, '../next/cli.js');
  assert.equal(report.local_tool.target, realpathSync(join(report.project, 'node_modules/next/cli.js')));
  assert.equal(report.local_tool.mode & 0o100, 0o100);
  assert.equal(report.local_tool.interpreter, '/usr/bin/env node');
  assert.equal(report.local_tool.scope, 'host-before-containment');
}));

test("tool diagnostics do not read a target outside the project or mask build failure", () => temporary(async path => {
  const options = await fixture(path, "outside-bin");
  await assert.rejects(runManagedNewsSite(options), /build.*37/);
  const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
  assert.match(report.local_tool.error, /target escapes project/);
  assert.equal(report.local_tool.interpreter, undefined);
  assert.equal(report.local_tool.mode, undefined);
  assert.match(report.error, /build.*37/);
  assert.equal(JSON.stringify(report).includes('synthetic outside data'), false);
}));

test("incomplete tool inspection rejects acceptance after successful scripts and cleanup", () => temporary(async path => {
  const options = await fixture(path, "outside-bin-success");
  await assert.rejects(runManagedNewsSite(options), /installed tool inspection incomplete.*target escapes project/);
  const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
  assert.equal(report.status, 'failed');
  assert.match(report.local_tool.error, /target escapes project/);
  assert.match(report.error, /installed tool inspection incomplete/);
  assert.equal(report.commands.length, 4);
  assert.equal(report.cleanup_attempted, true);
  assert.equal(report.start.receipt.termination, 'Cancelled');
  assert.equal(report.start.receipt.completion.cleanup_confidence, 'KernelOwnedComplete');
  assert.equal(readFileSync(join(report.project, 'shutdown-attempted'), 'utf8'), 'yes');
}));

test("readiness timeout still cancels and checks cleanup", () => temporary(async path => {
  const options = await fixture(path, "not-ready");
  await assert.rejects(runManagedNewsSite(options), /readiness/);
  const result = JSON.parse(readFileSync(join(options.evidence, 'start.json'), 'utf8'));
  assert.equal(result.receipt.termination, 'Cancelled');
}));

for (const mode of ['early-exit', 'not-ready-bad-cancel']) {
  test(`${mode} preserves readiness error alongside cleanup diagnostics`, () => temporary(async path => {
    const options = await fixture(path, mode);
    const expected = mode === 'early-exit' ? /start exited before readiness.*42/ : /HTTP readiness timed out/;
    await assert.rejects(runManagedNewsSite(options), expected);
    const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
    assert.match(report.error, expected);
    assert.match(report.cleanup_error, /start cancellation exited/);
    assert.equal(report.cleanup_attempted, true);
    if (mode !== 'early-exit') assert.equal(readFileSync(join(report.project, 'shutdown-attempted'), 'utf8'), 'yes');
  }));
}


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

test("failed build retains contained diagnostics without accepting diagnostic success", () => temporary(async path => {
  const options = await fixture(path, "build-failure");
  await assert.rejects(runManagedNewsSite(options), /build.*37/);
  const report = JSON.parse(readFileSync(join(options.evidence, "summary.json"), "utf8"));
  assert.equal(report.status, "failed");
  assert.equal(report.diagnostic.receipt.receipt.termination, "Exited(0)");
  assert.equal(report.diagnostic.observations.runtime, process.execPath);
  assert.equal(report.diagnostic.observations.attempts[0].error, "EACCES");
  assert.equal(report.diagnostic.observations.paths[0].error, "ENOENT");
  const build = report.commands.find(command => command.name === 'build');
  const diagnostic = report.commands.find(command => command.name === 'diagnostic');
  const expected = [...build.args];
  expected[expected.indexOf('--result-file') + 1] = join(options.evidence, 'diagnostic.json');
  assert.deepEqual(diagnostic.args, expected);
  assert.equal(readFileSync(join(report.project, "package.json"), "utf8"), JSON.stringify({mode:"build-failure"}));
}));

for (const mode of ['diagnostic-large', 'diagnostic-unavailable']) {
  test(`${mode} preserves primary failure with bounded diagnostic error`, () => temporary(async path => {
    const options = await fixture(path, mode);
    await assert.rejects(runManagedNewsSite(options), /build.*37/);
    const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
    assert.equal(report.status, 'failed');
    assert.equal(report.diagnostic.observations, undefined);
    assert.match(report.diagnostic.error, mode === 'diagnostic-large' ? /bound/ : /ENOENT/);
  }));
}

test('generated diagnostic observes execution errors without retaining child output', () => temporary(async path => {
  const options = await fixture(path, 'diagnostic-real');
  await assert.rejects(runManagedNewsSite(options), /build.*37/);
  const report = JSON.parse(readFileSync(join(options.evidence, 'summary.json'), 'utf8'));
  assert.equal(report.diagnostic.error, undefined);
  const observations = report.diagnostic.observations;
  assert.equal(observations.runtime, process.execPath);
  assert.equal(observations.attempts.length, 2);
  assert.equal(observations.attempts[0].error, 'ENOENT');
  assert.equal(observations.attempts[1].status, 0);
  assert.ok(observations.paths.some(entry => entry.path.endsWith('/next/cli.js')));
  assert.equal(observations.attempts[1].stdout, undefined);
}));
