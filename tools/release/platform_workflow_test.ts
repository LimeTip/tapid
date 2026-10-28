import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { PLATFORMS } from './platforms.ts';

// Deliberately accept only the checked-in block-matrix grammar. Unknown structure
// must require review rather than disappear from a presence-only assertion.
function rows(source: string, job: string, targetKey: 'target' | 'archive') {
  const jobs = source.split(new RegExp(`^  ${job}:\\n`, 'm'));
  assert.equal(jobs.length, 2, `expected one ${job} job`);
  const body = jobs[1].split(/^  [\w-]+:\n/m)[0];
  assert.equal((body.match(/^      matrix:\n/gm) ?? []).length, 1, 'expected one matrix');
  const matrix = body.split('      matrix:\n')[1].split(/^(?=\S| {1,6}\S)/m)[0];
  const lines = matrix.trimEnd().split('\n');
  assert.equal(lines.shift(), '        include:', 'unsupported matrix structure');
  const result: Record<string,string>[] = [];
  for (const line of lines) {
    const match = /^(          - |            )(runner|target|archive|binary): ([A-Za-z0-9_.-]+)$/.exec(line);
    assert(match, `unsupported matrix row: ${line}`);
    if (match[1] === '          - ') result.push({});
    const row = result.at(-1);
    assert(row && !(match[2] in row), `duplicate or misplaced matrix key: ${line}`);
    row[match[2]] = match[3];
  }
  return result.map(row => {
    assert.deepEqual(Object.keys(row).sort(), ['binary', 'runner', targetKey].sort(), 'unexpected matrix fields');
    return {target:row[targetKey], runner:row.runner, binary:row.binary};
  });
}
function validate(source: string, job: string, key: 'target' | 'archive') {
  const order = (a: {target:string}, b: {target:string}) => a.target.localeCompare(b.target);
  assert.deepEqual(rows(source, job, key).sort(order), [...PLATFORMS].sort(order), `${job} platform matrix mismatch`);
}
for (const [file, job, key] of [
  ['release-publication.yml','build','target'],
  ['release-draft-verify.yml','install','archive'],
] as const) {
  test(`${file} matrix agrees exactly with release platforms and rejects drift`, async () => {
    const source = await readFile(new URL(`../../.github/workflows/${file}`, import.meta.url), 'utf8');
    validate(source, job, key);
    const row = /          - runner: [^\n]+\n            (?:target|archive): [^\n]+\n            binary: [^\n]+\n/.exec(source)![0];
    for (const altered of [
      source.replace(row,''), source.replace(row,row+row),
      source.replace(row,row.replace(/(?:target|archive): [^\n]+/, `${key}: unknown-target`)),
      source.replace(row,row.replace(/runner: [^\n]+/, 'runner: wrong-runner')),
      source.replace(row,row.replace(/binary: [^\n]+/, 'binary: wrong-binary')),
      source.replace(row,row+'          - runner: extra\n            '+key+': extra\n            binary: tapid\n'),
      source.replace(row,row+'            binary: tapid\n'),
      source.replace(row,row.replace(/            binary: [^\n]+\n/, '')),
      source.replace(row,row+'            unexpected: value\n'),
      source.replace('        include:', '        include: []'),
    ]) assert.throws(() => validate(altered,job,key));
  });
}
