import { test } from 'node:test';
import { createHash } from 'node:crypto';
import { strict as assert } from 'node:assert';
import { readFile, readdir, mkdtemp, mkdir, writeFile, rm, copyFile, symlink } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { join, delimiter } from 'node:path';
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

const assertStandaloneTestOwners = (ci: string) => {
  assert.doesNotMatch(ci, /nextest-coverage-pilot|stage5-|cargo llvm-cov nextest|cargo nextest list|cargo llvm-cov report|cargo llvm-cov show-env|--no-report|-- --list|\.profraw|tee /);
  for (const [id, name, command, tool] of [
    ['nextest', 'Run tests with nextest', 'cargo nextest run --workspace --all-features --locked', 'cargo-nextest'],
    ['coverage', 'Generate LCOV coverage report', 'cargo llvm-cov --workspace --all-features --locked --lcov --output-path lcov.info', 'cargo-llvm-cov'],
  ] as const) {
    const owner = job(ci, id);
    assert.match(owner, /runs-on: ubuntu-latest/);
    assert.match(owner, /TAPID_REQUIRE_NODE_ASSERTIONS: '1'/);
    assert.doesNotMatch(owner, /\n    (?:if|needs|continue-on-error):|\n        (?:if|continue-on-error):|--exclude|--skip|--ignored|\|\| true/);
    assert.equal(step(owner, name).trimEnd(), `      - name: ${name}\n        run: ${command}`);
    assert.match(owner, new RegExp(`tool: ${tool}\\n`));
    const node = step(owner, 'Install Node.js 22 for required CLI assertions');
    assert.match(node, /uses: actions\/setup-node@[a-f0-9]{40} # v7\n        with:\n          node-version: 22\n/);
    assert(owner.indexOf(node) < owner.indexOf(step(owner, name)));
  }
  assert.match(step(job(ci, 'coverage'), 'Upload LCOV coverage report'), /name: lcov-coverage\n          path: lcov.info\n          if-no-files-found: error\n          retention-days: 30/);
  assertSourceOnlyOwners(ci);
};

test('standalone nextest and Cargo coverage remain mandatory without comparison builds', async () => {
  assertStandaloneTestOwners(await workflow('ci'));
  for (const name of await readdir(new URL('../../.github/workflows/', import.meta.url))) {
    if (name.endsWith('.yml')) assert.doesNotMatch(await workflow(name.slice(0, -4)), /nextest-coverage-pilot|stage5-|cargo llvm-cov nextest/);
  }
  await assert.rejects(source('.config/nextest.toml'), { code: 'ENOENT' });
  await assert.rejects(source('tools/release/nextest_coverage_pilot_test.ts'), { code: 'ENOENT' });
});

test('standalone contracts reject missing gates, assertion bypass and comparison work', async () => {
  const ci = await workflow('ci');
  assertStandaloneTestOwners(ci);
  for (const id of ['nextest', 'coverage']) {
    const owner = job(ci, id);
    const node = step(owner, 'Install Node.js 22 for required CLI assertions');
    for (const replacement of [
      '', owner.replace('    steps:', '    if: false\n    steps:'),
      owner.replace('    steps:', '    continue-on-error: true\n    steps:'),
      owner.replace("TAPID_REQUIRE_NODE_ASSERTIONS: '1'", "TAPID_REQUIRE_NODE_ASSERTIONS: '0'"),
      owner.replace(node, ''), owner.replace('node-version: 22', 'node-version: 20'),
      owner.replace('        run: cargo', '        if: false\n        run: cargo'),
      owner.replace('--all-features', '--exclude tapid'),
      owner.replace('    steps:', '    steps:\n      - name: Discover comparison tests\n        run: cargo nextest list --workspace --all-features --locked\n'),
    ]) assert.throws(() => assertStandaloneTestOwners(ci.replace(owner, replacement)), undefined, id);
  }
  for (const broken of [
    ci + '\n  nextest-coverage-pilot:\n    runs-on: ubuntu-latest\n',
    ci.replace('cargo llvm-cov --workspace', 'cargo llvm-cov nextest --workspace'),
    ci.replace('name: lcov-coverage', 'name: comparison-coverage'),
    ci.replace('retention-days: 30\n\n  release-platform-build:', 'retention-days: 14\n\n  release-platform-build:'),
    ci.replace('cargo nextest run --workspace', 'cargo nextest run --workspace --no-fail-fast'),
    ci.replace('    steps:', '    steps:\n      - name: Export comparison\n        run: cargo llvm-cov report --json\n'),
  ]) assert.throws(() => assertStandaloneTestOwners(broken));
});

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

const assertWindowsWriteNodeOwner = (ci: string) => {
  const native = job(ci, 'test');
  const setup = step(native, 'Install relocatable Node.js for native sandbox tests');
  const prerequisite = step(native, 'Bind absolute Node for Windows native write tests');
  assert.match(prerequisite, /if: runner.os == 'Windows'\n        shell: pwsh\n/);
  assert.doesNotMatch(prerequisite, /continue-on-error:|SilentlyContinue|\|\|/);
  assert.match(prerequisite, /\$ErrorActionPreference = 'Stop'/);
  assert.match(prerequisite, /Get-Command node -CommandType Application -ErrorAction Stop \| Select-Object -First 1\)\.Source/);
  assert.match(prerequisite, /\[IO.Path\]::IsPathFullyQualified\(\$node\)/);
  assert.match(prerequisite, /Test-Path -LiteralPath \$node -PathType Leaf/);
  assert.match(prerequisite, /& \$node --version/);
  assert.match(prerequisite, /\$LASTEXITCODE -ne 0/);
  assert.match(prerequisite, /"TAPID_TEST_NODE=\$node" >> \$env:GITHUB_ENV/);
  assert(native.indexOf(setup) < native.indexOf(prerequisite));
  assert(native.indexOf(prerequisite) < native.indexOf(step(native, 'Run tests')));
  assertSourceOnlyOwners(ci);
};

test('Windows native write owner binds a validated absolute Node before workspace tests', async () => {
  const ci = await workflow('ci');
  assertWindowsWriteNodeOwner(ci);
  const owner = job(ci, 'test');
  const prerequisite = step(owner, 'Bind absolute Node for Windows native write tests');
  for (const replacement of [
    '', prerequisite.replace("runner.os == 'Windows'", 'false'),
    prerequisite.replace('        shell: pwsh', '        continue-on-error: true\n        shell: pwsh'),
    prerequisite.replace(' -ErrorAction Stop', ' -ErrorAction SilentlyContinue'),
    prerequisite.replace(' | Select-Object -First 1', ''),
    prerequisite.replace('Select-Object -First 1', 'Select-Object -Last 1'),
    prerequisite.replace('[IO.Path]::IsPathFullyQualified($node)', '$true'),
    prerequisite.replace('Test-Path -LiteralPath $node -PathType Leaf', '$true'),
    prerequisite.replace('& $node --version', '& node --version'),
    prerequisite.replace('$LASTEXITCODE -ne 0', '$false'),
    prerequisite.replace('TAPID_TEST_NODE=$node', 'TAPID_TEST_NODE=node'),
  ]) assert.throws(() => assertWindowsWriteNodeOwner(ci.replace(owner, owner.replace(prerequisite, replacement))));
  const late = owner.replace(prerequisite, '').replace(step(owner, 'Run tests'), step(owner, 'Run tests') + prerequisite);
  assert.throws(() => assertWindowsWriteNodeOwner(ci.replace(owner, late)));
  const tests = await source('crates/tapid-runner/src/windows_job_tests.rs');
  for (const name of ['appcontainer_node_ordinary_project_write_mutation_contract', 'appcontainer_node_write_only_grant_denies_read']) {
    assert.match(tests, new RegExp(`#\\[test\\]\\s*fn ${name}\\(\\)`));
  }
  assert.match(tests, /var_os\("TAPID_TEST_NODE"\)\s*\.expect\(/);
});

test('Windows Node binding executes the first of two spaced PATH candidates in PowerShell', async (context) => {
  let powershell: string;
  try {
    const result = await execFileAsync('pwsh', ['-NoProfile', '-NonInteractive', '-Command', '(Get-Process -Id $PID).Path'], { timeout: 30_000 });
    powershell = result.stdout.trim();
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT' || process.env.CI === 'true') throw error;
    context.skip('PowerShell is unavailable locally; CI must execute this regression');
    return;
  }
  const prerequisite = step(job(await workflow('ci'), 'test'), 'Bind absolute Node for Windows native write tests');
  const script = prerequisite.split('        run: |\n')[1].replace(/^          /gm, '');
  const directory = await mkdtemp(join(tmpdir(), 'tapid-node-binding-'));
  try {
    const candidates = [join(directory, 'first Node runtime'), join(directory, 'second Node runtime')];
    const executable = process.platform === 'win32' ? 'node.exe' : 'node';
    for (const candidate of candidates) {
      await mkdir(candidate);
      if (process.platform === 'win32') await copyFile(process.execPath, join(candidate, executable));
      else await symlink(process.execPath, join(candidate, executable));
    }
    const environment = join(directory, 'github-env');
    const harness = join(directory, 'bind.ps1');
    const env = { ...process.env, PATH: candidates.join(delimiter), GITHUB_ENV: environment };
    const run = () => execFileAsync(powershell, ['-NoProfile', '-NonInteractive', '-File', harness], {
      env, timeout: 30_000,
    });
    // This assertion proves real Get-Command discovery returns both applications,
    // rather than mocking the collection responsible for the hosted CI failure.
    await writeFile(harness, "$ErrorActionPreference = 'Stop'\n$candidates = @(Get-Command node -CommandType Application -ErrorAction Stop)\nif ($candidates.Count -ne 2) { throw 'Expected two Node candidates' }\n$candidates.Source\n");
    const discovery = await run();
    for (const candidate of candidates) assert(discovery.stdout.includes(join(candidate, executable)));
    context.diagnostic(`PowerShell discovered two candidates:\n${discovery.stdout.trim()}`);
    await writeFile(environment, '');
    await writeFile(harness, script);
    const result = await run();
    assert.equal(result.stdout.trim(), process.version, 'the chosen absolute executable must actually run');
    assert.equal((await readFile(environment, 'utf8')).trim(), `TAPID_TEST_NODE=${join(candidates[0], executable)}`);
    context.diagnostic(`Exact workflow snippet: ${result.stdout.trim()}; exported first absolute spaced path`);

    // Exercise the retained nonzero-exit guard using the same real Node binary.
    await writeFile(environment, '');
    await writeFile(harness, script.replace('& $node --version', '& $node -e "process.exit(23)"'));
    await assert.rejects(run, (error: { code: number; stderr: string }) => {
      assert.notEqual(error.code, 0);
      assert.match(error.stderr, /Native write test Node is not executable/);
      return true;
    });
    assert.equal(await readFile(environment, 'utf8'), '', 'failed execution must not export Node');
    context.diagnostic('Real Node exit 23 rejected before GITHUB_ENV export');
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
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
const handoffName = 'ci-cli-${{ github.run_id }}-${{ github.run_attempt }}-windows-latest-Windows-X64-default-debug';
function assertWindowsHandoff(ci: string) {
  assertSourceOnlyOwners(ci);
  const producer = job(ci, 'platform-consumer-validation');
  const verifier = job(ci, 'windows-installer-contract');
  assert.match(verifier, /name: Windows installer contract\n    runs-on: windows-latest\n    needs: \[platform-consumer-validation\]\n    if: \$\{\{ always\(\) && !cancelled\(\) \}\}\n    steps:/);
  assert.match(producer, /name: Consumer validation \(\$\{\{ matrix.os \}\}\)/);
  assert.match(producer, /fail-fast: false/);
  assert.match(step(producer, 'Build Tapid binary'), /run: cargo build --bin tapid --locked\n/);
  for (const [name, condition, timeout] of [
    ['Verify install and lifecycle suppression; reject unsupported Linux containment', "runner.os == 'Linux'", ''],
    ['Verify native read-only child, forwarding and exit codes (Windows)', "runner.os == 'Windows'", '        timeout-minutes: 5\n'],
    ['Verify native Restricted child, forwarding and exit codes (macOS)', "runner.os == 'macOS'", ''],
  ] as const) assert.equal(step(producer, name), `      - name: ${name}\n        if: ${condition}\n${timeout}        run: node tests/fixtures/validate_consumer_project.js\n`);
  const prepare = step(producer, 'Prepare same-run Windows CLI handoff');
  const upload = step(producer, 'Retain same-run Windows CLI');
  for (const s of [prepare, upload]) assert.match(s, /if: runner.os == 'Windows'\n/);
  assert.match(prepare, /run: node --experimental-strip-types tools\/release\/ci_binary.ts prepare --directory "\$env:RUNNER_TEMP\/tapid-ci-cli" --binary target\/debug\/tapid.exe\n/);
  assert(producer.indexOf(prepare) > producer.indexOf(step(producer, 'Verify native Restricted child, forwarding and exit codes (macOS)')));
  assert(producer.indexOf(upload) > producer.indexOf(prepare));
  assert.match(upload, /uses: actions\/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7/);
  for (const text of [`name: ${handoffName}\n`, 'path: ${{ runner.temp }}/tapid-ci-cli/\n', 'if-no-files-found: error\n', 'retention-days: 1\n', 'compression-level: 0\n', 'overwrite: false\n']) assert(upload.includes(text));
  const download = step(verifier, 'Download same-attempt Windows CLI');
  assert.match(download, /uses: actions\/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8/);
  for (const text of [`name: ${handoffName}\n`, 'path: ${{ runner.temp }}/tapid-ci-cli-download\n', 'digest-mismatch: error\n']) assert(download.includes(text));
  const verify = step(verifier, 'Verify same-run native verifier');
  assert.match(verify, /shell: pwsh/);
  assert(verify.includes('node --experimental-strip-types tools/release/ci_binary.ts verify --directory "$env:RUNNER_TEMP/tapid-ci-cli-download" --destination target/debug/tapid.exe\n          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }'));
  assert(verify.includes('$actual = & target/debug/tapid.exe --version\n          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }'));
  assert(verify.includes('if ($actual -cne "tapid $expected") { throw "Unexpected native version: $actual" }'));
  assert(verifier.indexOf(download) < verifier.indexOf(verify));
  assert(verifier.indexOf(verify) < verifier.indexOf('      - name: Install and reject archive fixtures'));
  for (const s of [producer, verifier]) assert.doesNotMatch(s, /continue-on-error:|: write|github-token:|run-id:|repository:|pattern:|merge-multiple:|\|\| true|if: (?:false|always\(\)|success\(\))/);
  assert.doesNotMatch(verifier, /cargo build|rust-toolchain@|rust-cache@/);
  assert.doesNotMatch(download + verify, /\n        if:/);
  const fixture = verifier.slice(verifier.indexOf('      - name: Install and reject archive fixtures'));
  // Stage 3 fixture snapshot: no installer/security assertion or cleanup changes permitted.
  assert.equal(createHash('sha256').update(fixture).digest('hex'), 'ab1cd26c75fe0313e34482f1982369469e0a8f4920158b3c93515e1a521b9348');
}

test('Windows-only consumer handoff retains real verifier and same-attempt fail-closed ownership', async () => {
  assertWindowsHandoff(await workflow('ci'));
});
test('Windows handoff contract rejects ownership, stale-attempt, bypass and fixture mutations', async () => {
  const ci = await workflow('ci'); assertWindowsHandoff(ci);
  const producer = job(ci, 'platform-consumer-validation'); const verifier = job(ci, 'windows-installer-contract');
  const upload = step(producer, 'Retain same-run Windows CLI');
  const mutations = [
    ci.replace(verifier, ''), ci.replace(verifier, verifier.replace('    steps:', '    continue-on-error: true\n    steps:')),
    ci.replace('always() && !cancelled()', "needs.platform-consumer-validation.result == 'success'"),
    ci.replace('always() && !cancelled()', 'always()'),
    ci.replace('    needs: [platform-consumer-validation]\n', ''),
    ci.replace(verifier, verifier.replace('    steps:', '    if: false\n    steps:')),
    ci.replace('digest-mismatch: error', 'digest-mismatch: warn'),
    ci.replaceAll('${{ github.run_attempt }}-', ''),
    ci.replace(verifier, verifier.replace(`name: ${handoffName}`, 'pattern: ci-cli-*')),
    ci.replace(verifier, verifier.replace('digest-mismatch: error', 'digest-mismatch: error\n          github-token: ${{ github.token }}')),
    ci.replace(verifier, verifier.replace('digest-mismatch: error', 'digest-mismatch: error\n          run-id: 123')),
    ci.replace(verifier, verifier.replace(step(verifier, 'Verify same-run native verifier'), '')),
    ci.replace(producer, producer.replace(upload, '').replace('      - name: Build Tapid binary', upload + '      - name: Build Tapid binary')),
    ci.replace(upload, upload.replace("runner.os == 'Windows'", 'always()')),
    ci.replace(producer, producer.replace("if: runner.os == 'Linux'", 'if: false')),
    ci.replace(producer, producer.replace("if: runner.os == 'macOS'", 'if: false')),
    ci.replace(producer, producer.replace('run: node tests/fixtures/validate_consumer_project.js', 'run: echo skipped')),
    ci.replace(upload, upload.replace('overwrite: false', 'overwrite: true')),
    ci.replace(verifier, verifier.replace('$actual = & target/debug/tapid.exe --version', '$actual = & tapid --version')),
    ci.replace('if ($actual -cne "tapid $expected")', 'if ($false)'),
  ];
  const windowsConsumer = step(producer, 'Verify native read-only child, forwarding and exit codes (Windows)');
  for (const replacement of [
    '',
    windowsConsumer.replace("runner.os == 'Windows'", 'false'),
    windowsConsumer.replace("runner.os == 'Windows'", "runner.os != 'macOS'"),
    windowsConsumer.replace('        timeout-minutes: 5\n', ''),
    windowsConsumer.replace('run: node tests/fixtures/validate_consumer_project.js', 'run: echo skipped'),
    windowsConsumer.replace('        run:', '        continue-on-error: true\n        run:'),
  ]) {
    assert.notEqual(replacement, windowsConsumer, 'Windows consumer mutation must change its exact owner');
    mutations.push(ci.replace(producer, producer.replace(windowsConsumer, replacement)));
  }
  for (const broken of mutations) {
    assert.notEqual(broken, ci, 'handoff mutation must not be vacuous');
    assert.throws(() => assertWindowsHandoff(broken));
  }
  for (const text of ['exactly one member named tapid.exe', 'release record signature verification failed', 'bootstrap archive checksum mismatch', "SetEnvironmentVariable('Path', $originalUserPath, 'User')"]) {
    assert.throws(() => assertWindowsHandoff(ci.replace(verifier, verifier.replace(text, 'disabled'))));
  }
});
test('public installer discovery runs independently and requires its downloaded installer', async () => {
  const smoke = await workflow('release-public-smoke');
  for (const name of ['unix', 'windows']) {
    const section = job(smoke, name);
    const publicInstall = step(section, 'Check the public website installer with an explicit version');
    const discovery = step(section, 'Install latest release through discovery');
    assert.match(publicInstall, /id: install_public\n/);
    assert(publicInstall.includes("if: ${{ !cancelled() && steps.install_published.outcome == 'success' }}"));
    assert(discovery.includes("if: ${{ !cancelled() && steps.install_public.outcome == 'success' }}"));
    assert(section.indexOf(publicInstall) < section.indexOf(discovery));
  }
});

test('release approval still runs when recovery skips the build job', async () => {
  const publication = await workflow('release-publication');
  const assemble = job(publication, 'assemble');
  const signing = job(publication, 'draft-release');
  assert(assemble.includes("needs.build.result == 'success' || needs.build.result == 'skipped'"));
  assert.match(signing, /if: \$\{\{ !cancelled\(\) && needs\.assemble\.result == 'success' \}\}/);
  assert.match(signing, /environment: stable-release/);
});

test('previous-release upgrade uses that release installer beneath the current rollback floor', async () => {
  const smoke = await workflow('release-public-smoke');
  const resolver = job(smoke, 'resolve');
  const unix = job(smoke, 'unix');
  const windows = job(smoke, 'windows');
  const unixUpgrade = step(unix, 'Check previous-version upgrade and repeat upgrade through the public service');
  const windowsUpgrade = step(windows, 'Check previous-version upgrade and repeat upgrade through the public service');
  assert(resolver.includes('previous_installer_sh: ${{ steps.release.outputs.previous_installer_sh }}'));
  assert(resolver.includes('previous_installer_ps1: ${{ steps.release.outputs.previous_installer_ps1 }}'));
  assert(resolver.includes('gh api "repos/LimeTip/tapid/commits/$previous_tag" --jq .sha'));
  assert(resolver.includes('version <= (0, 0, 10)'));
  assert(resolver.includes('raw.githubusercontent.com/LimeTip/tapid/$previous_sha/scripts/install.sh'));
  assert(resolver.includes('raw.githubusercontent.com/LimeTip/tapid/$previous_sha/scripts/install.ps1'));
  assert(resolver.includes('https://github.com/LimeTip/tapid/releases/download/$previous_tag/install.sh'));
  assert(resolver.includes('https://github.com/LimeTip/tapid/releases/download/$previous_tag/install.ps1'));
  assert(unixUpgrade.includes('PREVIOUS_INSTALLER_URL: ${{ needs.resolve.outputs.previous_installer_sh }}'));
  assert(unixUpgrade.includes('"$PREVIOUS_INSTALLER_URL" -o "$previous_installer"'));
  assert(unixUpgrade.includes('previous_installer="$RUNNER_TEMP/previous-install.sh"'));
  assert(unixUpgrade.includes('sh "$previous_installer" --version "$PREVIOUS_TAG"'));
  assert(!unixUpgrade.includes('sh "$RUNNER_TEMP/public-install.sh" --version "$PREVIOUS_TAG"'));
  assert(windowsUpgrade.includes('PREVIOUS_INSTALLER_URL: ${{ needs.resolve.outputs.previous_installer_ps1 }}'));
  assert(windowsUpgrade.includes('-fsSL $env:PREVIOUS_INSTALLER_URL --output $previousInstaller'));
  assert(windowsUpgrade.includes('& $previousInstaller -Version $env:PREVIOUS_TAG'));
  assert(!windowsUpgrade.includes('& $installer -Version $env:PREVIOUS_TAG'));
});
