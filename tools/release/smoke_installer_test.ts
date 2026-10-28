import { rejects, strictEqual } from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { test } from 'node:test';

const run = promisify(execFile);
test('native latest consumers survive supplemental failures and retain only attempted evidence', async () => {
  const workflow = await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url), 'utf8');
  for (const job of ['unix', 'windows']) {
    const body = workflow.split(`\n  ${job}:`)[1].split(/\n  [a-z][a-z-]*:/)[0];
    const consumer = body.split('name: Run latest discovery consumer contract')[1].split('\n      - name:')[0];
    const upload = body.split('name: Retain latest consumer evidence')[1].split('\n      - name:')[0];
    const condition = (step: string) => step.match(/\n        if: (.+)/)![1].replace(/^\$\{\{\s*|\s*\}\}$/g, '');
    // Model Actions' implicit success() guard when no status function is present.
    const scheduled = (expression: string, priorSuccess: boolean, cancelled: boolean, install: string, outcome: string) => {
      const hasStatus = /\b(success|failure|always|cancelled)\(/.test(expression);
      const evaluate = new Function('cancelled', 'always', 'steps', `return (${expression});`);
      return (hasStatus || priorSuccess) && evaluate(() => cancelled, () => true, {
        install_latest: { outcome: install }, latest_consumer: { outcome },
      });
    };
    for (const priorSuccess of [true, false]) {
      for (const cancelled of [true, false]) {
        for (const install of ['success', 'failure', 'skipped']) {
          strictEqual(scheduled(condition(consumer), priorSuccess, cancelled, install, 'skipped'), !cancelled && install === 'success', `${job}: consumer scheduling`);
        }
        for (const outcome of ['success', 'failure', 'skipped']) {
          strictEqual(scheduled(condition(upload), priorSuccess, cancelled, 'success', outcome), outcome !== 'skipped', `${job}: evidence retention`);
        }
      }
    }
    strictEqual(upload.includes('if-no-files-found: error'), true, `${job}: attempted evidence is required`);
  }
});
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

test('native discovery binds latest source identity and isolates Windows before installation', async () => {
  const workflow = await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url), 'utf8');
  strictEqual(workflow.includes('latest_sha: ${{ steps.release.outputs.latest_sha }}'), true);
  const windows = workflow.split('\n  windows:')[1];
  strictEqual(windows.indexOf('name: Isolate installer home') < windows.indexOf('name: Install the published release'), true);
  for (const segment of workflow.split('name: Install latest release through discovery').slice(1)) {
    const step = segment.split('\n      - name:')[0];
    strictEqual(step.includes('RELEASE_SHA: ${{ needs.resolve.outputs.latest_sha }}'), true);
    strictEqual(step.includes('--version "$RELEASE_TAG"'), false);
    strictEqual(step.includes('-Version $env:RELEASE_TAG'), false);
    if (step.includes('shell: bash')) {
      strictEqual(step.includes('actual="$("$install_dir/tapid" --version)"'), true);
      strictEqual(step.includes('test "$actual" = "tapid ${RELEASE_TAG#v}"'), true);
    }
  }
  strictEqual((workflow.match(/name: Run latest discovery consumer contract/g) || []).length, 2);
});

test('Unix discovery rejects a failing version process even when its output matches', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'tapid-smoke-version-'));
  try {
    const workflow = await readFile(new URL('../../.github/workflows/release-public-smoke.yml', import.meta.url), 'utf8');
    const step = workflow.split('name: Install latest release through discovery')[1].split('\n      - name:')[0];
    const script = step.split('        run: |\n')[1];
    await writeFile(join(directory, 'public-install.sh'), `#!/bin/sh
mkdir -p "$2"
cat > "$2/tapid" <<'BINARY'
#!/bin/sh
printf 'tapid 1.2.3\\n'
exit 37
BINARY
chmod +x "$2/tapid"
`);
    await rejects(run('bash', ['-c', script], { env: { HOME: directory, RUNNER_TEMP: directory, RELEASE_TAG: 'v1.2.3', PATH: process.env.PATH } }), { code: 37 });
  } finally { await rm(directory, { recursive: true, force: true }); }
});
