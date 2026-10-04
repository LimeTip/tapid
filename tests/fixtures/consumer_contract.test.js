const assert = require('node:assert/strict');
const { test } = require('node:test');
const { reviewedContracts } = require('./consumer_contract.js');

const contract = { legacy: false, nativePlatforms: ['darwin', 'linux'] };
const historical = { legacy: true, nativePlatforms: [] };
const metadata = { current: contract, historical: { 'v1.0.0': historical } };

test('current contract follows the product version from Cargo metadata', () => {
  const manifest = '[package]\nname = "tapid"\nversion = "2.3.4"\n[dependencies]\nexample = { version = "9.9.9" }\n';
  const contracts = reviewedContracts(manifest, metadata);
  assert.deepEqual(contracts.get('v2.3.4'), contract);
  assert.deepEqual(contracts.get('v1.0.0'), historical);
  assert.equal(contracts.has('v2.3.5'), false);
  assert.deepEqual(reviewedContracts(manifest.replace('2.3.4', '3.0.0'), metadata).get('v3.0.0'), contract);
});

test('unreviewed versions have no contract', () => {
  const contracts = reviewedContracts('[package]\nversion = "2.3.4"\n', metadata);
  assert.equal(contracts.has('v99.0.0'), false);
});

test('conflicting historical and current contracts fail closed', () => {
  assert.throws(() => reviewedContracts('[package]\nversion = "1.0.0"\n', metadata), /historical/);
});

for (const manifest of ['[package]\nversion = "2.3.4-rc.1"\n', '[dependencies]\nversion = "2.3.4"\n', '[package]\nversion = "02.3.4"\n']) {
  test('invalid or absent product versions fail closed', () => {
    assert.throws(() => reviewedContracts(manifest, metadata), /product version/);
  });
}

for (const invalid of [
  { current: { legacy: false, nativePlatforms: ['unknown'] }, historical: {} },
  { current: { legacy: 'false', nativePlatforms: [] }, historical: {} },
  { current: contract, historical: { latest: historical } },
  { current: { legacy: true, nativePlatforms: ['linux'] }, historical: {} },
]) {
  test('malformed capability metadata fails closed', () => {
    assert.throws(() => reviewedContracts('[package]\nversion = "2.3.4"\n', invalid), /contract/);
  });
}
