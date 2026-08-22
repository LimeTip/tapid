const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

const projectDirectory = fs.mkdtempSync(
  path.join(os.tmpdir(), 'tapid-consumer-'),
);
const positional = String.fromCharCode(36);
const testScript = process.platform === 'win32'
  ? 'node fixture.js'
  : `node fixture.js ${positional}1 ${positional}2`;

const packageJson = JSON.stringify(
  {
    name: 'dynamic-platform-fixture',
    version: '1.0.0',
    private: true,
    scripts: {
      preinstall: "node -e \"require('fs').writeFileSync('LIFECYCLE_SHOULD_NOT_RUN', 'x')\"",
      test: testScript,
    },
    bin: {
      fixture: 'fixture.js',
    },
  },
  null,
  2,
) + '\n';

const manifestDigest = crypto
  .createHash('sha256')
  .update(packageJson)
  .digest('hex');
const lockfile = JSON.stringify(
  {
    lockfileVersion: 3,
    rootManifestDigest: `sha256-${manifestDigest}`,
    resolverVersion: '0',
    linkerVersion: '0',
    packages: {},
  },
  null,
  2,
) + '\n';

const fixtureScript = [
  "if (process.argv[2] !== 'forwarded') process.exit(41);",
  "if (process.env.TAPID_FIXTURE !== '1') process.exit(42);",
  "if (require('fs').existsSync('LIFECYCLE_SHOULD_NOT_RUN')) process.exit(43);",
  'process.exit(Number(process.argv[3] || 0));',
  '',
].join('\n');

fs.writeFileSync(path.join(projectDirectory, 'package.json'), packageJson);
fs.writeFileSync(path.join(projectDirectory, 'tapid.lock'), lockfile);
fs.writeFileSync(path.join(projectDirectory, 'fixture.js'), fixtureScript);
fs.writeFileSync(
  path.join(projectDirectory, 'fixture-contract.json'),
  JSON.stringify({
    project: projectDirectory,
    checks: [
      'install',
      'root-script',
      'bin-shim',
      'argument-forwarding',
      'exit-code',
      'lifecycle-suppression',
    ],
  }, null, 2) + '\n',
);

if (!process.env.GITHUB_ENV) {
  throw new Error('GITHUB_ENV is required for CI fixture creation');
}
fs.appendFileSync(
  process.env.GITHUB_ENV,
  `TAPID_FIXTURE_PROJECT=${projectDirectory}${os.EOL}`,
);
console.log(`Fixture project: ${projectDirectory}`);
