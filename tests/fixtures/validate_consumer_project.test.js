// Offline validator regression tests; simulated children are not release evidence.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');
const source = fs.readFileSync(path.join(__dirname, 'validate_consumer_project.js'), 'utf8');

function validate(platform, failure, releaseTag = 'v0.0.10') {
  const calls = [];
  const binary = path.resolve('published', 'tapid');
  const context = {
    require(name) {
      if (name === 'node:fs') return {
        existsSync: () => false,
        statSync: () => ({ isDirectory: () => true }),
        readFileSync: () => 'assurance = "restricted"\nwrite = []\nnetwork = false',
      };
      if (name === 'node:child_process') return { spawnSync(executable, args, options) {
        assert.equal(executable, binary, 'must execute the supplied published binary');
        calls.push(args);
        const result = { status: 0, signal: null, stdout: '', stderr: '' };
        if (args[0] === 'install') return result;
        if (platform !== 'darwin' && releaseTag !== 'v0.0.9') {
          result.status = failure === 'success' ? 0 : 1;
          result.stderr = failure === 'unrelated' ? 'unrelated failure' :
            'sandbox execution failed (unsupported-containment): no process was started and no enforcement receipt was issued';
          if (failure === 'child') result.stdout = 'TAPID_FIXTURE_STARTED=[]';
          if (failure === 'receipt') result.stderr += '\n{"schema_version":1}';
          return result;
        }
        const forwarded = args.slice(args.indexOf('--') + 1);
        result.status = forwarded.length !== 2 ? 44 : forwarded[0] !== 'forwarded' ? 41 :
          options.env.TAPID_FIXTURE !== '1' ? 42 : Number(forwarded[1]);
        result.stdout = 'TAPID_FIXTURE_STARTED=' + JSON.stringify(forwarded) + '\n';
        result.stderr = JSON.stringify({ schema_version: 1, assurance: 'Restricted',
          backend: { name: 'tapid-runner/macos-seatbelt-restricted-experimental' },
          enforced: { filesystem_read: true, filesystem_write: true, network: true, environment_sanitization: true },
          termination: `Exited(${result.status})`, configured_limits: { timeout_seconds: null } });
        if (releaseTag === 'v0.0.9') {
          assert.equal(args.includes('--receipt-json'), false, 'legacy CLI has no receipt option');
          result.stderr = '';
        }
        if (failure === 'argv') result.stdout = 'TAPID_FIXTURE_STARTED=[]\n';
        if (failure === 'exit') result.status = 0;
        return result;
      } };
      return require(name);
    },
    process: { platform, argv: ['node', 'validator', '--binary', binary, '--release-tag', releaseTag],
      env: { TAPID_FIXTURE_PROJECT: '/fixture' }, stdout: { write() {} }, stderr: { write() {} } },
    console: { log() {} },
  };
  vm.runInNewContext(source, context);
  return calls;
}

for (const platform of ['linux', 'win32', 'darwin']) {
  test(`${platform}: preserves reviewed legacy forwarding without claiming containment`, () => {
    assert.equal(validate(platform, undefined, 'v0.0.9').length, 7);
  });
  test(`${platform}: validates the supplied published binary for all fixture cases`, () => {
    assert.equal(validate(platform).length, 7);
  });
}
for (const platform of ['linux', 'win32']) {
  for (const failure of ['success', 'unrelated', 'child', 'receipt']) {
    test(`${platform}: rejects ${failure} instead of fail-closed containment`, () => {
      assert.throws(() => validate(platform, failure));
    });
  }
}
for (const failure of ['argv', 'exit']) {
  test(`darwin: rejects incorrect ${failure}`, () => assert.throws(() => validate('darwin', failure)));
}
test('unknown published releases require an explicit reviewed contract', () => {
  assert.throws(() => validate('linux', undefined, 'v9.9.9'), /unreviewed root-script release/);
});
