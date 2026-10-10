const assert = require('node:assert/strict');
assert.equal(require('@example/ui'), 'local-ui');
assert.equal(require('external'), 'registry-dependency');
const fs = require('node:fs');
// File identity remains stable across Windows short and long path aliases.
const actual = fs.statSync(require.resolve('@example/ui'), { bigint: true });
const expected = fs.statSync('../../packages/ui/index.js', { bigint: true });
assert.equal(actual.dev, expected.dev);
assert.equal(actual.ino, expected.ino);
console.log('NEWS_WORKSPACE_OK');
