const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const stableVersion = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;

function productVersion(manifest) {
  const packageSection = manifest.split(/^\[package\][ \t]*\r?$/m)[1]?.split(/^\[/m)[0];
  const version = packageSection?.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  assert.ok(version && stableVersion.test(version), 'invalid or absent product version');
  return version;
}

function validateContract(contract) {
  assert.ok(contract && typeof contract.legacy === 'boolean' &&
    Array.isArray(contract.nativePlatforms) &&
    contract.nativePlatforms.every(platform => ['darwin', 'linux', 'win32'].includes(platform)) &&
    (!contract.legacy || contract.nativePlatforms.length === 0), 'invalid consumer contract');
}

// Expected capabilities only. Unknown releases never inherit a contract by range.
function reviewedContracts(manifest, metadata) {
  const currentTag = `v${productVersion(manifest)}`;
  validateContract(metadata.current);
  assert.ok(metadata.historical && typeof metadata.historical === 'object', 'invalid historical contracts');
  const releases = new Map();
  for (const [tag, contract] of Object.entries(metadata.historical)) {
    assert.ok(tag.startsWith('v') && stableVersion.test(tag.slice(1)), 'invalid historical contract tag');
    assert.notEqual(tag, currentTag, 'current product version conflicts with historical contract');
    validateContract(contract);
    releases.set(tag, contract);
  }
  releases.set(currentTag, metadata.current);
  return releases;
}

function loadConsumerContracts() {
  const manifest = fs.readFileSync(path.join(__dirname, '../../crates/tapid-cli/Cargo.toml'), 'utf8');
  const metadata = JSON.parse(fs.readFileSync(path.join(__dirname, 'consumer_contracts.json'), 'utf8'));
  return { current: metadata.current, currentTag: `v${productVersion(manifest)}`,
    releases: reviewedContracts(manifest, metadata) };
}

module.exports = { reviewedContracts, loadConsumerContracts };
