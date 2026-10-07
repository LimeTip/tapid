import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFile, readdir, mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const execFileAsync = promisify(execFile);
const workflow = (name: string) => readFile(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
const source = (path: string) => readFile(new URL(`../../${path}`, import.meta.url), 'utf8');
const job = (yaml: string, name: string) => {
  const section = yaml.match(new RegExp(`^  ${name}:\\n[\\s\\S]*?(?=^  [a-z][a-z-]*:|$(?![\\s\\S]))`, 'm'))?.[0];
  assert(section, `missing ${name} job`);
  return section;
};

const step = (section: string, name: string) => {
  const sections = section.split(/(?=^      - name: )/m);
  const matches = sections.filter(section => section.startsWith(`      - name: ${name}\n`));
  assert.equal(matches.length, 1, `expected exactly one ${name} step`);
  return matches[0];
};

const assertSourceOnlyOwners = (ci: string) => {
  assert.match(ci, /push:\n    branches: \[main\]\n  pull_request:\n/);
  const native = job(ci, 'test');
  const header = native.slice(0, native.indexOf('    steps:'));
  assert.match(header, /runs-on: \$\{\{ matrix.os \}\}/);
  assert.match(header, /os: \[ubuntu-latest, macos-latest, windows-latest\]/);
  assert.doesNotMatch(header, /if:|continue-on-error:|exclude:|include:|defaults:/);
  for (const [name, command] of [
    ['Check formatting', 'cargo fmt --all --check'],
    ['Verify pinned node-semver compatibility oracle', 'npm ci --ignore-scripts --no-audit --no-fund\n          npm test --offline'],
  ]) {
    const owner = step(native, name);
    assert.match(owner, /\n        if: matrix.os == 'ubuntu-latest'\n/);
    assert.doesNotMatch(owner, /continue-on-error:|\|\|\s*true|allow-failure/);
    if (name === 'Check formatting') {
      assert.equal(owner, `      - name: ${name}\n        if: matrix.os == 'ubuntu-latest'\n        run: ${command}\n`);
    } else {
      assert.equal(owner, `      - name: ${name}\n        if: matrix.os == 'ubuntu-latest'\n        working-directory: tests/node-semver-oracle\n        run: |\n          ${command}\n`);
    }
    assert.equal(ci.split(command).length - 1, 1, `${name} must run exactly once`);
  }
  const node = step(native, 'Install relocatable Node.js for native sandbox tests');
  assert.match(node, /node-version: 22\n/);
  assert.doesNotMatch(node, /if:|continue-on-error:/);
  assert(native.indexOf(node) < native.indexOf('Verify pinned node-semver compatibility oracle'));
  for (const [name, command] of [
    ['Run Clippy', 'cargo clippy --workspace --all-targets --all-features --locked -- -D warnings'],
    ['Run tests', 'cargo test --workspace --all-features --locked -- --show-output'],
    ['Test development build cache selection', 'python -m unittest discover -s tests -p test_dev.py -v'],
    ['Run nested integration workspace', 'cargo test --manifest-path tests/integration/Cargo.toml --locked'],
  ]) {
    const retained = step(native, name);
    assert(retained.includes(`run: ${command}\n`));
    assert.doesNotMatch(retained, /if:|continue-on-error:|--exclude|--skip|--ignored/);
  }
  const docs = step(native, 'Test executable documentation runner (Unix)');
  assert.match(docs, /if: runner.os != 'Windows'\n        run: python3 -m unittest discover -s tests -p test_doc_examples.py -v\n/);
  assert.doesNotMatch(docs, /continue-on-error:/);
  const python = step(native, 'Install Python for development and documentation tests');
  assert.match(python, /python-version: '3.12'/);
  assert.doesNotMatch(python, /if:|continue-on-error:/);
  assert.match(step(native, 'Install Rust toolchain'), /components: rustfmt, clippy/);
  assert.match(job(ci, 'package'), /needs: \[test, security\]/);
};

test('source-only checks have one mandatory Ubuntu owner while native gates stay cross-platform', async () => {
  assertSourceOnlyOwners(await workflow('ci'));
  const manifest = JSON.parse(await source('tests/node-semver-oracle/package.json'));
  const lock = JSON.parse(await source('tests/node-semver-oracle/package-lock.json'));
  assert.equal(manifest.devDependencies.semver, '7.8.5');
  assert.equal(lock.packages['node_modules/semver'].version, '7.8.5');
  assert.match(lock.packages['node_modules/semver'].integrity, /^sha512-/);
  assert.match(await source('Cargo.toml'), /"crates\/tapid-resolver"/);
});

test('source-only ownership contract rejects missing, disabled, optional and widened Ubuntu owners', async () => {
  const ci = await workflow('ci');
  assertSourceOnlyOwners(ci);
  for (const name of ['Check formatting', 'Verify pinned node-semver compatibility oracle']) {
    const owner = step(job(ci, 'test'), name);
    for (const replacement of [
      '',
      owner.replace("matrix.os == 'ubuntu-latest'", 'false'),
      owner.replace("matrix.os == 'ubuntu-latest'", "matrix.os != 'Windows'"),
      owner.replace("        if: matrix.os == 'ubuntu-latest'\n", ''),
      owner.replace('        run:', '        continue-on-error: true\n        run:'),
    ]) {
      assert.throws(() => assertSourceOnlyOwners(ci.replace(owner, replacement)), undefined, name);
    }
  }
  for (const broken of [
    ci.replace('os: [ubuntu-latest, macos-latest, windows-latest]', 'os: [macos-latest, windows-latest]'),
    ci.replace('  test:\n', '  test:\n    if: false\n'),
    ci.replace('  test:\n', '  test:\n    continue-on-error: true\n'),
    ci.replace('  test:\n', '  test:\n    defaults:\n      run:\n        shell: bash {0}\n'),
    ci.replace('        os: [ubuntu-latest, macos-latest, windows-latest]', '        exclude: [{os: ubuntu-latest}]\n        os: [ubuntu-latest, macos-latest, windows-latest]'),
  ]) assert.throws(() => assertSourceOnlyOwners(broken));
});

test('main native workspace owns command-help and Windows collision tests without standalone duplicates', async () => {
  const names = await readdir(new URL('../../.github/workflows/', import.meta.url));
  assert(!names.includes('cli-documentation.yml'), 'obsolete standalone command-help owner');
  const ci = await workflow('ci');
  const native = job(ci, 'test');
  assert.match(ci, /push:\n    branches: \[main\]\n  pull_request:/);
  assert.match(native, /os: \[ubuntu-latest, macos-latest, windows-latest\]/);
  assert.match(native, /node-version: 22/);
  assert.match(native, /TAPID_REQUIRE_NODE_ASSERTIONS: '1'/);
  assert(native.indexOf('node-version: 22') < native.indexOf('cargo test --workspace'));
  assert.match(native, /run: cargo test --workspace --all-features --locked -- --show-output\n/);
  const workspaceStep = native.slice(native.indexOf('      - name: Run tests\n'), native.indexOf('      - name: Install Python'));
  assert.doesNotMatch(workspaceStep, /working-directory:|--exclude|--skip|--ignored|continue-on-error:|if:/);
  assert.doesNotMatch(native, /defaults:|continue-on-error:/);
  assert.match(native, /cargo test --manifest-path tests\/integration\/Cargo.toml --locked/);
  const manifest = await source('Cargo.toml');
  assert.match(manifest, /"crates\/tapid-cli"/);
  const lib = await source('crates/tapid-cli/src/lib.rs');
  assert.match(lib, /mod commands;/);
  assert.match(lib, /mod filesystem;/);
  const commands = await source('crates/tapid-cli/src/commands/mod.rs');
  assert.match(commands, /mod documentation;/);
  const docs = await source('crates/tapid-cli/src/commands/documentation.rs');
  assert.match(docs, /#\[test\]/);
  assert.doesNotMatch(docs, /#\[ignore/);
  const filesystem = await source('crates/tapid-cli/src/filesystem/mod.rs');
  assert.match(filesystem, /mod tree;/);
  const tree = await source('crates/tapid-cli/src/filesystem/tree.rs');
  assert.match(tree, /#\[cfg\(windows\)\]\s*#\[test\]\s*fn native_windows_shim_materialization_rejects_collisions_before_writes\(\)/);
  assert.doesNotMatch(tree, /#\[ignore/);
  for (const name of names.filter(name => name.endsWith('.yml'))) {
    const yaml = await workflow(name.slice(0, -4));
    assert.doesNotMatch(yaml, /cargo test[^\n]*native_windows_shim_materialization_rejects_collisions_before_writes|cargo test[^\n]*commands::documentation::/);
  }
});

test('main release-contract is the sole installer syntax/help owner and fails closed', async () => {
  const names = await readdir(new URL('../../.github/workflows/', import.meta.url));
  assert(!names.includes('website-installer-sync.yml'), 'obsolete standalone installer owner');
  const ci = await workflow('ci');
  const contracts = job(ci, 'release-contract');
  assert.doesNotMatch(contracts, /if:|continue-on-error:/);
  assert.match(contracts, /run: \|\n          sh -n scripts\/install.sh\n          sh scripts\/install.sh --help >\/dev\/null/);
  assert.match(contracts, /shell: pwsh/);
  assert.match(contracts, /\[System.Management.Automation.PSParser\]::Tokenize\(/);
  assert.match(contracts, /\(Get-Content -Raw scripts\/install.ps1\),\n            \[ref\]\$errors/);
  assert.match(contracts, /if \(\$errors -and \$errors.Count -gt 0\) \{[\s\S]*?Write-Error\n            exit 1/);
  for (const scope of ['sh -n scripts/install.sh', 'sh scripts/install.sh --help', '[System.Management.Automation.PSParser]::Tokenize(']) {
    const owners = [];
    for (const name of names.filter(name => name.endsWith('.yml'))) {
      if ((await workflow(name.slice(0, -4))).includes(scope)) owners.push(name);
    }
    assert.deepEqual(owners, ['ci.yml'], `${scope} must have exactly one workflow owner`);
  }
  const coordinator = await workflow('crates-publication');
  assert.match(coordinator, /tools\/release\/automation.ts wait-ci/);
  const automation = await source('tools/release/automation.ts');
  assert.match(automation, /actions\/workflows\/ci.yml\/runs\?event=push&head_sha=/);
  assert.match(automation, /readChecks: async/);
});

test('CI PowerShell syntax validator accepts the installer and rejects malformed input', async (context) => {
  try {
    await execFileAsync('pwsh', ['-NoProfile', '-NonInteractive', '-Command', '$null'], { timeout: 30_000 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT' || process.env.CI === 'true') throw error;
    context.skip('PowerShell is unavailable locally; CI must execute this regression');
    return;
  }
  const validator = step(job(await workflow('ci'), 'release-contract'), 'Validate PowerShell installer syntax');
  assert.match(validator, /shell: pwsh\n        run: \|\n/);
  const script = validator.split('        run: |\n')[1].replace(/^          /gm, '');
  const directory = await mkdtemp(join(tmpdir(), 'tapid-syntax-'));
  try {
    await mkdir(join(directory, 'scripts'));
    const input = join(directory, 'scripts', 'install.ps1');
    const harness = join(directory, 'validate.ps1');
    await writeFile(harness, script);
    await writeFile(input, await source('scripts/install.ps1'));
    const run = () => execFileAsync('pwsh', ['-NoProfile', '-NonInteractive', '-File', harness], {
      cwd: directory, timeout: 30_000,
    });
    await run();
    // Parse only: neither the real installer nor the malformed source is executed.
    await writeFile(input, 'function Broken {\n');
    await assert.rejects(run, (error: { code: number; stderr: string }) => {
      assert.equal(error.code, 1, 'malformed PowerShell must exit nonzero');
      assert.match(error.stderr, /Missing|closing|brace|ParseError/i);
      return true;
    });
    // Prove the harness catches a disabled error guard, not just parser availability.
    await writeFile(harness, script.replace('if ($errors -and $errors.Count -gt 0)', 'if ($false)'));
    await execFileAsync('pwsh', ['-NoProfile', '-NonInteractive', '-File', harness], {
      cwd: directory, timeout: 30_000,
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('every release trigger uses the same candidate approval and same-run smoke before OIDC', async () => {
  const coordinator = await workflow('crates-publication');
  assert.match(coordinator, /push:\n    branches: \[main\]/);
  assert.match(coordinator, /docs\/releases\/intent.json/);
  assert.doesNotMatch(coordinator, /workflow_run:/);
  assert.match(coordinator, /automation.ts wait-ci/);
  assert.match(coordinator, /automation.ts check-plan/);
  assert.match(coordinator, /uses: .\/\.github\/workflows\/release-publication.yml/);
  assert.match(coordinator, /needs: \[preflight, candidate, verify, promote, smoke\]/);
  assert.match(coordinator, /environment: crates-io-release/);
  const publish = coordinator.slice(coordinator.indexOf('\n  publish:'));
  assert.match(publish, /candidate.ts publish/);
  assert(publish.indexOf('candidate.ts publish') < publish.indexOf('crates-io-auth-action@'));
  assert.doesNotMatch(coordinator, /event=release&status=success/);
  assert.match(coordinator, /secrets: inherit/);
});
test('approval consumes the immutable unsigned artifact after package preflight', async () => {
  const build = await workflow('release-publication');
  assert.doesNotMatch(build, /push:|workflow_dispatch:/);
  assert.match(build, /workflow_call:/);
  const assemble = build.slice(build.indexOf('\n  assemble:'), build.indexOf('\n  draft-release:'));
  assert.match(assemble, /candidate.ts unsigned/);
  assert.match(assemble, /bootstrap.ts release/);
  assert.doesNotMatch(assemble, /secrets\.|environment:/);
  const gate = build.slice(build.indexOf('\n  draft-release:'));
  assert.match(gate, /environment: stable-release/);
  assert.match(gate, /artifact-ids: \$\{\{ needs.assemble.outputs.artifact_id \}\}/);
  assert(gate.indexOf('validate-unsigned') < gate.indexOf('sign.ts sign'));
  assert(gate.indexOf('sign.ts verify') < gate.indexOf('candidate.ts create-draft'));
  assert.match(gate, /candidate_sha256/);
  assert.doesNotMatch(gate, /cargo build|cargo package|cargo test/);
});
test('native and public smoke verification can be called with exact release identity', async () => {
  for (const name of ['release-draft-verify', 'release-public-smoke']) {
    const yaml = await workflow(name);
    assert.match(yaml, /workflow_call:/);
    assert.match(yaml, /release_id:/);
    assert.match(yaml, /commit_sha:/);
  }
});
