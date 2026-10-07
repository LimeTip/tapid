import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFile, readdir } from 'node:fs/promises';
import { createHash } from 'node:crypto';

const source = (path: string) => readFile(new URL(`../../${path}`, import.meta.url), 'utf8');
const pin = 'Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2';
const job = (ci: string, id: string) => {
  const result = ci.match(new RegExp(`^  ${id}:\\n[\\s\\S]*?(?=^  [a-z][a-z-]*:|$(?![\\s\\S]))`, 'm'))?.[0];
  if (result === undefined) throw new Error(`missing ${id}`);
  return result;
};
const step = (owner: string, name: string) => {
  const matches = owner.split(/(?=^      - name: )/m).filter(s => s.startsWith(`      - name: ${name}\n`));
  assert.equal(matches.length, 1, `expected exactly one ${name} step`);
  return matches[0];
};
const digest = (value: string) => createHash('sha256').update(value).digest('hex');
const testCacheInputs = '        with:\n          key: source-install-release-v1\n';
const assertTestBootstrap = (ci: string) => {
  for (const id of ['test', 'nextest', 'coverage', 'security', 'package']) {
    const inputs = id === 'test' ? testCacheInputs.trimEnd() : '';
    assert.equal(step(job(ci, id), 'Cache Rust build artifacts').trimEnd(),
      `      - name: Cache Rust build artifacts\n        uses: ${pin}` + (inputs ? `\n${inputs}` : ''), `${id} cache inputs must remain exactly scoped`);
  }
};

test('Test bootstrap has the stable release namespace and no other cache input changes', async () => {
  assertTestBootstrap(await source('.github/workflows/ci.yml'));
});

test('Test bootstrap rejects omitted, wrong, unstable and overbroad keys', async () => {
  const ci = await source('.github/workflows/ci.yml'); assertTestBootstrap(ci);
  const native = job(ci, 'test'); const cache = step(native, 'Cache Rust build artifacts');
  const mutations: [string, string][] = [
    ['omitted key', cache.replace(testCacheInputs, '')],
    ['wrong namespace', cache.replace('source-install-release-v1', 'source-install-release-v2')],
    ['per-commit key', cache.replace('source-install-release-v1', '${{ github.sha }}')],
    ['per-run key', cache.replace('source-install-release-v1', '${{ github.run_id }}')],
    ['shared key', cache.replace('          key:', '          shared-key:')],
    ['prefix key', cache.replace('          key:', '          prefix-key:')],
    ['job isolation disabled', cache.replace('        with:', "        with:\n          add-job-id-key: 'false'")],
    ['environment isolation disabled', cache.replace('        with:', "        with:\n          add-rust-environment-hash-key: 'false'")],
    ['workspace caching broadened', cache.replace('        with:', "        with:\n          cache-workspace-crates: 'true'")],
    ['restore only', cache.replace('        with:', "        with:\n          lookup-only: 'true'")],
    ['save disabled', cache.replace('        with:', "        with:\n          save-if: 'false'")],
  ];
  for (const [label, changed] of mutations) {
    assert.notEqual(changed, cache, `vacuous mutation: ${label}`);
    assert.throws(() => assertTestBootstrap(ci.replace(native, native.replace(cache, changed))), undefined, label);
  }
  for (const id of ['nextest', 'coverage', 'security', 'package']) {
    const owner = job(ci, id); const other = step(owner, 'Cache Rust build artifacts');
    const changed = owner.replace(other, other + testCacheInputs);
    assert.notEqual(changed, owner, `vacuous mutation: ${id}`);
    assert.throws(() => assertTestBootstrap(ci.replace(owner, changed)), undefined, `${id} must not gain a key`);
  }
});

const cacheName = 'Cache ARM release build dependencies';
const assertArmCache = (ci: string) => {
  const arm = job(ci, 'release-platform-build');
  const cache = step(arm, cacheName);
  assert.equal(cache, `      - name: ${cacheName}\n        uses: ${pin}\n        with:\n` + "          key: release-${{ matrix.target }}\n          cache-bin: 'false'\n");
  assert(arm.indexOf(step(arm, 'Install Rust toolchain and release target')) < arm.indexOf(cache));
  assert(arm.indexOf(cache) < arm.indexOf(step(arm, 'Build release target')));
  // Pre-Stage-6 owner snapshot: native runners, both targets, release/locked build,
  // binary paths and architecture assertions. Update only after reviewing scope.
  assert.equal(digest(arm.replace(cache, '')), '229ab398815e869c840b8f53cc8cce8206f937a6d89252d4223f1e27586d8d9f');
  assertTestBootstrap(ci);
  assert.equal((ci.match(/uses: Swatinem\/rust-cache@/g) || []).length, 6);
};

test('ARM dependency cache retains both real native release builds and consistent reviewed pins', async () => {
  assertArmCache(await source('.github/workflows/ci.yml'));
  for (const file of await readdir(new URL('../../.github/workflows/', import.meta.url))) {
    if (!file.endsWith('.yml')) continue;
    const yaml = await source(`.github/workflows/${file}`);
    for (const action of yaml.matchAll(/uses: (Swatinem\/rust-cache@[^\n]+)/g)) assert.equal(action[1], pin);
  }
});

test('ARM contracts reject cache bypass, matrix, flags, ordering and pin mutations', async () => {
  const ci = await source('.github/workflows/ci.yml');
  assertArmCache(ci);
  const arm = job(ci, 'release-platform-build'); const cache = step(arm, cacheName);
  const mutations: [string, string][] = [
    ['remove cache', arm.replace(cache, '')],
    ['cache before toolchain', arm.replace(cache, '').replace('      - name: Install Rust toolchain and release target', cache + '      - name: Install Rust toolchain and release target')],
    ['cache after build', arm.replace(cache, '').replace('      - name: Verify release binary', cache + '      - name: Verify release binary')],
    ['Linux runner', arm.replace('ubuntu-24.04-arm', 'ubuntu-latest')],
    ['Windows runner', arm.replace('windows-11-vs2026-arm', 'windows-latest')],
    ['Linux target', arm.replace('aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu')],
    ['Windows target', arm.replace('aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc')],
    ['release flag', arm.replace('--release ', '')],
    ['locked flag', arm.replace('--locked ', '')],
    ['target flag', arm.replace(' --target "${{ matrix.target }}"', '')],
    ['binary flag', arm.replace('--bin tapid', '--workspace')],
    ['conditional build', arm.replace('        run: cargo build', "        if: steps.cache.outputs.cache-hit != 'true'\n        run: cargo build")],
    ['key target', arm.replace('key: release-${{ matrix.target }}', 'key: release')],
    ['bin caching', arm.replace("cache-bin: 'false'", "cache-bin: 'true'")],
    ['shared key', arm.replace('          key:', '          shared-key:')],
    ['workspace caching', arm.replace(cache, cache.replace('        with:', "        with:\n          cache-workspace-crates: 'true'"))],
    ['target pollution', arm.replace('    steps:', '    env:\n      CARGO_TARGET_DIR: /shared\n    steps:')],
    ['mutable pin', arm.replace(pin, 'Swatinem/rust-cache@v2')],
  ];
  for (const [name, changed] of mutations) {
    assert.notEqual(changed, arm, `vacuous mutation: ${name}`);
    assert.throws(() => assertArmCache(ci.replace(arm, changed)), undefined, name);
  }
  for (const id of ['test', 'nextest', 'coverage', 'security', 'package']) {
    const owner = job(ci, id);
    const changed = owner.replace(pin, 'Swatinem/rust-cache@49a0bdc70d2e1b713ca9e2869b211fcce03d3c1c # v2');
    assert.notEqual(changed, owner);
    assert.throws(() => assertArmCache(ci.replace(owner, changed)), undefined, id);
  }
});

const installerNames = ['Validate Unix developer installer at the checked head', 'Validate Windows developer installer'];
const compilerEnv = '        env:\n          CARGO_TARGET_DIR: ${{ github.workspace }}/target\n';
const assertInstallerCache = (ci: string) => {
  const native = job(ci, 'test');
  for (const name of installerNames) {
    const install = step(native, name);
    assert(install.includes(compilerEnv), `${name} requires checkout-local compiler env`);
    assert.equal((install.match(/\n        env:/g) || []).length, 1);
  }
  assert.equal((ci.match(/CARGO_TARGET_DIR/g) || []).length, 2, 'compiler target must not leak to other steps, jobs, global env or GITHUB_ENV');
  // Normalize only the approved Test bootstrap input, pin migration and two compiler env blocks.
  // All actual install commands, source-ref, fresh destination, help, uninstall,
  // native tests and documentation ownership remain the pre-Stage-6 snapshot.
  assertTestBootstrap(ci);
  const original = native.replace(testCacheInputs, '').replaceAll(compilerEnv, '').replaceAll(pin, 'Swatinem/rust-cache@49a0bdc70d2e1b713ca9e2869b211fcce03d3c1c # v2');
  assert.equal(digest(original), '8705dbd784cced1b13136ec22278291b4c842231adf22ade3d3c1be5a1dd9ae4');
};
const assertSourceInstallers = (sh: string, ps: string) => {
  const unixStart = sh.indexOf('if [ "$SOURCE_REF_SET" -eq 1 ]; then');
  const windowsStart = ps.indexOf('if (-not [string]::IsNullOrEmpty($SourceRef)) {');
  assert(unixStart >= 0 && windowsStart >= 0);
  // Freeze only the source branches, including clone/detached checkout/fetch,
  // actual locked Cargo install (default release), fresh root, and cleanup.
  assert.equal(digest(sh.slice(unixStart, sh.indexOf('\ncommand -v curl', unixStart))), '983cbaca05be17464227d7cc59e358821bec7ff2f62714b00eef97ce85d2ff61');
  assert.equal(digest(ps.slice(windowsStart, ps.indexOf('\n$legacyRelease =', windowsStart))), '6abd721ba149f4fc8b4d329e60feba702048d63bdbc01b4b3dbb2b67917c36dd');
};

test('source installer compiler reuse is step-only without changing real install or handoff provenance', async () => {
  assertInstallerCache(await source('.github/workflows/ci.yml'));
  assertSourceInstallers(await source('scripts/install.sh'), await source('scripts/install.ps1'));
});

test('installer contracts reject widened env, common-dir reuse, source bypass and omitted uninstall', async () => {
  const ci = await source('.github/workflows/ci.yml'); assertInstallerCache(ci);
  const native = job(ci, 'test');
  const mutations: [string, string][] = [];
  for (const name of installerNames) {
    const install = step(native, name);
    for (const [label, changed] of [
      ['missing env', install.replace(compilerEnv, '')],
      ['shared dev path', install.replace('${{ github.workspace }}/target', '${{ github.workspace }}/../target/dev')],
      ['disabled installer', install.replace('        run:', '        continue-on-error: true\n        run:')],
      ['skip installer', install.replace(/if: runner.os[^\n]+/, 'if: false')],
      ['omit uninstall', install.replace('scripts/uninstall.sh', 'scripts/install.sh').replace('scripts\\uninstall.ps1', 'scripts\\install.ps1')],
      ['omit source ref', install.replace('            --source-ref "$TAPID_SOURCE_REF" \\\n', '').replace(' -SourceRef $env:TAPID_SOURCE_REF', '')],
      ['copy existing binary', install.replace('bash "$GITHUB_WORKSPACE/scripts/install.sh"', 'cp target/release/tapid "$install_dir/tapid" #').replace('& "$env:GITHUB_WORKSPACE\\scripts\\install.ps1"', 'Copy-Item target/release/tapid.exe $installDir #')],
    ] as [string, string][]) {
      assert.notEqual(changed, install, `vacuous mutation: ${name} / ${label}`);
      mutations.push([`${name} / ${label}`, ci.replace(install, changed)]);
    }
  }
  mutations.push(
    ['job env', ci.replace(native, native.replaceAll(compilerEnv, '').replace('    env:', '    env:\n      CARGO_TARGET_DIR: ${{ github.workspace }}/target'))],
    ['global env', ci.replaceAll(compilerEnv, '').replace('\nenv:', '\nenv:\n  CARGO_TARGET_DIR: ${{ github.workspace }}/target')],
    ['GITHUB_ENV persistence', ci.replace('          set -euo pipefail', '          echo CARGO_TARGET_DIR=target >> "$GITHUB_ENV"\n          set -euo pipefail')],
    ['handoff producer env', ci.replace('  platform-consumer-validation:\n', '  platform-consumer-validation:\n    env:\n      CARGO_TARGET_DIR: target\n')],
    ['handoff verifier env', ci.replace('  windows-installer-contract:\n', '  windows-installer-contract:\n    env:\n      CARGO_TARGET_DIR: target\n')],
    ['wrong source identity', ci.replace('github.event.pull_request.head.sha || github.sha', 'github.sha')],
  );
  for (const [label, changed] of mutations) {
    assert.notEqual(changed, ci, `vacuous mutation: ${label}`);
    assert.throws(() => assertInstallerCache(changed), undefined, label);
  }
});

test('source installer contracts reject binary substitution, changed ref, root reuse and lost cleanup', async () => {
  const sh = await source('scripts/install.sh'); const ps = await source('scripts/install.ps1');
  assertSourceInstallers(sh, ps);
  const unixMutations = [
    sh.replace('git clone --filter=blob:none --no-checkout', 'git clone --filter=blob:none'),
    sh.replace('checkout --detach "$SOURCE_REF"', 'checkout --detach main'),
    sh.replace('fetch --filter=blob:none origin "$SOURCE_REF"', 'fetch --filter=blob:none origin main'),
    sh.replace('cargo install --path', 'cp target/release/tapid #'),
    sh.replace('--locked --root "$tmp_dir/root"', '--debug --root "$tmp_dir/root"'),
    sh.replace('--root "$tmp_dir/root"', '--root "$HOME/.cargo"'),
    sh.replace('trap cleanup 0 1 2 15', 'trap : 0 1 2 15'),
  ];
  const windowsMutations = [
    ps.replace('& git clone --filter=blob:none --no-checkout', '& git clone --filter=blob:none'),
    ps.replace('checkout --detach $SourceRef', 'checkout --detach main'),
    ps.replace('fetch --filter=blob:none origin $SourceRef', 'fetch --filter=blob:none origin main'),
    ps.replace('& cargo install --path', 'Copy-Item target/release/tapid.exe #'),
    ps.replace('--locked --root $cargoRoot', '--debug --root $cargoRoot'),
    ps.replace('$cargoRoot = Join-Path $tempRoot "root"', '$cargoRoot = Join-Path $env:USERPROFILE ".cargo"'),
    ps.replace('Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue', 'Write-Host no-cleanup'),
    ps.replace('if ($LASTEXITCODE -ne 0) { Fail "cargo build failed" }', 'Write-Host ignore-cargo-failure'),
  ];
  for (const changed of unixMutations) {
    assert.notEqual(changed, sh); assert.throws(() => assertSourceInstallers(changed, ps));
  }
  for (const changed of windowsMutations) {
    assert.notEqual(changed, ps); assert.throws(() => assertSourceInstallers(sh, changed));
  }
});
