const assert = require('node:assert/strict');
assert.equal(require('@example/ui'), 'local-ui');
assert.equal(require('external'), 'registry-dependency');
const fs = require('node:fs');
assert.equal(fs.realpathSync(require.resolve('@example/ui')), fs.realpathSync('../../packages/ui/index.js'));
console.log('NEWS_WORKSPACE_OK');
