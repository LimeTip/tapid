import { strictEqual } from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { test } from 'node:test';

const run = promisify(execFile);
test('previous-release installer selection preserves the legacy version boundary', async () => {
  const workflow = await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url), 'utf8');
  const block = workflow.split(`if node - "$previous_tag" <<'JAVASCRIPT'\n`)[1].split('          JAVASCRIPT')[0];
  for (const [tag, expected] of [['v0.0.9', 0], ['v0.0.10', 0], ['v0.0.11', 1], ['v0.1.0', 1], ['v1.0.0', 1], ['v9007199254740993.0.0', 1]] as const) {
    const result = spawnSync(process.execPath, ['-', tag], { input: block, encoding: 'utf8' });
    strictEqual(result.status, expected, `${tag}: ${result.stderr}`);
  }
});
for (const [selected, latest] of [['v0.0.10', 'v0.0.10'], ['v0.0.10', 'v0.0.11'], ['v0.0.11', 'v0.0.11'], ['v0.0.11', 'v1.2.0']]) {
  test(`public smoke selects the correct installer contracts for ${selected} and latest ${latest}`, async () => {
    const directory = await mkdtemp(join(tmpdir(), 'tapid-smoke-installer-'));
    try {
      const workflow = await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url), 'utf8');
      const block = workflow.split("<<'JAVASCRIPT' >> \"$GITHUB_OUTPUT\"\n")[1].split('          JAVASCRIPT')[0]
        .split('\n').map(line => line.startsWith('          ') ? line.slice(10) : line).join('\n');
      await writeFile(join(directory, 'releases.json'), JSON.stringify([
        { tagName: 'v0.0.9', isDraft: false, isPrerelease: false },
        { tagName: 'v0.0.10', isDraft: false, isPrerelease: false },
        { tagName: 'v0.0.11', isDraft: false, isPrerelease: false },
        { tagName: 'v99.0.0', isDraft: false, isPrerelease: false },
        { tagName: 'v0.9.0', isDraft: true, isPrerelease: false },
        { tagName: 'v0.8.0', isDraft: false, isPrerelease: true },
        { tagName: 'invalid', isDraft: false, isPrerelease: false },
      ]));
      const { stdout } = await run(process.execPath, ['-e', block], { env: {
        ...process.env, RUNNER_TEMP: directory, SELECTED_TAG: selected, LATEST_TAG: latest,
        SELECTED_SHA: 'a'.repeat(40), LATEST_SHA: 'b'.repeat(40),
      } });
      const values = Object.fromEntries(stdout.trim().split('\n').map(line => line.split('=')));
      strictEqual(values.previous_tag, latest === 'v0.0.10' ? '' : latest === 'v0.0.11' ? 'v0.0.10' : 'v0.0.11');
      strictEqual(values.record_aware, String(latest !== 'v0.0.10'));
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
