import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { evaluate } from '../scripts/evaluate-adoption.ts';

test('pilot rollback preserves the checkpoint when retained inputs changed', async () => {
  const { uptimeKumaRun } = await import('../scripts/evaluate-uptime-kuma.ts');
  const { mkdirSync, writeFileSync } = await import('node:fs');
  const dir = mkdtempSync(join(tmpdir(), 'adoption-restore-'));
  const source = join(dir, 'source'), root = join(dir, 'run');
  mkdirSync(source);
  mkdirSync(join(root, 'add', 'node_modules'), { recursive: true });
  for (const name of ['package.json', 'package-lock.json', 'LICENSE']) {
    writeFileSync(join(source, name), 'changed retained input');
    writeFileSync(join(root, 'add', name), 'checkpoint input');
  }
  writeFileSync(join(root, 'add', 'node_modules', 'checkpoint'), 'installed checkpoint');
  writeFileSync(join(root, 'add', 'tapid.lock'), 'checkpoint lock');
  try {
    const spec = uptimeKumaRun({ source, root, node: process.execPath,
      npmCli: join(dir, 'unused-npm.js'), tapid: process.execPath,
      tapidRevision: 'a'.repeat(40), env: {} });
    // Execute only the reviewed recovery command, modeling retained-input damage
    // after preparation. No npm, Tapid or application command runs in this test.
    const restore = spec.phases.find(p => p.id === 'add-restore')!;
    const result = await evaluate({ ...spec, phases: [{ ...restore, prerequisites: [] }] });
    assert.equal(result.phases[0].outcome, 'failed');
    assert.match(result.phases[0].output, /Rollback input mismatch/);
    assert.equal(readFileSync(join(root, 'add', 'node_modules', 'checkpoint'), 'utf8'), 'installed checkpoint');
    assert.equal(readFileSync(join(root, 'add', 'tapid.lock'), 'utf8'), 'checkpoint lock');
    for (const name of ['package.json', 'package-lock.json', 'LICENSE'])
      assert.equal(readFileSync(join(root, 'add', name), 'utf8'), 'checkpoint input');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

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

test('bounded loopback readiness checks HTTP content and stops the service', async () => {
  const { createServer } = await import('node:net');
  const reservation = createServer();
  await new Promise<void>((resolve, reject) => {
    reservation.once('error', reject);
    reservation.listen(0, '127.0.0.1', resolve);
  });
  const port = (reservation.address() as import('node:net').AddressInfo).port;
  await new Promise<void>(resolve => reservation.close(() => resolve()));
  const dir = mkdtempSync(join(tmpdir(), 'adoption-'));
  try {
    const result = await evaluate({version:1,id:'readiness',project:'synthetic',destination:dir,phases:[{
      id:'start',executable:process.execPath,
      args:['-e',`require('node:http').createServer((req,res)=>res.end('ready application')).listen(${port},'127.0.0.1')`],
      cwd:dir,env:{},timeoutMs:3000,outputLimit:1024,
      readiness:{url:`http://127.0.0.1:${port}/`,status:200,bodyIncludes:'ready application'},
    }]});
    assert.equal(result.phases[0].outcome,'passed');
    assert.equal(result.phases[0].reason,'readiness-satisfied');
    assert.deepEqual(result.phases[0].assertions,[true]);
    await assert.rejects(fetch(`http://127.0.0.1:${port}/`));
  } finally {rmSync(dir,{recursive:true,force:true});}
});

test('readiness failure stays failed and non-loopback probes reject before launch', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'adoption-'));
  const phase = {id:'start',executable:process.execPath,args:['-e','process.exit(0)'],cwd:dir,env:{},timeoutMs:1000,outputLimit:1024,
    readiness:{url:'http://127.0.0.1:1/',status:200,bodyIncludes:'ready'}};
  try {
    const result = await evaluate({version:1,id:'missing',project:'synthetic',destination:dir,phases:[phase]});
    assert.equal(result.phases[0].outcome,'failed');
    assert.deepEqual(result.phases[0].assertions,[false]);
    await assert.rejects(evaluate({version:1,id:'external',project:'synthetic',destination:dir,phases:[{
      ...phase,readiness:{...phase.readiness,url:'http://example.com/'},
    }]}), /literal HTTP loopback/);
  } finally {rmSync(dir,{recursive:true,force:true});}
});

test('pilot command specification rejects changed inputs before invoking npm or application code', { skip: process.platform === 'win32' }, async () => {
  const { uptimeKumaRun } = await import('../scripts/evaluate-uptime-kuma.ts');
  const { mkdirSync, writeFileSync } = await import('node:fs');
  const dir = mkdtempSync(join(tmpdir(), 'adoption-pilot-'));
  const source = join(dir, 'source');
  mkdirSync(source);
  writeFileSync(join(source, 'package.json'), '{}');
  try {
    const spec = uptimeKumaRun({ source, root:join(dir,'run'), node:process.execPath,
      npmCli:join(dir,'never-run-npm.js'), tapid:process.execPath,
      tapidRevision:'a'.repeat(40), env:{} });
    const result = await evaluate(spec);
    assert.equal(result.launches,1);
    assert.equal(result.phases[0].outcome,'failed');
    assert.match(result.phases[0].output,/Pinned input mismatch/);
    assert.ok(result.phases.slice(1).every(p=>p.outcome==='not-reached'));
    assert.ok(result.phases.some(p=>p.id==='add-restore'));
    assert.ok(result.phases.some(p=>p.id==='update-rollback-start'));
  } finally { rmSync(dir,{recursive:true,force:true}); }
});
