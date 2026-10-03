import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import semver from 'semver';
import semverPackage from 'semver/package.json' with { type: 'json' };

assert.equal(semverPackage.version, '7.8.5', 'unexpected semver oracle version');
const cases = JSON.parse(await readFile(new URL('./cases.json', import.meta.url), 'utf8'));
assert.ok(Array.isArray(cases) && cases.length > 0, 'corpus must contain cases');
for (const [index, item] of cases.entries()) {
  assert.equal(typeof item.range, 'string', `case ${index}: range must be a string`);
  assert.equal(typeof item.version, 'string', `case ${index}: version must be a string`);
  assert.equal(typeof item.satisfies, 'boolean', `case ${index}: expected result must be boolean`);
  if (Object.hasOwn(item, 'validRange')) {
    assert.equal(typeof item.validRange, 'boolean', `case ${index}: validRange must be boolean`);
    assert.equal(
      semver.validRange(item.range) !== null,
      item.validRange,
      `case ${index}: validity of ${JSON.stringify(item.range)}`
    );
  }
  let actual;
  try {
    actual = semver.satisfies(item.version, item.range);
  } catch (error) {
    assert.equal(item.satisfies, false, `case ${index}: unexpected exception: ${error.message}`);
    continue;
  }
  assert.equal(
    actual,
    item.satisfies,
    `case ${index}: ${JSON.stringify(item.version)} against ${JSON.stringify(item.range)}`
  );
}
console.log(`node-semver ${semverPackage.version}: ${cases.length} corpus cases passed`);
