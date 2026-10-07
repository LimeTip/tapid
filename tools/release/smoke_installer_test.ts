import { strictEqual } from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { test } from 'node:test';

const run = promisify(execFile);
for (const [selected, latest] of [['v0.0.10', 'v0.0.10'], ['v0.0.10', 'v0.0.11'], ['v0.0.11', 'v0.0.11'], ['v0.0.11', 'v1.2.0']]) {
  test(`public smoke selects the correct installer contracts for ${selected} and latest ${latest}`, async () => {
    const directory = await mkdtemp(join(tmpdir(), 'tapid-smoke-installer-'));
    try {
      const workflow = await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url), 'utf8');
      const block = workflow.split("<<'PYTHON' >> \"$GITHUB_OUTPUT\"\n")[1].split('          PYTHON')[0]
        .split('\n').map(line => line.startsWith('          ') ? line.slice(10) : line).join('\n');
      await writeFile(join(directory, 'releases.json'), '[]');
      const { stdout } = await run('python3', ['-c', block], { env: {
        ...process.env, RUNNER_TEMP: directory, SELECTED_TAG: selected, LATEST_TAG: latest,
        SELECTED_SHA: 'a'.repeat(40), LATEST_SHA: 'b'.repeat(40),
      } });
      const values = Object.fromEntries(stdout.trim().split('\n').map(line => line.split('=')));
      for (const [kind, tag, sha] of [['selected', selected, 'a'.repeat(40)], ['latest', latest, 'b'.repeat(40)]]) {
        for (const extension of ['sh', 'ps1']) {
          const expected = tag === 'v0.0.10'
            ? `https://raw.githubusercontent.com/LimeTip/tapid/${sha}/scripts/install.${extension}`
            : `https://github.com/LimeTip/tapid/releases/download/${tag}/install.${extension}`;
          strictEqual(values[`${kind}_installer_${extension}`], expected);
        }
      }
    } finally { await rm(directory, { recursive: true, force: true }); }
  });
}
