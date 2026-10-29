import assert from 'node:assert/strict';
import { test } from 'node:test';
import { PLATFORMS, TARGETS } from './platforms.ts';
import { archiveNames } from './release.ts';

test('release platforms preserve the independent six-platform contract and order', () => {
  assert.deepEqual(PLATFORMS, [
    {target:'aarch64-apple-darwin', binary:'tapid', runner:'macos-14'},
    {target:'aarch64-pc-windows-msvc', binary:'tapid.exe', runner:'windows-11-vs2026-arm'},
    {target:'aarch64-unknown-linux-gnu', binary:'tapid', runner:'ubuntu-24.04-arm'},
    {target:'x86_64-apple-darwin', binary:'tapid', runner:'macos-15-intel'},
    {target:'x86_64-pc-windows-msvc', binary:'tapid.exe', runner:'windows-latest'},
    {target:'x86_64-unknown-linux-gnu', binary:'tapid', runner:'ubuntu-latest'},
  ]);
  assert.equal(new Set(TARGETS).size, 6);
  assert.equal(archiveNames('1.2.3')[0], 'tapid-1.2.3-aarch64-apple-darwin.tar.gz');
  assert.throws(() => archiveNames('01.2.3'), /release tag/);
});

test('explicit installer selections agree with manifest while preserving aliases and rejection', async () => {
  const { readFile } = await import('node:fs/promises');
  const { execFileSync } = await import('node:child_process');
  const shell = await readFile(new URL('../../scripts/install.sh', import.meta.url), 'utf8');
  const selection = /case "\$\(uname -s\):\$\(uname -m\)" in\n[\s\S]*?\nesac/.exec(shell)![0];
  const select = (os: string, arch: string) => execFileSync('sh', ['-c',
    'fixture_os=$1; fixture_arch=$2; uname() { case "$1" in -s) printf "%s" "$fixture_os" ;; -m) printf "%s" "$fixture_arch" ;; esac; }; fail() { echo "$*" >&2; exit 1; };\n'+selection+'\nprintf "%s" "$target"',
    'fixture', os, arch], {encoding:'utf8', stdio:['ignore','pipe','pipe']});
  const cases = [
    ['Darwin','arm64','aarch64-apple-darwin'], ['Darwin','aarch64','aarch64-apple-darwin'],
    ['Darwin','x86_64','x86_64-apple-darwin'], ['Linux','x86_64','x86_64-unknown-linux-gnu'],
    ['Linux','amd64','x86_64-unknown-linux-gnu'], ['Linux','arm64','aarch64-unknown-linux-gnu'],
    ['Linux','aarch64','aarch64-unknown-linux-gnu'],
  ];
  if (process.platform !== 'win32') {
    for (const [os,arch,target] of cases) assert.equal(select(os,arch),target);
  }
  assert.deepEqual([...new Set(cases.map(row=>row[2]))].sort(), PLATFORMS.filter(p=>p.binary==='tapid').map(p=>p.target).sort());
  const declared = [...selection.matchAll(/target="([^"]+)"/g)].map(match=>match[1]).sort();
  assert.deepEqual(declared, PLATFORMS.filter(p=>p.binary==='tapid').map(p=>p.target).sort());
  if (process.platform !== 'win32') {
    for (const [os,arch] of [['FreeBSD','x86_64'],['Linux','i686'],['Darwin','amd64'],['Windows','arm64']]) {
      assert.throws(()=>select(os,arch), /unsupported platform/);
    }
  }
  const powershell = await readFile(new URL('../../scripts/install.ps1', import.meta.url), 'utf8');
  const windows = /\$target = switch \(\$architecture\) \{\n([\s\S]*?)\n\}/.exec(powershell)![1];
  assert.deepEqual(windows.trim().split('\n').map(line=>line.trim()), [
    '"X64" { "x86_64-pc-windows-msvc" }',
    '"Arm64" { "aarch64-pc-windows-msvc" }',
    'default { Fail "unsupported Windows architecture: $architecture" }',
  ]);
  assert.deepEqual([...windows.matchAll(/"([^"]+-pc-windows-msvc)"/g)].map(m=>m[1]).sort(), PLATFORMS.filter(p=>p.binary==='tapid.exe').map(p=>p.target).sort());
});

test('read-only expectation commands emit exact names offline and reject invalid arguments', async () => {
  const { execFileSync } = await import('node:child_process');
  const { fileURLToPath } = await import('node:url');
  const run = (file: string, ...args: string[]) => execFileSync(process.execPath,
    ['--experimental-strip-types',fileURLToPath(new URL(file,import.meta.url)),...args],
    {encoding:'utf8',env:{...process.env,GH_TOKEN:'',GITHUB_TOKEN:''},stdio:['ignore','pipe','pipe']});
  const archives=run('./release.ts','archives','1.2.3').trim().split('\n');
  assert.deepEqual(archives, archiveNames('1.2.3').sort());
  const assets=run('./candidate.ts','expected-assets','v1.2.3').trim().split('\n');
  assert.deepEqual(assets,[...archives,'SHA256SUMS','install.ps1','install.sh','tapid-release-v1.tsv','tapid-release-v1.tsv.sig'].sort());
  for (const args of [['archives'],['archives','v1.2.3'],['archives','1.2.3','extra']]) assert.throws(()=>run('./release.ts',...args));
  for (const args of [['expected-assets'],['expected-assets','1.2.3'],['expected-assets','v1.2.3','extra']]) assert.throws(()=>run('./candidate.ts',...args));
});
