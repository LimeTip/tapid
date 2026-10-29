import assert from 'node:assert/strict';
import { test } from 'node:test';
import { stableVersionParts, compareStableVersions } from './versions.ts';

test('stable numeric versions compare every component without Number precision loss', () => {
  for (const [left,right] of [
    ['2.0.0','1.99.99'], ['1.2.0','1.1.99'], ['1.1.2','1.1.1'],
    ['9007199254740993.0.0','9007199254740992.99.99'],
    ['1.9007199254740993.0','1.9007199254740992.99'],
    ['1.1.9007199254740993','1.1.9007199254740992'],
  ]) {
    assert.equal(compareStableVersions(left,right),1);
    assert.equal(compareStableVersions(right,left),-1);
    assert.equal(compareStableVersions(left,left),0);
  }
  assert.deepEqual(stableVersionParts('0.1.9007199254740993'), [0n,1n,9007199254740993n]);
  assert.equal(compareStableVersions('0.0.0','0.0.0'),0);
});
test('unsupported stable syntax is indeterminate on either side', () => {
  for (const invalid of ['', 'v1.2.3','01.2.3','1.02.3','1.2.03','1.2','1.2.3.4',
    ' 1.2.3','1.2.3 ', '1.2.3\n', '1.2.3-alpha','1.2.3+build','-1.2.3','1.x.3','1..3']) {
    assert.equal(stableVersionParts(invalid),undefined, invalid);
    assert.equal(compareStableVersions(invalid,'1.2.3'),undefined, invalid);
    assert.equal(compareStableVersions('1.2.3',invalid),undefined, invalid);
  }
});
