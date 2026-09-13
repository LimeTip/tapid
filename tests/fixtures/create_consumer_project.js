const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

const projectDirectory = fs.mkdtempSync(
  path.join(os.tmpdir(), 'tapid-consumer-'),
);
// Tapid appends shell-quoted positional arguments itself; do not also add $1/$2.
const testScript = 'node fixture.js';

const packageJson = JSON.stringify(
  {
    name: 'dynamic-platform-fixture',
    version: '1.0.0',
    private: true,
    scripts: {
      preinstall: "node -e \"require('fs').writeFileSync('LIFECYCLE_SHOULD_NOT_RUN', 'x')\"",
      test: testScript,
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
    lockfileVersion: 4,
    rootManifestDigest: `sha256-${manifestDigest}`,
    resolverVersion: '0',
    linkerVersion: '0',
    packages: {},
  },
  null,
  2,
) + '\n';

const fixtureScript = [
  // A stdout marker proves Node ran without granting any filesystem writes.
  "console.log('TAPID_FIXTURE_STARTED=' + JSON.stringify(process.argv.slice(2)));",
  "if (process.argv.length !== 4) process.exit(44);",
  "if (process.argv[2] !== 'forwarded') process.exit(41);",
  "if (process.env.TAPID_FIXTURE !== '1') process.exit(42);",
  "if (require('fs').existsSync('LIFECYCLE_SHOULD_NOT_RUN')) process.exit(43);",
  'process.exit(Number(process.argv[3] || 0));',
  '',
].join('\n');

const runPolicy = [
  '[run.defaults]',
  'read = ["."]',
  'write = []',
  'network = false',
  'environment = []',
  'subprocess = true',
  // AssuranceLevel uses serde kebab-case, not the Rust enum spelling.
  'assurance = "restricted"',
  '',
  '[run.scripts.test]',
  'environment = ["TAPID_FIXTURE"]',
  '',
].join('\n');

fs.writeFileSync(path.join(projectDirectory, 'package.json'), packageJson);
fs.writeFileSync(path.join(projectDirectory, 'tapid.lock'), lockfile);
fs.writeFileSync(path.join(projectDirectory, 'tapid.toml'), runPolicy);
fs.writeFileSync(path.join(projectDirectory, 'fixture.js'), fixtureScript);
fs.writeFileSync(
  path.join(projectDirectory, 'fixture-contract.json'),
  JSON.stringify({
    project: projectDirectory,
    checks: [
      'install',
      'root-script',
      'checked-in-run-policy',
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
