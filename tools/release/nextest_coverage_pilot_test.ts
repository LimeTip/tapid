import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFile, readdir } from 'node:fs/promises';

const workflows = new URL('../../.github/workflows/', import.meta.url);
const job = (yaml: string, id: string) => {
  const block = yaml.match(new RegExp(`^  ${id}:\\n[\\s\\S]*?(?=^  [a-z][a-z-]*:|$(?![\\s\\S]))`, 'm'))?.[0];
  assert(block, `missing ${id} job`);
  return block;
};
const candidateId = 'nextest-coverage-pilot';
function contract(ci: string) {
  for (const [id, command] of [
    ['nextest', 'cargo nextest run --workspace --all-features --locked'],
    ['coverage', 'cargo llvm-cov --workspace --all-features --locked --lcov --output-path lcov.info'],
  ]) {
    const original = job(ci, id);
    assert.match(original, /runs-on: ubuntu-latest/);
    assert(original.includes(command));
    assert.doesNotMatch(original, /\n    (?:if|needs|continue-on-error):|\n        (?:if|continue-on-error): (?!always\(\))/);
    assert.doesNotMatch(original, /--exclude|--skip|--ignored|\|\| true/);
  }
  assert.match(job(ci, 'coverage'), /name: lcov-coverage\n          path: lcov.info\n          if-no-files-found: error/);
  const native = job(ci, 'test');
  assert.match(native, /os: \[ubuntu-latest, macos-latest, windows-latest\]/);
  assert.match(native, /cargo test --workspace --all-features --locked -- --show-output/);
  assert.match(native, /cargo test --manifest-path tests\/integration\/Cargo.toml --locked/);
  const pilot = job(ci, candidateId);
  assert.match(pilot, /name: Experimental nextest \+ coverage \(Ubuntu; not release evidence\)/);
  assert.match(pilot, /runs-on: ubuntu-latest/);
  assert.match(pilot, /continue-on-error: true/);
  assert.doesNotMatch(pilot, /\n    (?:needs|outputs):|\n        continue-on-error:|--exclude|--skip|--ignored|\|\| true/);
  assert.match(pilot, /TAPID_REQUIRE_NODE_ASSERTIONS: '1'/);
  for (const setup of ['node-version: 22', 'components: llvm-tools-preview', 'tool: cargo-nextest,cargo-llvm-cov']) {
    assert(pilot.indexOf(setup) >= 0 && pilot.indexOf(setup) < pilot.indexOf('cargo llvm-cov nextest --workspace'));
  }
  assert.match(pilot, /cargo llvm-cov nextest --workspace --all-features --locked --no-fail-fast --lcov --output-path lcov.info/);
  assert.match(pilot, /cargo nextest list --workspace --all-features --locked --message-format json/);
  assert.match(pilot, /cargo llvm-cov report --json --output-path stage5-evidence\/coverage.json/);
  assert.match(pilot, /name: stage5-candidate-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/);
  assert.doesNotMatch(pilot, /name: lcov-coverage\n|__verify-release|release\.ts|download-artifact/);
  // No status consumer may use the optional experiment in place of a real gate.
  assert.doesNotMatch(ci.replace(pilot, ''), /needs[^\n]*nextest-coverage-pilot|needs\.nextest-coverage-pilot/);
  for (const id of ['nextest', 'coverage']) {
    const original = job(ci, id);
    assert.match(original, /stage5-evidence/);
    assert.match(original, /github.run_id/);
    assert.match(original, /github.run_attempt/);
  }
}

test('Stage 5 is additive evidence, never a replacement for standalone or native gates', async () => {
  contract(await readFile(new URL('ci.yml', workflows), 'utf8'));
  for (const file of await readdir(workflows)) {
    if (file === 'ci.yml' || !file.endsWith('.yml')) continue;
    assert.doesNotMatch(await readFile(new URL(file, workflows), 'utf8'), /nextest-coverage-pilot|stage5-candidate-/);
  }
  const config = await readFile(new URL('../../.config/nextest.toml', import.meta.url), 'utf8');
  assert.equal(config, '# Temporary Stage 5 evidence only: no test selection or scheduling overrides.\n[profile.default.junit]\npath = "stage5-junit.xml"\nstore-success-output = true\nstore-failure-output = true\n');
});

test('Stage 5 contract rejects removal, optional originals, release substitution and evidence loss', async () => {
  const ci = await readFile(new URL('ci.yml', workflows), 'utf8');
  contract(ci);
  const pilot = job(ci, candidateId);
  const mutations = [
    ...['nextest', 'coverage'].flatMap(id => {
      const original = job(ci, id);
      return [ci.replace(original, ''), ci.replace(`  ${id}:\n`, `  ${id}:\n    if: false\n`), ci.replace(`  ${id}:\n`, `  ${id}:\n    continue-on-error: true\n`)];
    }),
    ci.replace('cargo test --workspace --all-features --locked -- --show-output', 'cargo test --lib'),
    ci.replace('cargo test --manifest-path tests/integration/Cargo.toml --locked', 'cargo test --lib'),
    ci.replace(pilot, pilot.replace('continue-on-error: true\n', '')),
    ci.replace(pilot, pilot.replace("TAPID_REQUIRE_NODE_ASSERTIONS: '1'", "TAPID_REQUIRE_NODE_ASSERTIONS: '0'")),
    ci.replace(pilot, pilot.replace('--no-fail-fast', '--skip cli')),
    ci.replace(pilot, pilot.replace('stage5-candidate-${{ github.run_id }}-${{ github.run_attempt }}', 'lcov-coverage')),
    ci.replace('needs: [test, security]', 'needs: [nextest-coverage-pilot, security]'),
    ci.replace(pilot, pilot.replace('    steps:', '    outputs:\n      release-evidence: true\n    steps:')),
  ];
  for (const [index, broken] of mutations.entries()) assert.throws(() => contract(broken), undefined, `mutation ${index}`);
});
