// Run the real built CLI, never a shell wrapper or a simulated backend.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const project = process.env.TAPID_FIXTURE_PROJECT;
assert.ok(project, 'TAPID_FIXTURE_PROJECT is required');
const binary = path.resolve('target', 'debug', process.platform === 'win32' ? 'tapid.exe' : 'tapid');
const lifecycleMarker = path.join(project, 'LIFECYCLE_SHOULD_NOT_RUN');
const startMarker = 'TAPID_FIXTURE_STARTED=';
assert.ok(['darwin', 'linux', 'win32'].includes(process.platform), 'unsupported validation host');

function invoke(args, fixtureEnvironment = '1') {
  const result = spawnSync(binary, args, {
    encoding: 'utf8',
    env: { ...process.env, TAPID_FIXTURE: fixtureEnvironment },
    timeout: 60_000,
    maxBuffer: 4 * 1024 * 1024,
    shell: false,
  });
  process.stdout.write(result.stdout || '');
  process.stderr.write(result.stderr || '');
  assert.ifError(result.error);
  assert.equal(result.signal, null, 'CLI must exit normally');
  assert.ok(Number.isInteger(result.status), 'CLI must return an exit code');
  assert.equal(fs.existsSync(lifecycleMarker), false, 'install lifecycle must never run');
  return result;
}

// This fixture deliberately requests no limits and grants no writes/network access.
const policy = fs.readFileSync(path.join(project, 'tapid.toml'), 'utf8');
assert.match(policy, /assurance = "restricted"/);
assert.match(policy, /write = \[\]/);
assert.match(policy, /network = false/);
assert.doesNotMatch(policy, /timeout_seconds|max_output_bytes|max_processes|max_memory_bytes/);
assert.equal(invoke(['install', '--project-dir', project, '--offline', '--frozen']).status, 0);
assert.ok(fs.statSync(path.join(project, 'node_modules')).isDirectory());

const cases = [
  { args: ['forwarded', '0'], status: 0 },
  { args: ['wrong', '0'], status: 41 },
  { args: ['forwarded', '37'], status: 37 },
  { args: ['forwarded', '0'], status: 42, environment: 'wrong' },
  // Exact argv comparison below catches double appending and shell interpolation.
  { args: ["spaces 'quotes' $HOME ; literal", '0'], status: 41 },
  { args: ['forwarded', '0', 'extra'], status: 44 },
];
for (const test of cases) {
  const result = invoke(
    ['run', '--project-dir', project, '--receipt-json', 'test', '--', ...test.args],
    test.environment,
  );
  const output = result.stdout + result.stderr;
  const receipts = result.stderr.split(/\r?\n/).filter(line => line.startsWith('{')).map(line => JSON.parse(line));
  if (process.platform !== 'darwin') {
    assert.equal(result.status, 1, 'unsupported native containment must fail closed');
    assert.match(result.stderr, /unsupported-containment/);
    assert.match(result.stderr, /no process was started and no enforcement receipt was issued/);
    assert.equal(output.includes(startMarker), false, 'unsupported backend must not start Node');
    assert.equal(receipts.length, 0, 'unsupported backend must not issue a JSON receipt');
    assert.doesNotMatch(output, /sandbox receipt:|"schema_version"/);
  } else {
    assert.equal(result.status, test.status, `unexpected child status for ${JSON.stringify(test.args)}`);
    const starts = result.stdout.split(/\r?\n/).filter(line => line.startsWith(startMarker));
    assert.equal(starts.length, 1, 'the real Node child must run exactly once');
    assert.deepEqual(JSON.parse(starts[0].slice(startMarker.length)), test.args);
    assert.equal(receipts.length, 1, 'a native run must issue exactly one receipt');
    const receipt = receipts[0];
    assert.equal(receipt.schema_version, 1);
    assert.equal(receipt.assurance, 'Restricted');
    assert.equal(receipt.backend.name, 'tapid-runner/macos-seatbelt-restricted-experimental');
    for (const dimension of ['filesystem_read', 'filesystem_write', 'network', 'environment_sanitization']) {
      assert.equal(receipt.enforced[dimension], true, `${dimension} must be natively enforced`);
    }
    assert.equal(receipt.termination, `Exited(${test.status})`);
    assert.ok(Object.values(receipt.configured_limits).every(value => value === null));
  }
}
console.log(process.platform === 'darwin'
  ? 'macOS native Restricted: install, lifecycle suppression, child marker, exact forwarding, environment and exit codes passed.'
  : `${process.platform}: install, lifecycle suppression, unsupported containment, no target marker and no receipt passed.`);
