import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFile, readdir } from 'node:fs/promises';
const workflow = (name: string) => readFile(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
const source = (path: string) => readFile(new URL(`../../${path}`, import.meta.url), 'utf8');
const job = (yaml: string, name: string) => {
  const section = yaml.match(new RegExp(`^  ${name}:\\n[\\s\\S]*?(?=^  [a-z][a-z-]*:|$(?![\\s\\S]))`, 'm'))?.[0];
  assert(section, `missing ${name} job`);
  return section;
};

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
