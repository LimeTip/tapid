// Offline validator regression tests; simulated children are not release evidence.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');
const { loadConsumerContracts } = require('./consumer_contract.js');
const { current, currentTag, releases } = loadConsumerContracts();
const legacyTag = [...releases].find(([, contract]) => contract.legacy)[0];
const historicalNativeTag = [...releases].find(([tag, contract]) =>
  tag !== currentTag && !contract.legacy && !contract.nativePlatforms.includes('linux'))[0];
const source = fs.readFileSync(path.join(__dirname, 'validate_consumer_project.js'), 'utf8');

/**
 * Execute the capability contract against a controlled platform model.
 *
 * The validator deliberately uses the supplied binary so offline regressions
 * cannot accidentally validate a locally built replacement.
 */
function validate(platform, failure, releaseTag = historicalNativeTag) {
  const calls = [];
  const binary = path.resolve('published', 'tapid');
  const validatorArgs = releaseTag
    ? ['--binary', binary, '--release-tag', releaseTag]
    : ['--binary', binary];
  const contract = releaseTag ? releases.get(releaseTag) : current;
  const legacy = contract?.legacy;
  const nativeRestricted = contract?.nativePlatforms.includes(platform);
  const context = {
    require(name) {
      if (name === 'node:fs') return {
        existsSync: () => false,
        statSync: () => ({ isDirectory: () => true }),
        readFileSync: () => 'assurance = "restricted"\nwrite = []\nnetwork = false',
      };
      if (name === 'node:child_process') return { spawnSync(executable, args, options) {
        assert.equal(executable, binary, 'must execute the supplied published binary');
        assert.equal(options.timeout, 60_000, 'every invocation must remain bounded');
        assert.equal(options.maxBuffer, 4 * 1024 * 1024, 'diagnostics must remain bounded');
        assert.equal(options.env.TAPID_WINDOWS_STAGE_TRACE,
          platform === 'win32' && !releaseTag ? '1' : undefined,
          'stage trace is host-only and opt-in for native Windows source validation');
        assert.equal(options.shell, false, 'must execute the binary without a shell');
        calls.push(args);
        const result = { status: 0, signal: null, stdout: '', stderr: '' };
        if (args[0] === 'install') return result;
        if (!nativeRestricted && !legacy) {
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
          backend: { name: platform === 'win32' ? 'tapid-runner/windows-appcontainer-job' :
            platform === 'linux' ? 'tapid-runner/linux-landlock-seccomp-restricted' :
            'tapid-runner/macos-seatbelt-restricted-experimental' },
          enforced: { filesystem_read: true, filesystem_write: true, network: true, environment_sanitization: true },
          termination: `Exited(${result.status})`, configured_limits: { timeout_seconds: null } });
        if (legacy) {
          assert.equal(args.includes('--receipt-json'), false, 'legacy CLI has no receipt option');
          result.stderr = '';
        }
        if (failure === 'backend' || failure === 'assurance' || failure === 'enforcement') {
          const receipt = JSON.parse(result.stderr);
          if (failure === 'backend') receipt.backend.name = 'tapid-runner/macos-seatbelt-restricted-experimental';
          if (failure === 'assurance') receipt.assurance = 'Uncontained';
          if (failure === 'enforcement') receipt.enforced.filesystem_write = false;
          result.stderr = JSON.stringify(receipt);
        }
        if (failure === 'missing-receipt') result.stderr = '';
        if (failure === 'unsupported') {
          result.status = 1;
          result.stdout = '';
          result.stderr = 'unsupported-containment: no process was started and no enforcement receipt was issued';
        }
        if (failure === 'argv') result.stdout = 'TAPID_FIXTURE_STARTED=[]\n';
        if (failure === 'exit') result.status = 0;
        return result;
      } };
      return require(name);
    },
    Buffer,
    process: { hrtime: process.hrtime, platform, argv: ['node', 'validator', ...validatorArgs],
      env: { TAPID_FIXTURE_PROJECT: '/fixture' }, stdout: { write() {} }, stderr: { write() {} } },
    console: { log() {} },
  };
  vm.runInNewContext(source, context);
  return calls;
}

for (const platform of ['linux', 'win32', 'darwin']) {
  test(`${platform}: preserves reviewed legacy forwarding without claiming containment`, () => {
    assert.equal(validate(platform, undefined, legacyTag).length, 9);
  });
  test(`${platform}: validates the supplied published binary for all fixture cases`, () => {
    assert.equal(validate(platform).length, 9);
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
test('win32: source without a release tag requires native Restricted execution', () => {
  assert.equal(validate('win32', undefined, null).length, 9);
});

for (const failure of ['argv', 'exit', 'backend', 'assurance', 'enforcement', 'missing-receipt', 'unsupported']) {
  test(`win32: source rejects incorrect ${failure}`, () => {
    assert.throws(() => validate('win32', failure, null));
  });
}

test('unknown published releases require an explicit reviewed contract', () => {
  let unknownTag = 'v99.0.0';
  while (releases.has(unknownTag)) unknownTag += '0';
  assert.throws(() => validate('linux', undefined, unknownTag), /unreviewed root-script release/);
});
test('linux source validation exercises native Restricted execution', () => {
  assert.equal(validate('linux', undefined, null).length, 9);
});

for (const platform of ['linux', 'darwin', 'win32']) {
  test(`${platform}: validates the reviewed ${currentTag} containment contract`, () => {
    assert.equal(validate(platform, undefined, currentTag).length, 9);
  });
}
for (const platform of ['linux', 'darwin']) {
  for (const failure of ['argv', 'exit']) {
    test(`${platform}: ${currentTag} rejects incorrect ${failure}`, () => {
      assert.throws(() => validate(platform, failure, currentTag), error => {
        assert.doesNotMatch(error.message, /unreviewed root-script release/);
        return true;
      });
    });
  }
}
for (const failure of ['success', 'unrelated', 'child', 'receipt']) {
  test(`win32: ${currentTag} rejects ${failure} instead of fail-closed containment`, () => {
    assert.throws(() => validate('win32', failure, currentTag), error => {
      assert.doesNotMatch(error.message, /unreviewed root-script release/);
      return true;
    });
  });
}

test('source Windows support never changes reviewed published contracts', () => {
  assert.ok(current.nativePlatforms.includes('win32'));
  for (const [tag, contract] of releases) {
    assert.equal(contract.nativePlatforms.includes('win32'), false, `${tag} must retain its reviewed published contract`);
  }
});

for (const failure of ['backend', 'assurance', 'enforcement', 'missing-receipt', 'unsupported']) {
  test(`linux: source rejects incorrect ${failure}`, () => {
    assert.throws(() => validate('linux', failure, null));
  });
}
