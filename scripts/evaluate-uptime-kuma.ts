// Reviewed commands for the first pilot. Call only inside disposable isolation.
import { isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { evaluate } from './evaluate-adoption.ts';
import { readFileSync } from 'node:fs';

type Specification = Parameters<typeof evaluate>[0];
type Options = {
  source: string; root: string; node: string; npmCli: string; tapid: string;
  tapidRevision: string; env: Record<string, string>;
};

// Source inputs are verified before any supplied npm or application code runs.
export function uptimeKumaRun(options: Options): Specification {
  for (const path of [options.source, options.root, options.node, options.npmCli, options.tapid])
    if (!isAbsolute(path)) throw new Error('Pilot paths must be absolute');
  if (!/^[a-f0-9]{40}$/.test(options.tapidRevision)) throw new Error('Full Tapid revision required');
  const source = resolve(options.source), root = resolve(options.root);
  const nested = (a: string, b: string) => {
    const path = relative(a, b);
    return !path || (!path.startsWith('..') && !isAbsolute(path));
  };
  if (nested(root, source) || nested(source, root))
    throw new Error('Retained source and disposable run must be separate');
  const plan = JSON.parse(readFileSync(new URL('../docs/adoption/uptime-kuma/plan.json', import.meta.url), 'utf8'));
  const phases: Specification['phases'] = [];
  const directories = ['npm', 'import', 'native', 'add', 'update'];
  const inputNames = ['package.json', 'package-lock.json', 'LICENSE'];
  const expected = [plan.manifestSha256, plan.lockSha256, plan.licenseSha256];
  const common = { timeoutMs: 20 * 60_000, outputLimit: 128 * 1024 };
  const env = (name: string) => ({ ...options.env, HOME: join(root, name, '.home'),
    USERPROFILE: join(root, name, '.home'), npm_config_cache: join(root, name, '.npm-cache'),
    npm_config_userconfig: join(root, name, '.home', '.npmrc'),
    npm_config_globalconfig: join(root, name, '.home', 'global.npmrc'),
  });
  const node = (id: string, script: string, prerequisites = ['prepare'], outputs?: string[]) => {
    phases.push({ ...common, id, executable: options.node, args: ['-e', script],
      cwd: source, env: env('control'), prerequisites, outputs });
  };
  node('prepare', `
    const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
    const source=${JSON.stringify(source)},root=${JSON.stringify(root)};
    const names=${JSON.stringify(inputNames)},expected=${JSON.stringify(expected)};
    for(let i=0;i<names.length;i++) {
      if(crypto.createHash('sha256').update(fs.readFileSync(path.join(source,names[i]))).digest('hex')!==expected[i])
        throw Error('Pinned input mismatch: '+names[i]);
    }
    if(fs.existsSync(root)) throw Error('Run root must not exist; never overwrite an earlier attempt');
    fs.mkdirSync(root,{recursive:true});
    for(const name of ${JSON.stringify(directories)}) {
      fs.cpSync(source,path.join(root,name),{recursive:true,filter:p=>!['node_modules','.git'].includes(path.basename(p))});
      fs.mkdirSync(path.join(root,name,'.home'),{recursive:true});
    }
    fs.mkdirSync(path.join(root,'control','.home'),{recursive:true});
    console.log('Pinned original inputs retained; independent checkpoints created');
  `, []);
  node('host-identity', `console.log(JSON.stringify({node:process.version,platform:process.platform,arch:process.arch,tapidRevision:${JSON.stringify(options.tapidRevision)}}))`);
  phases.push({ ...common, id: 'binary-identity', executable: options.tapid, args: ['--version'],
    cwd: source, env: env('control'), prerequisites: ['prepare'], inputs: [options.tapid, options.npmCli] });
  phases.push({ ...common, id: 'npm-version', executable: options.node, args: [options.npmCli, '--version'],
    cwd: source, env: env('control'), prerequisites: ['prepare'] });
  const command = (name: string, id: string, executable: string, args: string[], prerequisites: string[]) => {
    const phase = { ...common, id, executable, args, cwd: join(root, name), env: env(name), prerequisites,
      inputs: inputNames.map(file => join(root, name, file)) };
    phases.push(phase);
    return phase;
  };
  const npm = (name: string, id: string, args: string[], prerequisites: string[]) =>
    command(name, id, options.node, [options.npmCli, ...args], prerequisites);
  const tapid = (name: string, id: string, args: string[], prerequisites: string[]) =>
    command(name, id, options.tapid, ['install', 'add', 'update'].includes(args[0])
      ? [...args, '--store-dir', join(root, name, '.store')] : args, prerequisites);
  const checks = (name: string, prefix: string, prerequisite: string) => {
    npm(name, prefix + '-build', ['run', 'build'], [prerequisite]);
    npm(name, prefix + '-test', ['run', 'jest-backend'], [prerequisite]);
    const start = command(name, prefix + '-start', options.node,
      ['server/server.js', '--host=127.0.0.1', '--port=3001', `--data-dir=./data/${prefix}/`],
      [prefix + '-build']);
    Object.assign(start, { timeoutMs: 60_000, readiness: {
      url: 'http://127.0.0.1:3001/', status: 200, bodyIncludes: '<title>Uptime Kuma</title>',
    }, contains: ['Listening on 127.0.0.1:3001'] });
  };
  npm('npm', 'npm-install', ['ci', '--no-fund'], ['prepare', 'npm-version']);
  checks('npm', 'npm', 'npm-install');
  tapid('import', 'status', ['status'], ['prepare', 'binary-identity']);
  tapid('import', 'explain-sqlite', ['explain', '@louislam/sqlite3@15.1.6'], ['prepare', 'binary-identity']);
  tapid('import', 'import-lock', ['import-package-lock', './package-lock.json'], ['prepare', 'binary-identity']);
  tapid('import', 'frozen', ['install', '--frozen'], ['import-lock']);
  tapid('import', 'offline', ['install', '--offline', '--frozen'], ['frozen']);
  checks('import', 'import', 'frozen');
  command('import', 'graph-comparison', options.node,
    ['--experimental-strip-types', fileURLToPath(new URL('./compare-news-site-package-graphs.ts', import.meta.url)),
      '--npm-root', join(root, 'npm'), '--tapid-root', join(root, 'import'),
      '--json', join(root, 'graph.json'), '--text', join(root, 'graph.md')], ['npm-install', 'frozen']);
  tapid('native', 'native-install', ['install'], ['prepare', 'binary-identity']);
  checks('native', 'native', 'native-install');
  for (const name of ['add', 'update']) {
    tapid(name, name + '-import', ['import-package-lock', './package-lock.json'], ['prepare', 'binary-identity']);
    tapid(name, name + '-replay', ['install', '--frozen'], [name + '-import']);
    // Mutations are independent attempts even when imported replay is unsupported.
    if (name === 'update') node('update-declaration', `
      const fs=require('node:fs'),file=${JSON.stringify(join(root, name, 'package.json'))};
      const value=JSON.parse(fs.readFileSync(file,'utf8'));
      if(value.dependencies.dayjs!=='~1.11.5') throw Error('Unexpected Day.js declaration');
      value.dependencies.dayjs='1.11.14';fs.writeFileSync(file,JSON.stringify(value,null,4)+'\\n');
    `, ['prepare'], [join(root, name, 'package.json')]);
    const args = name === 'add' ? ['add', 'is-odd@3.0.1'] : ['update', 'dayjs', '--save-exact'];
    const mutation = tapid(name, name + '-mutation', args, ['prepare', 'binary-identity', ...(name === 'update' ? ['update-declaration'] : [])]);
    Object.assign(mutation, { outputs: [join(root, name, 'package.json'), join(root, name, 'package-lock.json')],
      manualSteps: name === 'update' ? ['Deliberately constrain dependencies.dayjs to 1.11.14 before named update; not unchanged-manifest update evidence'] : [] });
    checks(name, name, name + '-mutation');
    // Restoration must run after a failed mutation as well. Never erase a pending journal.
    node(name + '-restore', `
      const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
      const dir=${JSON.stringify(join(root, name))},source=${JSON.stringify(source)};
      if(fs.readdirSync(dir).some(n=>n.startsWith('.tapid-') && n.includes('journal')))
        throw Error('Pending transaction: recover through Tapid before rollback');
      for(const name of ['node_modules','tapid.lock']) fs.rmSync(path.join(dir,name),{recursive:true,force:true});
      const names=${JSON.stringify(inputNames)},expected=${JSON.stringify(expected)};
      for(let i=0;i<names.length;i++) {
        fs.copyFileSync(path.join(source,names[i]),path.join(dir,names[i]));
        if(crypto.createHash('sha256').update(fs.readFileSync(path.join(dir,names[i]))).digest('hex')!==expected[i])
          throw Error('Rollback input mismatch');
      }
      console.log('Original npm input hashes restored');
    `, ['prepare'], inputNames.map(file => join(root, name, file)));
    npm(name, name + '-rollback-install', ['ci', '--no-fund'], [name + '-restore', 'npm-version']);
    checks(name, name + '-rollback', name + '-rollback-install');
  }
  phases.push({ ...common, id: 'approved-lifecycle', executable: options.tapid, args: [],
    cwd: source, env: env('control'), prerequisites: ['prepare'],
    unsupported: 'No qualified Linux ManagedTree host or exact authority policy supplied; no hook grant is inferred' });
  if (process.platform === 'win32') phases[0].unsupported =
    'Root-only process cleanup does not support unattended external Windows evaluation';
  return { version: 1, id: `uptime-kuma-${plan.revision}-${options.tapidRevision}`, project: plan.repository,
    destination: join(root + '-evidence'), secrets: [root, source], phases };
}
