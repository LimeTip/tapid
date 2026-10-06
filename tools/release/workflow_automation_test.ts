import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFile } from 'node:fs/promises';
const workflow = (name: string) => readFile(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
test('every release trigger uses the same candidate approval and same-run smoke before OIDC', async () => {
  const coordinator = await workflow('crates-publication');
  assert.match(coordinator, /push:\n    branches: \[main\]/);
  assert.match(coordinator, /docs\/releases\/intent.json/);
  assert.doesNotMatch(coordinator, /workflow_run:/);
  assert.match(coordinator, /automation.ts wait-ci/);
  assert.match(coordinator, /automation.ts check-plan/);
  assert.match(coordinator, /uses: .\/\.github\/workflows\/release-publication.yml/);
  assert.match(coordinator, /needs: \[preflight, candidate, verify, promote, smoke\]/);
  assert.match(coordinator, /environment: crates-io-release/);
  const publish = coordinator.slice(coordinator.indexOf('\n  publish:'));
  assert.match(publish, /candidate.ts publish/);
  assert(publish.indexOf('candidate.ts publish') < publish.indexOf('crates-io-auth-action@'));
  assert.doesNotMatch(coordinator, /event=release&status=success/);
  assert.match(coordinator, /secrets: inherit/);
});
test('approval consumes the immutable unsigned artifact after package preflight', async () => {
  const build = await workflow('release-publication');
  assert.doesNotMatch(build, /push:|workflow_dispatch:/);
  assert.match(build, /workflow_call:/);
  const assemble = build.slice(build.indexOf('\n  assemble:'), build.indexOf('\n  draft-release:'));
  assert.match(assemble, /candidate.ts unsigned/);
  assert.match(assemble, /bootstrap.ts release/);
  assert.doesNotMatch(assemble, /secrets\.|environment:/);
  const gate = build.slice(build.indexOf('\n  draft-release:'));
  assert.match(gate, /environment: stable-release/);
  assert.match(gate, /artifact-ids: \$\{\{ needs.assemble.outputs.artifact_id \}\}/);
  assert(gate.indexOf('validate-unsigned') < gate.indexOf('sign.ts sign'));
  assert(gate.indexOf('sign.ts verify') < gate.indexOf('candidate.ts create-draft'));
  assert.match(gate, /candidate_sha256/);
  assert.doesNotMatch(gate, /cargo build|cargo package|cargo test/);
});
test('native and public smoke verification can be called with exact release identity', async () => {
  for (const name of ['release-draft-verify', 'release-public-smoke']) {
    const yaml = await workflow(name);
    assert.match(yaml, /workflow_call:/);
    assert.match(yaml, /release_id:/);
    assert.match(yaml, /commit_sha:/);
  }
});
