const assert = require('node:assert/strict');
assert.equal(require('@example/ui'), 'local-ui');
assert.equal(require('external'), 'registry-dependency');
assert.equal(require.resolve('@example/ui'), require('node:path').resolve('../../packages/ui/index.js'));
console.log('NEWS_WORKSPACE_OK');
