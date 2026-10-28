import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { evaluate } from '../scripts/evaluate-adoption.ts';

test('captures a command without inheriting credentials and continues independent phases', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'adoption-'));
  try {
    const result = await evaluate({ version: 1, id: 'local', project: 'synthetic', destination: dir,
      phases: [
        { id: 'failure', executable: process.execPath, args: ['-e', 'process.exit(3)'], cwd: dir, env: {}, timeoutMs: 2000, outputLimit: 1024 },
        { id: 'dependent', prerequisites: ['failure'], executable: process.execPath, args: [], cwd: dir, env: {}, timeoutMs: 2000, outputLimit: 1024 },
        { id: 'independent', executable: process.execPath, args: ['-e', "console.log(process.env.PATH ?? 'absent')"], cwd: dir, env: {}, timeoutMs: 2000, outputLimit: 1024, contains: ['absent'] },
      ] });
    assert.deepEqual(result.phases.map(p => p.outcome), ['failed', 'not-reached', 'passed']);
    assert.equal(result.launches, 2);
    assert.equal(JSON.parse(readFileSync(join(dir, 'receipts.json'), 'utf8')).launches, 2);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('records termination, launch failures, bounded redaction and immutable input identity', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'adoption-'));
  const base = { executable: process.execPath, cwd: dir, env: {}, timeoutMs: 1000, outputLimit: 1024 };
  try {
    const result = await evaluate({ version: 1, id: 'edges', project: 'synthetic', destination: dir, secrets: ['private-value'], phases: [
      { ...base, id: 'redaction', args: ['-e', "process.stdout.write('private-'); setTimeout(()=>console.log('value\\x1b[31m'),20)"], inputs: [fileURLToPath(import.meta.url)], outputs: [fileURLToPath(import.meta.url)] },
      { ...base, id: 'assertion', args: ['-e', 'console.log("actual")'], contains: ['missing'] },
      { ...base, id: 'launch', executable: join(dir, 'missing'), args: [] },
      { ...base, id: 'timeout', args: ['-e', 'setInterval(()=>{},1000)'], timeoutMs: 100 },
      { ...base, id: 'overflow', args: ['-e', 'process.stdout.write("x".repeat(100000))'] },
      { ...base, id: 'signal', args: ['-e', 'process.kill(process.pid,"SIGTERM")'] },
      { ...base, id: 'unsupported', args: [], unsupported: 'No qualified host' },
    ] });
    const [redaction, assertion, launch, timeout, overflow, signal, unsupported] = result.phases;
    assert.equal(redaction.outcome, 'passed');
    assert.match(redaction.output, /\[REDACTED\]\\u001b/);
    assert.deepEqual(redaction.inputs, redaction.outputs);
    assert.equal(Object.values(redaction.inputs)[0].length, 64);
    assert.equal(assertion.reason, 'assertion');
    assert.match(launch.reason!, /launch:/);
    assert.equal(timeout.reason, 'timeout');
    assert.equal(overflow.reason, 'output-limit');
    assert.equal(overflow.truncated, true);
    assert.ok(overflow.output.length <= 1024);
    assert.equal(signal.signal, 'SIGTERM');
    assert.equal(unsupported.outcome, 'unsupported');
    assert.equal(result.launches, 6);
    assert.ok(!readFileSync(join(dir, 'receipts.json'), 'utf8').includes('private-value'));
  } finally { rmSync(dir, {recursive:true, force:true}); }
});

test('deadline stops a process-group descendant and closes its inherited pipes', { skip: process.platform === 'win32' }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'adoption-'));
  try {
    const result = await evaluate({ version: 1, id: 'descendant', project: 'synthetic', destination: dir, phases: [{
      id: 'tree', executable: process.execPath,
      args: ['-e', `require('node:child_process').spawn(process.execPath,['-e',"setInterval(()=>require('node:fs').appendFileSync('heartbeat','x'),20)"],{stdio:'inherit'}); process.exit(0)`],
      cwd: dir, env: {}, timeoutMs: 500, outputLimit: 1024,
    }] });
    assert.equal(result.phases[0].reason, 'timeout');
    assert.ok(result.phases[0].elapsedMs < 3000);
    const stopped = readFileSync(join(dir,'heartbeat'),'utf8');
    assert.ok(stopped.length > 0);
    await delay(100);
    assert.equal(readFileSync(join(dir,'heartbeat'),'utf8'), stopped);
  } finally { rmSync(dir,{recursive:true,force:true}); }
});

test('redaction cannot turn a missing assertion into a pass', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'adoption-'));
  try {
    const result = await evaluate({version:1,id:'assertions',project:'synthetic',destination:dir,secrets:['secret-one','secret-two'],phases:[{
      id:'check',executable:process.execPath,args:['-e','console.log("secret-one")'],cwd:dir,env:{},timeoutMs:1000,outputLimit:1024,contains:['secret-two']
    }]});
    assert.equal(result.phases[0].outcome,'failed');
  } finally { rmSync(dir,{recursive:true,force:true}); }
});

test('rejects omitted environments before a command can inherit credentials', async () => {
  const dir = mkdtempSync(join(tmpdir(),'adoption-'));
  try {
    await assert.rejects(evaluate({version:1,id:'invalid',project:'synthetic',destination:dir,phases:[{
      id:'bad',executable:process.execPath,args:['-e','process.exit(0)'],cwd:dir,timeoutMs:1000,outputLimit:1024,
    } as any]}), /explicit environment/);
  } finally {rmSync(dir,{recursive:true,force:true});}
});
